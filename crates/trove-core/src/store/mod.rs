//! Object store abstraction (the bucket).
//!
//! Trove never talks to S3 directly from clients; instead the core depends on
//! this trait. Three implementations exist:
//!
//! - [`fs::FsStore`] — a local directory tree simulating the bucket (bootstrap,
//!   no credentials).
//! - [`stub::StubStore`] — in-memory, for tests.
//! - [`s3::S3Store`] — the production `aws-sdk-s3` backend (multipart uploads +
//!   staging→`music/` promotion via server-side copy). Compiled only under the
//!   `s3` feature to keep default builds fast and hermetic (ADR 001, ADR 003).

pub mod fs;
#[cfg(feature = "s3")]
pub mod s3;
pub mod stub;

use crate::error::Result;

/// Metadata about a stored object.
#[derive(Debug, Clone)]
pub struct ObjectMeta {
    pub key: String,
    pub size_bytes: u64,
    pub etag: Option<String>,
}

/// Outcome of a conditional write (see [`ObjectStore::put_if_match`]).
#[derive(Debug, Clone)]
pub enum PutOutcome {
    /// The write succeeded.
    Written(ObjectMeta),
    /// The write was rejected because the object's current state didn't match
    /// `expected_etag` (or, for a create-only write, because the object
    /// already existed). `current_etag` is a best-effort snapshot for
    /// diagnostics only — callers that need to retry must re-read current
    /// state themselves rather than trust this value, since it can itself be
    /// stale by the time a retry runs (ADR 007, Group B1).
    Conflict { current_etag: Option<String> },
}

/// A durable object store (S3 or S3-compatible).
///
/// Kept intentionally small for the bootstrap; multipart/resume specifics will
/// be layered on the production implementation behind these same operations.
pub trait ObjectStore: Send + Sync {
    /// Fetch an object's bytes by key.
    fn get(&self, key: &str) -> Result<Vec<u8>>;

    /// Store bytes at a key, returning the resulting object metadata.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<ObjectMeta>;

    /// Return whether an object exists.
    fn exists(&self, key: &str) -> Result<bool>;

    /// Head an object for metadata without downloading it.
    fn head(&self, key: &str) -> Result<Option<ObjectMeta>>;

    /// Copy an object server-side (used to promote staging → `music/`).
    fn copy(&self, from_key: &str, to_key: &str) -> Result<ObjectMeta>;

    /// List object keys under a prefix.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Write `bytes` to `key` only if the object's current etag matches
    /// `expected_etag`. `expected_etag: None` means create-only: the write
    /// succeeds only if no object currently exists at `key`.
    ///
    /// This is the compare-and-swap primitive backing the canonical index
    /// (ADR 007, Group B1) — never used for ordinary content-addressed audio
    /// objects, only for the small `schema-version.json` marker and the
    /// generation-keyed `archive-index/<generation>.jsonl` payload it points
    /// to. Implementations are not required to support conditional writes for
    /// arbitrarily large payloads; see each implementation's doc comment.
    fn put_if_match(
        &self,
        key: &str,
        expected_etag: Option<&str>,
        bytes: &[u8],
    ) -> Result<PutOutcome>;
}
