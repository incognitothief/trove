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

**We record this as a deliberate, pragmatic choice, not a principled one.** The
bridge is the lowest-blast-radius way to keep ADR 000's synchronous core while
adding an async-only SDK, but it has a real smell: it parks a thread per call
and stands up a second runtime purely to satisfy a sync signature. We are fine
paying that now. Two named exits, expanded under *Considered alternatives*,
supersede the bridge if pressure appears: (a) make the `ObjectStore` trait
async if the daemon ever needs genuine concurrency, or (b) adopt a
blocking-native S3 client to delete the bridge outright while staying sync.
Either is an acceptable future direction; both were understood when choosing the
bridge for the first cut.

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

The chosen sync-over-async bridge (Decision §2) is pragmatic, so its rivals are
recorded here in enough detail to pick one up later without re-deriving the
analysis. The first two are the sanctioned exits from the bridge.

- **Make the `ObjectStore` trait (and the core) async — the principled fix.**
  Turn the trait's methods into `async fn`, let `async` propagate up through
  `reconcile` / `import` / `facade`, have `trove-cli` open a runtime in `main`,
  and let `trove-serverd` call the core natively without any bridge or
  thread-parking. This is the cleanest long-term shape and the most likely place
  this ends up **if the daemon ever needs real I/O concurrency** (the bridge
  serializes S3 calls behind blocked worker threads; a truly async core would
  not). Costs, weighed honestly:
  - It is a large, cross-cutting change: every `ObjectStore` caller and both
    clients change, not one module.
  - Because the core holds the store as `Box<dyn ObjectStore>`, `async fn` in
    traits is not directly `dyn`-compatible; it needs `async-trait` (which
    heap-allocates/boxes a future per call) or `#[trait_variant]` — added
    machinery either way.
  - It partially revisits ADR 000's deliberate *thin **synchronous** core /
    mutex-guarded daemon* stance, so it is arguably an ADR-000-scale decision,
    not just an ADR-003 implementation detail. That scope is why we did not take
    it in the first cut, not a claim that the bridge is superior.
- **Use a blocking-native S3 client — delete the bridge while staying sync.**
  A client with first-class blocking support (e.g. the `rust-s3` crate's
  `sync`/blocking feature, or a SigV4 signer over a blocking HTTP client)
  satisfies the existing sync trait **natively**: no tokio, no second runtime,
  no per-call thread-parking. This is the most direct answer to "why bridge at
  all?" and keeps ADR 000's sync core untouched. Cost: it means leaving the
  official `aws-sdk-s3`, which is the more battle-tested implementation of
  request signing, retry/backoff, and multipart edge cases — meaningful for the
  one artifact we protect. A strong option specifically when we want to *keep*
  the core sync; deferred only because the official SDK is the safer default for
  the initial production backend. If the bridge's smell outweighs SDK maturity,
  this is the swap to make.
- **Explicit worker-thread actor instead of spawn-and-block.** A single
  dedicated thread owning a current-thread runtime and receiving command
  messages over a channel — functionally equivalent to the chosen bridge but
  structured as a deliberate actor rather than "spawn onto a runtime, block on a
  channel." Slightly clearer to read and reason about, but it does **not** remove
  the underlying sync/async seam or the thread-parking, so it is a cosmetic
  refinement of the bridge, not an escape from it. Reasonable to adopt if the
  inline bridge proves hard to follow.
- **Call `Runtime::block_on` on the ambient thread.** Rejected: panics inside
  the daemon's `#[tokio::main]` runtime ("Cannot start a runtime from within a
  runtime"). The dedicated-runtime + channel bridge is the safe equivalent, and
  is the reason the bridge spawns rather than blocks directly.
- **Add `aws-sdk-s3` unconditionally (no feature flag).** Rejected: it would
  regress ADR 001's fast, hermetic, credential-free default builds and tests for
  everyone, including contributors who only touch query/playlist/import logic.
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
