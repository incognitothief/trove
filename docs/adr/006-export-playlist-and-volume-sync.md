# ADR 006: Export — Playlist Files, Volume Sync, and Drive Flash

- **Status:** Proposed
- **Date:** 2026-07-08
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000](000-bootstrap.md), [ADR 001](001-bootstrap-implementation.md), [ADR 004](004-content-addressed-audio-objects.md)

## Context

[ADR 000](000-bootstrap.md)'s target workflow ends on a performance drive:

> Query archive → select tracks/crate → plug in new SSD → export files → write
> playlists → open in Mixxx

Genesis specifies:

- logical Trove playlists first, portable playlist files second
- `trove sync playlist|query --to <mount>`, `sync resume`, `sync verify`
- volume init / list / status / diff
- a dumb volume layout (`Music/`, `Playlists/`, `.trove-volume.json`) with
  host-side per-volume state in `~/.trove/volumes/{id}.sqlite`
- Mixxx as a consumer of relative playlist paths, not as Trove's DB format

[ADR 001](001-bootstrap-implementation.md) delivered:

- volume `init` + identity read-back
- sync **planning** (`plan_playlist_sync`, destination paths from export layout)
- playlist create/add/remove/list in SQLite

Still missing:

- moving bytes from the bucket onto a volume
- writing `.m3u8` / `.m3u`
- per-volume DB (schema exists, unused)
- volume list / status beyond identity / diff
- `sync resume` / `sync verify` / `sync query`
- UI flash-drive workflow

[ADR 004](004-content-addressed-audio-objects.md) made canonical bucket keys
opaque (`music/<sha256>.<ext>`). Export must therefore **derive human-facing
paths from metadata** (and fallbacks), not from the object key — volume paths
and playlist lines are presentation, not storage identity.

This ADR authorizes the export/flash surface that completes the genesis DJ loop.

## Decision

### 1. Export layout is host/volume presentation, not bucket identity

When writing tracks onto a volume, destination paths come from configured
`[export].layout` and track metadata:

| Layout | Path under volume |
| ------ | ----------------- |
| `artist-album` | `Music/<Artist>/<Album>/<display-name>.<ext>` |
| `flat` | `Music/<display-name>.<ext>` |

**Display name:** prefer metadata title (and optional track-number prefix if
present later); fall back to a sanitized stem from `source_path_original` when
title is missing. **Never** use the raw SHA object basename as the Mixxx-facing
filename for `artist-album` / default UI expectation — that would dump hash
names onto the USB.

Extension for the volume file: take from the canonical `object_key` (or
`metadata.file_type`) so lossless type is preserved.

Sanitize path components enough for portable filesystems (strip `/` and control
chars); keep readability. Collision on the same volume path: last writer wins
is wrong — prefer detect-and-suffix (`title (2).ext`) or fail the transfer for
operator review; implementation may choose suffix-with-warning for the first cut.

### 2. Playlist export writes portable files; Trove playlists stay logical

`trove playlist export <name> --format m3u8|m3u` (and the same from core/UI):

- Resolves playlist track ids against the reconciled archive.
- Emits a playlist file whose entries are **relative paths under the volume
  root** (or under a chosen export root), matching Mixxx-friendly
  `relative_paths = true` in config.
- Does **not** rewrite Mixxx's own library DB.

Default output location when flashing: `Playlists/<name>.m3u8` on the target
volume. Standalone export without a volume may write to stdout or a user path —
thin CLI detail; core returns the playlist body + intended relative path list.

### 3. Sync = plan → transfer → verify → record volume state

#### Planning (extends today's `SyncPlan`)

Inputs: playlist and/or query → set of `ArchiveEntry`s; mount point; export
layout. Output: transfers with `object_key`, destination relative path,
`bytes_total`; tracks already present (same sha256 at destination) skipped.

Presence check uses the **per-volume DB** (and/or quick size/mtime/sha of file
on disk when the DB is cold). Prefer DB as cache; refresh when mounting/syncing.

#### Execution

For each pending transfer:

1. `ObjectStore::get` (or a future streaming get) the canonical object.
2. Write atomically into place under `Music/...` (temp + rename in the same
   directory).
3. Update the host volume DB row: track_id, relative_path, size, sha256, status
   (`copied` / `failed`), last_seen.
4. Persist progress in `~/.trove/sync.sqlite` `transfers` so interrupt can
   resume.

Retry with exponential backoff on transient store/IO errors (same spirit as
ADR 005).

#### Verify

`trove sync verify <mount>` (and optional playlist/query scope):

- For each expected track, confirm file exists, size matches, and preferably
  sha256 matches archive entry (sha256 may be optional behind a flag for speed).
- Mark stale/missing/failed in the volume DB.

#### Resume

`trove sync resume` continues incomplete transfers from `transfers` for the
active job / last mount; does not re-download `done` rows.

### 4. Per-volume database is mandatory for flash workflows

On `volume init` (or first sync to a recognized volume), open or create
`~/.trove/volumes/{volume_id}.sqlite` using `VOLUME_SCHEMA`. Sync/verify read
and write it.

CLI:

```
trove volume init <mount> [--label …]
trove volume list
trove volume status <mount>     # identity + summary counts from volume DB
trove volume diff <mount> [--playlist <name>|--query …]
```

`diff` reports present / missing / stale relative to the selected set (playlist
or query), without necessarily transferring.

### 5. CLI sync surface (genesis-aligned)

```
trove sync playlist <name> --to <mount> [--plan]
trove sync query --to <mount> [--artist …] [--bpm …] …
trove sync resume
trove sync verify <mount>
```

`--plan` dry-runs: print the plan and exit without writing. Default without
`--plan` executes then writes playlists for any playlist-sourced sync.

Query-sourced sync uses the same reconcile-then-query path as `trove query`.

### 6. Destination path vs content-addressed source

Download source is always the archive `object_key` (`music/<sha256>.<ext>`).
Destination is always the human layout. Mixing those two is forbidden:

- wrong: writing `Music/<sha256>.mp3` as the primary artist-album export
- right: `get(music/<sha>.mp3)` → write `Music/Artist/Album/Title.mp3`

### 7. Facade and daemon

Core methods (illustrative):

- `playlist_export(name, format) → PlaylistExport`
- `plan_volume_sync(…)` / `run_volume_sync(…)` / `resume_sync` / `verify_volume`
- `volume_list` / `volume_status` / `volume_diff`

Daemon exposes enough JSON for the UI flash workflow: plan, start, progress,
verify. Byte transfer can stay synchronous behind the existing mutex for the
first cut (same honesty as S3 bridge); progress may be polled via transfer
table / status endpoint.

### 8. Out of scope for Mixxx integration depth

This ADR does **not** invent a Mixxx database importer/exporter. Success
criterion: a mounted volume with `Music/` + `Playlists/*.m3u8` that Mixxx can
open via "import playlist" / library scan as operators already do. Deeper Mixxx
crate sync is future work.

## Consequences

### Positive

- Completes the genesis end-to-end loop: archive → playlist → USB → Mixxx.
- Keeps volumes dumb and portable; host DB remains disposable cache of volume
  state.
- Content-addressed buckets stay compatible with human USB layouts.
- Resume/verify make interrupted flashes operable on real gigs.

### Negative / trade-offs

- Full-object `get` into memory (or large temp files) until streaming download
  exists — acceptable for first flash, costly for huge albums at once;
  transfer concurrency can come later.
- Sparse metadata (filename stub extractor) produces weak `Artist/Album`
  folders until real tags or analyzers exist; export still works with
  "Unknown Artist" / path fallback.
- Volume path collisions need a policy; suffixing can surprise DJs who expect
  exact names.
- Dual state (`transfers` + volume DB) must stay consistent or resume gets
  confusing — prefer writing volume DB only after a successful file verify.

## Considered alternatives

- **Export using hash filenames onto the volume.** Rejected: unusable in Mixxx
  and on-console browsing; breaks "portable dumb drive" ergonomics.
- **Write Mixxx DB directly.** Rejected for now: couples Trove to Mixxx version
  internals; portable playlists are enough for genesis.
- **Only playlist export, no sync execution.** Rejected: without byte transfer
  the headline workflow is incomplete; planning-only already exists.
- **Store volume index on the drive.** Rejected by ADR 000: drives stay free of
  `.trove` DB folders; host-side per-volume SQLite remains correct.

## Scope

- **This ADR authorizes:** metadata-driven volume destination paths; `.m3u8` /
  `.m3u` export; sync plan → download → atomic write → volume DB updates;
  sync resume/verify; volume list/status/diff; CLI + facade (+ thin daemon)
  surfaces for the flash workflow.
- **Explicitly not in scope:** Mixxx DB writers; streaming multipart download
  resume (may share infra with ADR 005 follow-ups); analyzer-based name
  enrichment; durable import resume (ADR 005); compare-and-swap on the archive
  index; deleting unused files from volumes (`prune`) unless needed for a
  minimal `diff` story.

## Suggested sequencing relative to ADR 005

ADR 005 and ADR 006 are independent at the code seams. Recommended order when
implementing:

1. **ADR 005** if bulk backfill risk is the immediate production pain.
2. **ADR 006** if validating the DJ loop against a small archive is the
   immediate product need (small libraries already import fine without resume).

Either order is compatible; neither should block the other.
