# ADR 001: Bootstrap Implementation Retrospective

- **Status:** Accepted
- **Date:** 2026-07-07
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000: Bootstrap Architecture](000-bootstrap.md)

## Context

[ADR 000](000-bootstrap.md) defined the target architecture: a single
`trove-core` Rust crate holding all domain logic, with thin CLI, daemon, and
React UI clients over it; S3 as the only durable source of truth; disposable
local SQLite caches; bucket reconciliation on reads; and a resumable,
crash-safe bulk import.

This ADR records what was actually built when that architecture was scaffolded,
which decisions were made during implementation, where the implementation
deliberately deferred work behind clean seams, and what remains. It exists so a
future contributor can trust the code matches ADR 000's intent and can see, at a
glance, what is real versus stubbed.

The workspace was green at the time of writing: only ADR 000 and a stub
`README.md` existed, and the Rust toolchain was not installed.

## Decision

We scaffolded the full workspace exactly along ADR 000's boundaries and made the
core runnable end-to-end. The following decisions were taken during
implementation.

### Toolchain and workspace

- Installed the stable Rust toolchain (1.96.1) with `rustfmt` and `clippy`;
  pinned via `rust-toolchain.toml`.
- A single Cargo workspace with three members: `crates/trove-core`,
  `crates/trove-cli`, `crates/trove-serverd`. Edition 2021, shared dependency
  versions declared in `[workspace.dependencies]`.
- Node/npm (already present) drives the `ui/` package (Vite + React + TS).

### Core dependencies

Chosen to be real and current but cheap to compile:

- `rusqlite` (bundled SQLite), `serde`/`serde_json`/`toml`, `chrono`,
  `uuid` (v4), `sha2`, `thiserror`, `tracing`, `dirs`.
- CLI: `clap` (derive). Daemon: `tokio`, `axum` 0.7, `tower-http`.

Heavy, slow-to-compile, network-bound dependencies (notably `aws-sdk-s3` and an
audio-metadata library) were **intentionally not added yet**, and instead placed
behind traits (see below). This keeps build times low and the core testable
without credentials during bootstrap.

### Abstraction seams introduced

Two seams were added that ADR 000 implied but did not name explicitly. Both
preserve the "one authoritative implementation" rule while letting the durable
backends land later without churn:

- **`store::ObjectStore` trait** — the bucket. The core never names S3 directly.
  Two implementations ship now:
  - `store::fs::FsStore` — simulates the bucket as a local directory tree, so
    the CLI and daemon run fully end-to-end with **no AWS credentials**.
  - `store::stub::StubStore` — in-memory, used by unit tests.
    The production `aws-sdk-s3` implementation (multipart + staging) will be a
    third impl of this same trait.
- **`metadata::MetadataExtractor` trait** — audio metadata extraction, with a
  filename-only `StubExtractor` for now. The real `lofty`/`symphonia`-based
  extractor is a drop-in replacement.

### What was implemented for real

- **Config** (`config`) — parses `~/.trove/config.toml`, resolves `~/.trove`
  (honoring `TROVE_HOME`), synthesizes a local default when absent.
- **Local index cache** (`db::archive`) — `archive.sqlite` with full upsert /
  replace-all / query, plus the reconcile generation stamp in a `meta` table.
- **Reconciliation** (`archive::reconcile`) — compares the local generation to
  the bucket's `schema-version.json`; returns `UpToDate` (near-instant no-op),
  `Rehydrated` (pulls `archive-index.jsonl` and rebuilds the cache),
  `EmptyBucket`, or `OfflineFallback`.
- **Bucket index interchange** (`archive::index`) — JSONL and schema-version
  (de)serialization, and `BucketPaths` for the `.trove/`, `staging/`, and
  `music/` key layout from ADR 000.
- **Query** (`query` + `db::archive`) — a declarative `QuerySpec` compiled to
  parameterized SQL (artist/album/genre/key/file-type, BPM/year ranges, text,
  tags, limit).
- **Playlists** (`playlist`) — `playlists.sqlite` with create/add/remove/list/get.
- **Bulk import** (`import`) — the full ADR pipeline
  `scan → hash → dedupe → upload(staging) → verify → commit(music/) → push index`,
  running against `ObjectStore`. Per-file `FileState` and `Phase` enums are
  explicit; commit is strictly last; the archive index only advances after
  commit (`facade::push_index`).
- **Volumes** (`volume`) — `init` writes the standard `Music/`, `Playlists/`,
  `.trove-volume.json` layout; identity read-back.
- **Sync planning** (`sync`) — computes per-track destination paths from the
  configured export layout and reports bytes remaining.
- **Facade** (`Trove`) — the single entry point clients drive; also offers
  `in_memory` / `in_memory_with_store` constructors used by tests.
- **CLI** (`trove`) — clap surface mirroring ADR 000 (`archive`, `import`,
  `query`, `playlist`, `volume`, `sync`), with `--json` and `--offline` globals.
- **Daemon** (`trove-serverd`) — axum HTTP/JSON API (`/health`, `/reconcile`,
  `/query`, `/playlists`, `/import`) over a shared `Arc<Mutex<Trove>>`, bound to
  loopback.
- **UI** (`ui/`) — a React thin client (library search, playlist sidebar,
  multi-select add-to-playlist, folder import) that talks only to the daemon via
  a `/api` proxy.
- **Tests** — `trove-core` unit tests cover reconcile (hydrate → no-op), empty
  bucket, query filtering, and playlist round-trip. `cargo build`, `cargo test`,
  and `cargo clippy --all-targets` are all clean; the UI builds and type-checks.

### Deliberate deferrals (stubbed behind seams)

Marked in code with `Error::NotImplemented` or explicit notes:

- **S3-backed `ObjectStore`** with multipart and resumable uploads.
- **Real metadata extraction** (currently filename-only).
- **Transfer execution** — `sync` plans transfers but does not move bytes;
  no retry/backoff or verification loop yet.
- **Playlist export** (`.m3u8`) and **volume diff/status** beyond identity.
- **`archive verify`**.
- **Persistent import bookkeeping** — the `import_jobs` / `import_files`
  schema exists in `db::schema`, but the import pipeline currently carries job
  state in memory; resume is not yet wired to those tables.
- **Per-volume database** — the `VOLUME_SCHEMA` is defined but volume operations
  do not yet read/write `~/.trove/volumes/{id}.sqlite`.
- **Write serialization** — `push_index` advances the generation with a
  read-modify-write; the compare-and-swap on `schema-version.json` that ADR 000
  calls for (to protect concurrent importers) is not yet implemented.

### Bootstrap storage backend

To make the tool usable immediately, the CLI and daemon default to `FsStore`
with two environment variables:

- `TROVE_HOME` — cache/config root (default `~/.trove`).
- `TROVE_BUCKET_DIR` — simulated bucket directory (default `~/.trove/bucket-sim`).

Swapping in the S3 store later is a client-wiring change only; no core code
changes.

## Consequences

### Positive

- **The core is real and exercised.** Reconcile, query, playlists, and the
  entire import pipeline run end-to-end today (against the filesystem store),
  with passing tests and a clean clippy.
- **ADR 000's boundary held.** All logic lives in `trove-core`; the CLI, daemon,
  and UI are genuinely thin. Adding S3 or real metadata touches one trait impl,
  not the clients.
- **Fast feedback.** Avoiding `aws-sdk-s3`/audio deps keeps builds in seconds and
  tests hermetic (no network, no credentials).
- **Honest status.** Every deferral is visible in code (`NotImplemented`) and
  listed here, so no capability is silently implied.

### Negative / trade-offs

- **Two storage impls to eventually reconcile.** `FsStore` and `StubStore` exist
  alongside the future S3 store; their semantics (e.g. etag/verify) must be kept
  aligned so behavior does not diverge between bootstrap and production.
- **Client wiring is duplicated.** `runtime.rs` (config + store selection) is
  copied between `trove-cli` and `trove-serverd`. Acceptable for now; a small
  shared "app wiring" helper may be warranted, but was kept out of `trove-core`
  to avoid leaking client concerns into the domain crate.
- **Import durability gap.** Because import state is in memory, a crash mid-job
  currently restarts the job rather than resuming — the opposite of ADR 000's
  headline guarantee. This is the highest-priority follow-up.
- **No concurrency safety on index writes yet.** Until compare-and-swap lands,
  two simultaneous importers could clobber the canonical index.

## Follow-ups (recommended order)

1. Persist import state to `import_jobs`/`import_files` and wire
   `import resume`/`status`, delivering the crash-safe resume ADR 000 promises.
2. Implement the S3 `ObjectStore` (multipart, staging→commit, verify).
3. Add compare-and-swap on `schema-version.json` for index writes.
4. Real metadata extraction; transfer execution with retry/backoff + verify.
5. Playlist `.m3u8` export and per-volume DB (diff/status).

## Considered alternatives

- **Add `aws-sdk-s3` immediately.** Rejected for bootstrap: heavy compile times
  and credential requirements would slow iteration and block hermetic tests,
  for no gain over the `ObjectStore` seam.
- **Skip the filesystem store and stub every backend.** Rejected: `FsStore`
  costs little and makes the tool genuinely runnable end-to-end, which is far
  more valuable for validating the core than a no-op stub.
- **Persist import bookkeeping in this pass.** Deferred (not rejected): the state
  machine and schema are in place; wiring persistence is the first follow-up
  rather than part of the initial scaffold.
