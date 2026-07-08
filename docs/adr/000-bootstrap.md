# ADR 000: Bootstrap Architecture

- **Status:** Accepted
- **Date:** 2026-07-07
- **Supersedes:** —
- **Superseded by:** —

## Context

Trove is a flexible and portable DJ library recovery and export tool.

The core problem it solves: a main music archive is too valuable to casually
carry everywhere, but performance contexts require local audio files on a
mounted drive. DJs need a way to keep the canonical archive somewhere durable,
query and build subsets locally, and quickly materialize a performance-ready
USB/SSD if a drive is lost, corrupted, or unavailable.

The design must support a clean recovery story: a new or wiped machine should be
able to reach the full library without crawling raw object storage or
re-reading file metadata for every track.

The target workflow is:

> Query archive → select tracks/crate → plug in new SSD → export files → write
> playlists → open in Mixxx

## Decision

We adopt the following foundational architecture.

### Core mental model

Four distinct roles, each with a single responsibility:

| Concept        | Role                              |
| -------------- | --------------------------------- |
| Bucket storage | Durable archive (source of truth) |
| Local drive    | Disposable performance cache      |
| Local index DB | Rebuildable search surface        |
| Sync state     | Resumable transfer bookkeeping    |

Stated as a separation of indexes:

- **S3 index** = canonical archive map (single source of truth)
- **Local index** = disposable, reconstructable cache of the S3 index
- **Volume index** = materialized subset state
- **Mixxx** = performance library

The removable volume is disposable. The local host is disposable. `~/.trove` is
a convenient operational cache, not an authority. S3 is the only durable source
of truth.

#### Guiding principle: everything but the bucket is disposable

Trove is built for the express purpose of treating caches — and the devices that
hold them — as disposable. Any machine in hand (a laptop, a borrowed computer, a
freshly wiped install) is assumed to be transient. The only artifact we protect
is the bucket and its canonical index.

This has a concrete operational rule:

> **Reads reconcile with the bucket.** Before serving a read operation (query,
> playlist listing, volume diff, export planning), Trove reconciles the local
> index against the canonical bucket index. If the local cache is missing,
> stale, or absent, it is (re)hydrated from the bucket first.

Reconciliation is cheap because it compares index metadata (a
`schema-version.json` marker plus per-entry `updated_at`/`sha256`), not audio
objects. When the local cache already matches the bucket generation, the check
is a near-instant no-op; when it does not, only the changed index entries are
pulled. Audio files are still fetched lazily on export, never during a read.

This keeps the mental model honest: you can throw away any device at any time and
lose nothing but a cache that rebuilds itself on the next read.

### Stack

All domain logic lives in a single Rust core crate. Every other surface is a
thin client over it and holds no business logic of its own, so there is only one
implementation to maintain and keep correct.

- **`trove-core` (Rust library crate)** — the one authoritative implementation:
  headless indexing, bucket reconciliation, S3 sync/export, metadata extraction,
  query, playlist generation, volume detection, and resumable transfer tracking.
- **`trove-cli` (thin client)** — argument parsing and terminal output over
  `trove-core`. No logic beyond translating flags into core calls.
- **`trove-serverd` (thin client)** — a local HTTP/JSON API over `trove-core`,
  serving the UI. Also holds no logic beyond exposing core operations.
- **React Web UI (thin client)** — a local browser interface for archive search,
  crate/list building, export progress, and volume status. Pure presentation:
  it only renders responses from `trove-serverd` and issues commands back to it.
- **AWS S3** — durable object storage holding the canonical music archive and
  serving as the source for rebuilding performance drives.
- **SQLite** — local archive index cache, per-volume indexes, and sync state.

> **Out of scope (future):** packaging the React UI and `trove-core` together as
> a single Tauri desktop app is appealing and would let the UI call core
> directly instead of over HTTP. We are deliberately not doing this now; the
> thin-client boundary above keeps that option open without committing to it.

### Data flow

```
S3 archive  (canonical index + audio objects)
  ↓
trove-core  (Rust: reconcile, query, sync, export)
  ↓                         ↑
SQLite indexes        trove-cli  ── thin clients ──  trove-serverd → React UI
(local cache)
  ↓
Mounted performance volume
  ↓
Mixxx
```

Both `trove-cli` and `trove-serverd` (and therefore the React UI) drive the same
`trove-core`; they never touch S3, SQLite, or volumes directly.

### Local layout (`~/.trove`)

The host holds operational cache and bookkeeping only — none of it is
authoritative. Databases and their responsibilities:

- `~/.trove/archive.sqlite` — disposable local cache of the bucket index,
  reconciled on every read and rebuildable at any time. Contains tracks,
  artists, albums, file paths, S3 object keys, metadata, checksums, tags,
  BPM/key, and import history.
- `~/.trove/volumes/{volume_id}.sqlite` — last-known index for each
  mounted/exported volume. Contains volume identity, relative paths, copied
  files, sizes, mtimes, hashes, last-seen time, and sync/export status.
- `~/.trove/sync.sqlite` — operational transfer state: active downloads, partial
  files, resume checkpoints, failed transfers, and verification status. Also
  holds bulk-import bookkeeping (`import_jobs`, `import_files`); a dedicated
  `~/.trove/import.sqlite` may be split out for large backfills. See
  [Initial archive backfill](#initial-archive-backfill-bulk-import).
- `~/.trove/playlists.sqlite` — logical playlists/crates (Trove playlists, not
  yet Mixxx playlists).
- `~/.trove/config.toml` — local configuration: S3 bucket, profiles, preferred
  export layout, default music folder, and Mixxx playlist settings.

Temporary caches live on the host, never on the removable drive:

```
~/.trove/cache/
├── downloads/
├── artwork/
├── manifests/
└── temp/
```

Used for partial downloads, extracted metadata, generated manifests,
retry-safe staging, and artwork thumbnails.

### Mounted volume layout

The mounted drive stays boring and portable, and must **not** carry the main
`.trove` database folder:

```
DJ_VOLUME/
├── Music/
│   └── Artist/Album/Track.flac
├── Playlists/
│   └── tonight.m3u8
└── .trove-volume.json
```

It may contain only a tiny identity file, `.trove-volume.json`, used to
recognize the drive across mounts.

### Bucket-side index

We keep a canonical, portable index in the bucket. Without it, every new machine
would have to discover the archive by crawling S3 objects and re-reading file
metadata, which defeats the purpose.

```
s3://your-bucket/.trove/
├── archive-index.sqlite    # for fast local restore
├── archive-index.jsonl     # durable, inspectable interchange format
├── playlists.jsonl
├── schema-version.json
├── manifests/
└── staging/                # per-job unverified uploads, promoted on commit
    └── {import-job-id}/
```

We keep **both** the SQLite form (fast local restore) and the JSONL form
(durable, inspectable interchange). Each bucket index entry contains:

- `track_id`
- `object_key`
- `size_bytes`
- `sha256`
- `metadata`
- `tags`
- `imported_at`
- `updated_at`
- `source_path_original`
- `artwork_object_key`

This makes S3 the source of truth without requiring S3 itself to be queryable.
The `schema-version.json` marker doubles as the reconciliation generation
stamp: a local cache compares its stored generation against this file to decide
whether it is current before serving reads.

### CLI surface (headless)

**Archive management** — fetch/update the canonical archive index from S3:

```
trove archive pull-index
trove archive push-index
trove archive verify
```

**Import new content** — scan files, extract metadata, compute hashes, upload
audio to S3, and update the archive index:

```
trove import ~/Downloads/new-music
```

The very first import is a special case (backfilling the whole archive) and is
treated as a resumable bulk migration — see
[Initial archive backfill (bulk import)](#initial-archive-backfill-bulk-import).

**Query songs** — reconcile the local cache against the bucket index, then
return matching tracks (offline use falls back to the last cached index):

```
trove query --artist "Theo Parrish"
trove query --bpm 118:124 --genre house --key 8A
trove query --text "dub techno warmup"
```

**Playlist/crate management** — playlists are logical selections first, exported
to portable files second:

```
trove playlist create "tonight"
trove playlist add tonight --query --bpm 120:128 --genre house
trove playlist remove tonight <track-id>
trove playlist export tonight --format m3u8
```

**Flash/sync a mounted drive** — copy/download needed tracks, resume interrupted
transfers, verify files, write playlists, and update the per-volume DB:

```
trove volume init /Volumes/DJ_USB
trove sync playlist tonight --to /Volumes/DJ_USB
trove sync query --genre techno --bpm 126:132 --to /Volumes/DJ_USB
trove sync resume
trove sync verify /Volumes/DJ_USB
```

**Inspect volumes** — show what is present, missing, stale, or safe to delete:

```
trove volume list
trove volume status /Volumes/DJ_USB
trove volume diff /Volumes/DJ_USB --playlist tonight
```

### Initial archive backfill (bulk import)

The first import is fundamentally different from routine "drop a few new tracks
in" imports: it seeds the entire archive from an existing 100–500 GB local
library. We treat it as a **resumable bulk migration**, not a normal upload.

The guiding rule is: **never make success implicit.** Every file carries an
explicit state, and the canonical archive index only advances _after_ files are
uploaded and verified. Commit is always the last step.

#### Phases

A bulk import proceeds through ordered, resumable phases:

```
scan → fingerprint/hash → dedupe → upload → verify → commit index
```

Because commit is last, a crash at any earlier phase leaves the canonical index
untouched — the job simply resumes from the last safe state.

#### Required robustness features

1. Resumable upload state
2. Per-file status tracking
3. Retry with exponential backoff
4. Checksum verification
5. Duplicate detection
6. Dry-run mode
7. Import manifest output
8. Crash-safe commits

#### Import job state (local)

Bulk import bookkeeping lives in local tables (in `~/.trove/sync.sqlite`, or a
dedicated `~/.trove/import.sqlite`), so a crashed job resumes without redoing
work:

- `import_jobs` — one row per bulk import (job id, source root, phase, counts,
  timestamps, staging prefix).
- `import_files` — one row per file, each in an explicit state:

```
pending → scanning → hashed → duplicate
                            → uploading → uploaded → verified → committed
                            → failed
```

For a 100–500 GB library the expensive work is re-reading files, so
`import_files` caches enough to skip already-done work on resume:

- `path`
- `size`
- `mtime`
- `sha256`
- `metadata_extracted`
- `upload_status`
- `s3_object_key`
- `etag`/`checksum`

If the process crashes, it resumes from the last known safe state rather than
rescanning and re-hashing from scratch.

#### S3 upload specifics

Large files use **multipart uploads**, with enough persisted state to resume (or
at least safely restart) an individual file. Per object we store:

- `object_key`
- `size_bytes`
- `sha256`
- `s3_etag`/`checksum`
- `upload_started_at`
- `upload_completed_at`
- `verified_at`

Uploads land in a per-job **staging prefix** first and are only promoted into
the archive namespace after verification:

```
s3://your-bucket/.trove/staging/{import-job-id}/...   # unverified uploads
s3://your-bucket/music/...                            # committed, canonical
```

Only after objects verify are they committed into the `music/` namespace (or
marked committed in the index).

#### Initial import workflow

1. Scan the local archive.
2. Build a local import manifest.
3. Detect duplicates.
4. Upload files to the staging prefix.
5. Verify uploaded objects.
6. Promote/commit objects into the archive namespace.
7. Write `archive-index.sqlite`/`.jsonl`.
8. Upload the bucket-side index.

#### CLI surface

```
trove import ~/Music --bulk --resume    # convenience: plan + run, resumable

trove import plan ~/Music               # scan, hash, dedupe → manifest (dry-run friendly)
trove import run <job-id>               # upload to staging + verify
trove import resume <job-id>            # continue an interrupted job
trove import status <job-id>            # per-file/per-phase progress
trove import verify <job-id>            # re-verify uploaded objects
trove import commit <job-id>            # promote to music/ and advance the index
```

Splitting `plan`/`run`/`verify`/`commit` lets the first upload be interrupted at
any point without ever leaving the archive in an ambiguous state.

### UI workflows

1. **Library search** — search the archive by artist, title, album, genre, BPM,
   key, date, tags, comments, file type, and duration. Output: a track table
   with preview metadata and selection controls.
2. **Playlist builder** — build logical crates/playlists from search results
   (search → filter → select → save). These are Trove playlists, not Mixxx
   playlists.
3. **Flash drive workflow** — plug in drive → choose playlist/query → choose
   export layout → start sync → verify → write `.m3u8` files → open in Mixxx.
   The UI shows tracks selected, already present, to download, missing from
   archive, bytes remaining, failed transfers, and verification status.
4. **Import/upload workflow** — choose local folder → scan files → review
   metadata → detect duplicates → upload to S3 → update archive index → push
   bucket-side index. This is how new music enters the durable archive.
5. **Recovery workflow** — new computer or wiped `~/.trove` → configure S3
   bucket → pull archive index → query library immediately → plug in new drive →
   flash selected playlist. This is the primary justification for the
   bucket-side index.

## Consequences

### Positive

- **Recovery is fast and cheap.** A wiped machine restores by pulling one index
  object, not by crawling the entire bucket.
- **Devices are truly disposable.** Because reads reconcile with the bucket and
  the local index rebuilds itself, losing or wiping any device costs nothing but
  a cache refresh.
- **One authority, no ambiguity.** The bucket index is the only source of truth,
  so there is never a question of which copy "wins" — local and volume state are
  always derivations of it.
- **Clear separation of concerns.** Durable archive, disposable cache,
  rebuildable index, and transfer bookkeeping never leak into each other.
- **Portable drives stay dumb.** Any drive is recognizable via a tiny identity
  file and works standalone in Mixxx without Trove installed.
- **Inspectable source of truth.** The JSONL bucket index can be read and
  audited without Trove or SQLite.
- **Resumable, verifiable transfers.** Sync state is first-class, so
  interrupted flashes can resume and be verified.
- **Single implementation, many surfaces.** With all logic in `trove-core` and
  the CLI, local daemon, and React UI as thin clients, there is no duplicated
  behavior to keep in sync — a bug is fixed once, and new features surface
  everywhere at once.
- **The scariest operation is the safest.** The initial 100–500 GB backfill is
  fully resumable and crash-safe: explicit per-file state, staging-then-commit,
  and an index that only advances after verification mean an interrupted first
  upload never leaves the archive ambiguous.

### Negative / trade-offs

- **Index duplication.** Maintaining both SQLite and JSONL bucket indexes, plus
  a local working copy, requires disciplined write/push flows and a
  `schema-version.json` to manage migrations.
- **Read-path latency and connectivity.** Because reads reconcile with the
  bucket first, every read incurs at least a lightweight generation check, and a
  fully cold cache blocks until it hydrates. Offline reads must explicitly fall
  back to the last cached index and accept possible staleness.
- **Write serialization.** With a single canonical index, concurrent importers
  (multiple devices pushing at once) need a write discipline — generation
  stamping or a compare-and-swap on `schema-version.json` — to avoid clobbering
  the canonical index.
- **S3 is not directly queryable.** All querying depends on a materialized
  index, so a stale or missing index degrades the experience until refreshed.
- **Staging costs space and transfers.** Uploading to a staging prefix and then
  promoting into `music/` means transient duplicate storage, and a server-side
  copy (or re-upload) at commit time — the deliberate price of never letting the
  index reference an unverified object.
- **Client/daemon boundary cost.** Isolating all logic in `trove-core` behind a
  local HTTP API means defining and versioning that API, running a local daemon
  for the UI, and paying serialization overhead the CLI (which links the core
  directly) does not. This is the deliberate price of never duplicating logic.

## Considered alternatives

For the client architecture, we evaluated the following before settling on a
single `trove-core` crate with thin CLI, daemon, and React clients:

- **Logic duplicated across a standalone CLI and a separate UI backend.**
  Rejected: two implementations of query/sync/reconcile inevitably drift and
  double the surface for bugs.
- **Tauri desktop app** (React webview + Rust core bundled as one binary, UI
  calling core directly instead of over HTTP). Attractive and compatible with
  the chosen core boundary, but **out of scope for now**. The thin-client design
  leaves this open as a future packaging option without rework.
- **Terminal UI (e.g. `ratatui`) instead of a web UI.** Rejected for now: a
  single binary is appealing, but a TUI is weaker for rich, artwork-heavy
  library browsing and crate building, which is central to the DJ workflow.
- **CLI-only, defer any UI.** Reasonable as a staging path and enabled by this
  design (the UI is additive), but we want the React UI as a first-class surface
  rather than a later bolt-on.
