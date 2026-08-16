//! Production [`ObjectStore`] backed by `aws-sdk-s3` (ADR 003).
//!
//! This is the third implementation of the [`ObjectStore`] seam, alongside
//! [`super::fs::FsStore`] and [`super::stub::StubStore`]. It is compiled only
//! under the `s3` feature so default builds stay fast, hermetic, and
//! credential-free (ADR 001).
//!
//! ## Sync-over-async bridge
//!
//! `ObjectStore` is synchronous by design, but `aws-sdk-s3` is async. Rather
//! than make the whole core `async` (see ADR 003 "Considered alternatives"),
//! `S3Store` owns a dedicated tokio runtime and bridges each call by *spawning*
//! the future onto that runtime and blocking the calling thread on a
//! `std::sync::mpsc` receive. Crucially it never calls `Runtime::block_on` on
//! the ambient thread — doing so panics when a client (`trove-serverd`) is
//! already inside a tokio runtime. Blocking on a channel from a separate
//! runtime does not, because the caller only parks on a synchronous receive.

use std::future::Future;

use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;

use super::{ObjectMeta, ObjectStore, PutOutcome};
use crate::error::{Error, Result};

/// Objects at or below this size use a single `PutObject`; larger buffers are
/// uploaded as a multipart upload in [`PART_SIZE`] chunks.
const MULTIPART_THRESHOLD: usize = 8 * 1024 * 1024;
/// Multipart part size (S3 requires >= 5 MiB for all but the final part).
const PART_SIZE: usize = 8 * 1024 * 1024;

/// An [`ObjectStore`] backed by an S3 (or S3-compatible) bucket.
pub struct S3Store {
    client: Client,
    bucket: String,
    runtime: tokio::runtime::Runtime,
}

impl S3Store {
    /// Connect to `bucket` in `region`, optionally against a custom `endpoint`
    /// (for S3-compatible stores like MinIO or Cloudflare R2). Credentials are
    /// resolved from the default AWS provider chain (env, profile, IMDS, …).
    ///
    /// When a custom `endpoint` is given, this also verifies the endpoint
    /// actually honors conditional writes before returning (ADR 007, Group
    /// B1): AWS S3 itself only gained native `If-Match`/`If-None-Match`
    /// support on `PutObject` in 2024, and S3-compatible services vary by
    /// product and version. If the endpoint silently ignores the conditional
    /// header instead of honoring or rejecting it, `put_if_match` would look
    /// like it works while providing no protection at all — the worst
    /// possible failure mode. Real AWS is trusted without a probe.
    pub fn new(
        bucket: impl Into<String>,
        region: impl Into<String>,
        endpoint: Option<String>,
    ) -> Result<Self> {
        let bucket = bucket.into();
        let region = region.into();
        let has_custom_endpoint = endpoint.is_some();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| Error::store(format!("building S3 runtime: {e}")))?;
        // Client construction is async (region/credential loading); run it on
        // the owned runtime via the same spawn-and-block bridge so this is safe
        // even when called from inside a client's tokio runtime.
        let client = block_on_runtime(&runtime, build_client(region, endpoint));
        let store = S3Store {
            client,
            bucket,
            runtime,
        };
        if has_custom_endpoint {
            store.verify_conditional_write_support()?;
        }
        Ok(store)
    }

    /// Probe: create-only write a throwaway object, then attempt the same
    /// create-only write again. A conditional-write-honoring endpoint must
    /// reject the second attempt (the object now exists); an endpoint that
    /// silently ignores `If-None-Match` will let it through. Fails loud
    /// rather than letting CAS silently provide no protection — consistent
    /// with this project's convention of surfacing gaps explicitly rather
    /// than faking success (`AGENTS.md`).
    ///
    /// This has not been verified against a live MinIO/R2/etc. deployment;
    /// the logic is correct against documented S3 conditional-write
    /// semantics, but real-world endpoint behavior should be confirmed
    /// before relying on this in production (see ADR 007's validation plan).
    fn verify_conditional_write_support(&self) -> Result<()> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let probe_key = format!(".trove-cas-probe/{}-{nonce}", std::process::id());
        let first = self.put_if_match(&probe_key, None, b"trove-cas-probe")?;
        if matches!(first, PutOutcome::Conflict { .. }) {
            return Err(Error::store(
                "conditional-write capability probe: unexpected conflict on first (create-only) write",
            ));
        }
        let second = self.put_if_match(&probe_key, None, b"trove-cas-probe-2");
        self.best_effort_delete(&probe_key);
        match second? {
            PutOutcome::Conflict { .. } => Ok(()),
            PutOutcome::Written(_) => Err(Error::store(
                "configured endpoint does not appear to honor conditional writes \
                 (a second create-only write to the same key was not rejected) — \
                 refusing to rely on compare-and-swap against this endpoint; see \
                 ADR 007, Group B1",
            )),
        }
    }

    /// Best-effort cleanup for the capability probe; failures are ignored
    /// since this is diagnostic housekeeping, not part of the store's
    /// correctness contract.
    fn best_effort_delete(&self, key: &str) {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = key.to_string();
        let _: std::result::Result<(), ()> = self.block_on(async move {
            let _ = client.delete_object().bucket(&bucket).key(&key).send().await;
            Ok(())
        });
    }

    /// Bridge an async operation to the synchronous trait: spawn onto the owned
    /// runtime and block the calling thread on a channel receive.
    fn block_on<F, T>(&self, fut: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        block_on_runtime(&self.runtime, fut)
    }
}

impl ObjectStore for S3Store {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = key.to_string();
        self.block_on(async move {
            match client.get_object().bucket(&bucket).key(&key).send().await {
                Ok(resp) => {
                    let data = resp
                        .body
                        .collect()
                        .await
                        .map_err(|e| Error::store(format!("reading body for {key}: {e}")))?;
                    Ok(data.into_bytes().to_vec())
                }
                Err(err) => {
                    if let Some(service) = err.as_service_error() {
                        if service.is_no_such_key() {
                            return Err(Error::not_found(format!("object {key}")));
                        }
                    }
                    Err(Error::store(format!("get {key}: {err}")))
                }
            }
        })
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<ObjectMeta> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = key.to_string();
        let data = bytes.to_vec();
        let size_bytes = data.len() as u64;
        self.block_on(async move {
            let etag = if data.len() > MULTIPART_THRESHOLD {
                put_multipart(&client, &bucket, &key, data).await?
            } else {
                let resp = client
                    .put_object()
                    .bucket(&bucket)
                    .key(&key)
                    .body(ByteStream::from(data))
                    .send()
                    .await
                    .map_err(|e| Error::store(format!("put {key}: {e}")))?;
                resp.e_tag().map(strip_quotes)
            };
            Ok(ObjectMeta {
                key,
                size_bytes,
                etag,
            })
        })
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.head(key)?.is_some())
    }

    fn head(&self, key: &str) -> Result<Option<ObjectMeta>> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = key.to_string();
        self.block_on(async move {
            match client.head_object().bucket(&bucket).key(&key).send().await {
                Ok(resp) => Ok(Some(ObjectMeta {
                    key,
                    size_bytes: resp.content_length().unwrap_or(0).max(0) as u64,
                    etag: resp.e_tag().map(strip_quotes),
                })),
                Err(err) => {
                    if let Some(service) = err.as_service_error() {
                        if service.is_not_found() {
                            return Ok(None);
                        }
                    }
                    Err(Error::store(format!("head {key}: {err}")))
                }
            }
        })
    }

    fn copy(&self, from_key: &str, to_key: &str) -> Result<ObjectMeta> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let from = from_key.to_string();
        let to = to_key.to_string();
        self.block_on(async move {
            // `copy_source` is `{bucket}/{key}` with the key percent-encoded, so
            // keys containing spaces/brackets (common in real archives) work.
            let source = format!("{}/{}", bucket, encode_key(&from));
            client
                .copy_object()
                .bucket(&bucket)
                .key(&to)
                .copy_source(source)
                .send()
                .await
                .map_err(|e| Error::store(format!("copy {from} -> {to}: {e}")))?;
            let head = client
                .head_object()
                .bucket(&bucket)
                .key(&to)
                .send()
                .await
                .map_err(|e| Error::store(format!("head after copy {to}: {e}")))?;
            Ok(ObjectMeta {
                key: to,
                size_bytes: head.content_length().unwrap_or(0).max(0) as u64,
                etag: head.e_tag().map(strip_quotes),
            })
        })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let prefix = prefix.to_string();
        self.block_on(async move {
            let mut keys = Vec::new();
            let mut continuation: Option<String> = None;
            loop {
                let mut req = client.list_objects_v2().bucket(&bucket).prefix(&prefix);
                if let Some(token) = &continuation {
                    req = req.continuation_token(token);
                }
                let resp = req
                    .send()
                    .await
                    .map_err(|e| Error::store(format!("list {prefix}: {e}")))?;
                for object in resp.contents() {
                    if let Some(k) = object.key() {
                        keys.push(k.to_string());
                    }
                }
                if resp.is_truncated().unwrap_or(false) {
                    continuation = resp.next_continuation_token().map(|s| s.to_string());
                    if continuation.is_none() {
                        break;
                    }
                } else {
                    break;
                }
            }
            keys.sort();
            Ok(keys)
        })
    }

    /// Uses S3's native conditional-write headers (`If-Match` for
    /// update-only, `If-None-Match: *` for create-only) — genuinely atomic
    /// server-side, no client-side locking. Only supports the single-`PutObject`
    /// path: `put_if_match` is scoped to the small `schema-version.json`
    /// marker and generation-keyed index payloads (ADR 007, Group B1), never
    /// to audio objects, so multipart conditional writes are deliberately not
    /// implemented — an oversized payload fails loudly rather than silently
    /// dropping the conditional check.
    fn put_if_match(
        &self,
        key: &str,
        expected_etag: Option<&str>,
        bytes: &[u8],
    ) -> Result<PutOutcome> {
        if bytes.len() > MULTIPART_THRESHOLD {
            return Err(Error::store(format!(
                "put_if_match {key}: conditional multipart writes are not supported ({} bytes > {MULTIPART_THRESHOLD})",
                bytes.len()
            )));
        }
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key_owned = key.to_string();
        let data = bytes.to_vec();
        let size_bytes = data.len() as u64;
        let expected = expected_etag.map(|s| s.to_string());
        self.block_on(async move {
            let mut req = client
                .put_object()
                .bucket(&bucket)
                .key(&key_owned)
                .body(ByteStream::from(data));
            req = match &expected {
                Some(etag) => req.if_match(etag),
                None => req.if_none_match("*"),
            };
            match req.send().await {
                Ok(resp) => Ok(PutOutcome::Written(ObjectMeta {
                    key: key_owned,
                    size_bytes,
                    etag: resp.e_tag().map(strip_quotes),
                })),
                Err(err) => {
                    if let Some(service) = err.as_service_error() {
                        // S3 returns 412 PreconditionFailed for a failed
                        // If-Match/If-None-Match; the SDK has no typed
                        // variant for it yet (aws-sdk-s3 1.137), so it
                        // surfaces via the generic error code string.
                        if service.code() == Some("PreconditionFailed") {
                            return Ok(PutOutcome::Conflict { current_etag: None });
                        }
                    }
                    Err(Error::store(format!("put_if_match {key_owned}: {err}")))
                }
            }
        })
    }
}

/// Spawn `fut` onto `runtime` and block the current thread until it resolves.
///
/// This is the whole bridge: because we spawn onto a *separate* runtime and
/// then park on a synchronous channel, it is safe to call from inside another
/// tokio runtime (unlike `Runtime::block_on`).
fn block_on_runtime<F, T>(runtime: &tokio::runtime::Runtime, fut: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    runtime.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv()
        .expect("s3 runtime worker dropped the task before responding")
}

/// Build an S3 client for `region`, honoring an optional custom endpoint.
async fn build_client(region: String, endpoint: Option<String>) -> Client {
    let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(Region::new(region));
    if let Some(url) = endpoint.clone() {
        loader = loader.endpoint_url(url);
    }
    let shared = loader.load().await;
    let mut builder = aws_sdk_s3::config::Builder::from(&shared);
    // Custom endpoints (MinIO, some R2 setups) generally require path-style
    // addressing; virtual-hosted-style is the default against real AWS.
    if endpoint.is_some() {
        builder = builder.force_path_style(true);
    }
    Client::from_conf(builder.build())
}

/// Upload `data` as a multipart upload, aborting the upload on any error so a
/// failure never leaves a dangling in-progress upload. Returns the final ETag.
async fn put_multipart(
    client: &Client,
    bucket: &str,
    key: &str,
    data: Vec<u8>,
) -> Result<Option<String>> {
    let create = client
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| Error::store(format!("create multipart {key}: {e}")))?;
    let upload_id = create
        .upload_id()
        .ok_or_else(|| Error::store(format!("no upload id returned for {key}")))?
        .to_string();

    let mut parts = Vec::new();
    let uploaded = async {
        for (index, chunk) in data.chunks(PART_SIZE).enumerate() {
            let part_number = (index as i32) + 1;
            let part = client
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(ByteStream::from(chunk.to_vec()))
                .send()
                .await
                .map_err(|e| Error::store(format!("upload part {part_number} of {key}: {e}")))?;
            parts.push(
                CompletedPart::builder()
                    .set_e_tag(part.e_tag().map(|s| s.to_string()))
                    .part_number(part_number)
                    .build(),
            );
        }
        Ok::<(), Error>(())
    }
    .await;

    if let Err(err) = uploaded {
        let _ = client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .send()
            .await;
        return Err(err);
    }

    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    let done = client
        .complete_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(&upload_id)
        .multipart_upload(completed)
        .send()
        .await
        .map_err(|e| Error::store(format!("complete multipart {key}: {e}")))?;
    Ok(done.e_tag().map(strip_quotes))
}

/// S3 wraps ETags in quotes; store the bare value. Note (ADR 003): an S3 ETag
/// is not a SHA-256, so callers must treat [`ObjectMeta::etag`] as opaque.
fn strip_quotes(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}

/// Percent-encode an object key for use in a `copy_source` value, leaving path
/// separators and unreserved characters intact.
fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{encode_key, strip_quotes};

    #[test]
    fn encode_key_preserves_paths_and_escapes_specials() {
        assert_eq!(encode_key("music/track.mp3"), "music/track.mp3");
        assert_eq!(
            encode_key("music/17 - Sigh. [Explicit].mp3"),
            "music/17%20-%20Sigh.%20%5BExplicit%5D.mp3"
        );
    }

    #[test]
    fn strip_quotes_unwraps_etag() {
        assert_eq!(strip_quotes("\"abc123\""), "abc123");
        assert_eq!(strip_quotes("abc123"), "abc123");
    }
}
