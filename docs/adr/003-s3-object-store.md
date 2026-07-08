# ADR 003: The S3-backed Object Store

- **Status:** Proposed
- **Date:** 2026-07-07
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000](000-bootstrap.md), [ADR 001](001-bootstrap-implementation.md)

## Context

[ADR 000](000-bootstrap.md) makes one thing durable — the bucket — and treats
every other artifact as disposable. It names AWS S3 as that durable store and
lays out the on-bucket key layout (`.trove/` index, `music/` canonical audio,
`staging/{import-job-id}/` unverified uploads), the multipart-upload
requirement for large objects, and a compare-and-swap on `schema-version.json`
to protect concurrent importers.

[ADR 001](001-bootstrap-implementation.md) deliberately did **not** add
`aws-sdk-s3`. Instead it introduced the `store::ObjectStore` trait as the bucket
seam and shipped two implementations against it — `store::fs::FsStore` (a local
directory tree that lets the CLI and daemon run end-to-end with no credentials)
and `store::stub::StubStore` (in-memory, for tests). Heavy, network-bound
dependencies were kept out to hold build times low and keep tests hermetic. The
production S3 implementation was recorded as **follow-up #2**:

> Implement the S3 `ObjectStore` (multipart, staging→commit, verify).

This ADR authorizes that implementation. Nothing above the seam changes: the
entire core (`reconcile`, `import`'s `upload → verify → commit → push_index`,
`capture_artwork`, `push_index`) already drives the archive exclusively through
`ObjectStore` and `BucketPaths`, so S3 is a new implementation of an existing
trait, not a change to any caller — exactly the "abstract external systems"
convention in `AGENTS.md`.

Two frictions have to be resolved to land it honestly:

1. **Sync trait, async SDK.** `ObjectStore` is synchronous by design (e.g.
   `fn get(&self, key: &str) -> Result<Vec<u8>>`); the whole core is
   synchronous, and `trove-serverd` deliberately holds the core behind a
   `std::Mutex` and never across an `.await`. `aws-sdk-s3` is async and
   tokio-based. The two must be bridged without infecting the core with `async`
   or panicking when a client already runs inside a tokio runtime.
2. **Build cost and hermeticity.** `aws-sdk-s3` pulls in a large dependency
   tree. ADR 001's fast-feedback, credential-free defaults must survive.

## Decision

### 1. A third `ObjectStore` implementation: `store::s3::S3Store`

Add `store::s3::S3Store`, wrapping `aws-sdk-s3`, implementing the existing trait
verbatim (`get` / `put` / `exists` / `head` / `copy` / `list`). No trait method
is added or changed, so `reconcile`, `import`, and `push_index` are untouched.
`FsStore` and `StubStore` remain as-is for bootstrap and tests.

Operation mapping:

- `get` → `GetObject`, collecting the body into `Vec<u8>`; `NoSuchKey` maps to
  `Error::NotFound(..)` so reconcile's "empty bucket" and the facade's
  generation/manifest probes (which already special-case `Error::NotFound`)
  behave identically to the filesystem store.
- `put` → `PutObject` for small objects; **multipart** above a threshold (see §3).
- `exists` / `head` → `HeadObject`; a 404/`NotFound` becomes `Ok(false)` /
  `Ok(None)` rather than an error.
- `copy` → server-side `CopyObject` (`copy_source = "{bucket}/{from_key}"`).
  This is what makes staging→`music/` promotion at commit a server-side move,
  not a download-and-re-upload.
- `list` → paginated `ListObjectsV2` over a prefix, following continuation
  tokens, returning sorted keys to match the other stores' ordering.

### 2. Sync-over-async bridge (no runtime-within-a-runtime panic)

`S3Store` owns a **dedicated multi-threaded tokio runtime** and an
`aws_sdk_s3::Client`. Each trait method bridges to async by **spawning the
future onto the owned runtime and blocking the calling thread on a
`std::sync::mpsc` receive** — never by calling `Runtime::block_on` on the
ambient thread.

This distinction is deliberate and load-bearing:

- `trove-cli` calls the core from a synchronous `main` with no ambient runtime.
- `trove-serverd` constructs and calls the core from **inside** its
  `#[tokio::main]` runtime. Calling `block_on` there would panic
  ("Cannot start a runtime from within a runtime"). Spawning onto a *separate*
  runtime and blocking on a channel does not, because the calling thread only
  parks on a synchronous receive — it never nests runtimes.

The owned runtime is also used to build the client (credential/region loading
is async), via the same spawn-and-block helper, so even construction is safe
from inside the daemon's runtime.

Blocking a daemon worker thread for the duration of an S3 call is acceptable:
core calls are already synchronous and already serialize behind the daemon's
`std::Mutex` (SQLite work blocks the same way today). Making the core itself
async, or introducing a distinct async object-store trait, is explicitly out of
scope — it would ripple through every caller and both clients for no benefit at
this stage.

### 3. Multipart uploads

`put` chunks buffers larger than a fixed threshold into a multipart upload
(`CreateMultipartUpload` → N × `UploadPart` → `CompleteMultipartUpload`),
aborting the upload on error; smaller buffers use a single `PutObject`. This
satisfies ADR 000's "large files use multipart uploads" for the sizes a
100–500 GB backfill produces.

Scope boundary, stated plainly: the trait is **byte-oriented**
(`put(&self, key, bytes: &[u8])`), and the import pipeline already reads whole
files into memory (`std::fs::read`) before calling `put`. So multipart here
chunks an in-memory buffer; it is **not** yet streaming-from-disk with
persisted, resume-from-checkpoint part state. True resumable multipart (survive
a crash mid-file and continue) requires evolving the trait to a streaming,
checkpointed shape and is left as a follow-up. This ADR does not claim it.

### 4. Feature-gated; clients select the store from config

`aws-sdk-s3`, `aws-config`, and `tokio` are added to `trove-core` behind a
non-default **`s3` feature**; `store::s3` is `#[cfg(feature = "s3")]`. This
preserves ADR 001's guarantees precisely:

- `cargo build` / `cargo test` (no features) stay fast and hermetic — no AWS
  tree, no credentials, existing tests unchanged.
- Production builds opt in with `--features s3`. `trove-cli` and
  `trove-serverd` expose a matching `s3` feature that forwards to
  `trove-core/s3`.

Backend selection stays in **client wiring** (`runtime.rs`), never in the core,
per the "wiring stays in clients" convention:

- If the configured bucket is the local sentinel (`region = "local"` or
  `name = "local"`, the synthesized default), use `FsStore` against
  `TROVE_BUCKET_DIR` — unchanged bootstrap behavior.
- Otherwise, when built with `s3`, construct `S3Store` from `bucket.name`,
  `bucket.region`, and the optional `bucket.endpoint` (already in `Config`,
  present for S3-compatible stores like MinIO/R2).
- Otherwise (real bucket configured, but `s3` not compiled in) fail with a clear
  message telling the operator to rebuild with `--features s3` or set
  `region = "local"`.

### 5. ETag semantics and verification

S3's `ETag` is not a SHA-256 (single-part uploads expose an MD5; multipart
exposes an MD5-of-MD5s with a `-N` suffix). Trove does not depend on ETag
equalling a content hash anywhere: `import::verify` checks **object size** via
`head`, and integrity is anchored by the **SHA-256 Trove computes itself** and
records in the archive index (`ArchiveEntry.sha256`). `S3Store` returns the raw
S3 ETag in `ObjectMeta.etag` (unquoted) as opaque metadata. This is a real
semantic difference from `FsStore`/`StubStore` (which put a SHA-256 in `etag`),
and it is safe only because nothing compares `etag` for correctness. Callers
must continue to treat `ObjectMeta.etag` as opaque.

## Consequences

### Positive

- **The bucket becomes real.** The durability promise at the center of ADR 000
  is finally backed by S3; recovery, import, and reconcile run against an actual
  durable store, not a simulation.
- **Zero caller churn.** Because S3 is just another `ObjectStore`, no core
  logic, no CLI command, and no daemon handler changes — the seam paid off.
- **ADR 001's fast path survives.** Default builds and tests stay AWS-free and
  hermetic; the heavy tree is opt-in.
- **Runs under both clients.** The spawn-and-block bridge works from the CLI's
  plain `main` and from inside the daemon's tokio runtime without panicking.
- **S3-compatible out of the box.** The existing `endpoint` config makes MinIO,
  Cloudflare R2, and friends usable with no code change.
- **Server-side promotion.** `copy` maps to `CopyObject`, so staging→`music/`
  commit is a server-side operation rather than a re-upload.

### Negative / trade-offs

- **Three store implementations to keep aligned.** `FsStore`, `StubStore`, and
  `S3Store` must agree on the contract that matters (NotFound mapping, size in
  `head`, key ordering in `list`). ETag semantics now legitimately differ; the
  contract that callers may not rely on ETag becomes load-bearing.
- **A thread parks per S3 call.** The sync-over-async bridge blocks the caller
  (a daemon worker thread) for the duration of each request. Fine given the
  already-synchronous, mutex-serialized core, but it caps in-flight concurrency
  and is not the shape a future async core would want.
- **In-memory multipart, not streaming/resumable.** Whole files are still read
  into memory before upload, and multipart cannot yet resume mid-file after a
  crash. This is narrower than ADR 000's eventual intent and is called out as a
  follow-up, not delivered here.
- **Bigger, slower production builds.** `--features s3` brings in the full AWS
  SDK compile cost. Contained to opt-in builds by design.
- **Concurrency safety on index writes is still open.** This ADR does not add
  the compare-and-swap on `schema-version.json`; `push_index` remains a
  read-modify-write. Two simultaneous importers against a real bucket can still
  clobber the canonical index. That remains ADR 001 follow-up #3.

## Considered alternatives

- **Make the `ObjectStore` trait (and the core) async.** Rejected for now:
  it would propagate `async` through every core method and both clients,
  contradicting ADR 000's thin-sync-core / mutex-guarded-daemon design, for no
  user-visible gain. The bridge confines all async to one module.
- **Call `Runtime::block_on` on the ambient thread.** Rejected: panics inside
  the daemon's `#[tokio::main]` runtime. The dedicated-runtime + channel bridge
  is the safe equivalent.
- **Add `aws-sdk-s3` unconditionally (no feature flag).** Rejected: it would
  regress ADR 001's fast, hermetic, credential-free default builds and tests for
  everyone, including contributors who only touch query/playlist/import logic.
- **Use a lighter hand-rolled S3 client (raw SigV4 over `reqwest`).** Rejected:
  more security-sensitive surface (request signing, retries, multipart edge
  cases) to own and get right than is justified; the official SDK is the durable
  choice for the one artifact we protect.
- **Stream from disk with persisted per-part checkpoints now.** Deferred (not
  rejected): the right end state, but it requires a streaming/checkpointed trait
  shape. Landing byte-oriented multipart first keeps this ADR scoped and honest
  about what resume does and does not yet cover.

## Scope

- **This ADR authorizes:** a feature-gated (`s3`) `store::s3::S3Store` — a third
  implementation of the existing `ObjectStore` trait wrapping `aws-sdk-s3`, with
  a dedicated-runtime sync-over-async bridge, in-memory multipart above a size
  threshold, server-side `copy`, paginated `list`, `NotFound` mapping, and
  config-driven store selection (local sentinel → `FsStore`, else `S3Store`)
  living in client wiring. Endpoint override for S3-compatible stores is
  supported via existing config.
- **Explicitly not in scope (named, deferred):** streaming/resume-from-checkpoint
  multipart (needs a trait evolution); compare-and-swap on `schema-version.json`
  for concurrent importers (ADR 001 follow-up #3); a `delete` operation (not on
  the trait); and any move of the core to `async`.
