# Bootstrap through first backfill

> **DISCLAIMER — MACHINE OF ORIGIN:** This note was written on the **desktop
> computer**. It is not from a laptop, a remote host, or any other machine.
> Paths such as `~/.trove` and `/Volumes/T72`, disk-full behavior, and the
> discarded local cache describe **this desktop**, not another device.

- **Date recorded:** 2026-08-15
- **Period covered:** 2026-07-07 through 2026-07-09
- **Kind:** cross-chat recap (four working sessions)
- **Machine of origin:** desktop computer (see disclaimer above)

Trove is past bootstrap and into the first real archive backfill. That backfill
has not finished.

## Sessions

1. **Project design ADR conversion** (2026-07-07) — turned the design transcript
   into [ADR 000](../adr/000-bootstrap.md).
2. **Genesis ADR bootstrap** (2026-07-07–08) — scaffolded the workspace from
   ADR 000; recorded [ADR 001](../adr/001-bootstrap-implementation.md); planned
   and implemented the near-term slice of
   [ADR 002](../adr/002-import-enhancements-and-analyzer.md).
3. **Command line interface runbook** (2026-07-08) — wrote operator runbooks for
   every CLI family; closed the runbooks gap in
   [ADR 999](../adr/999-known-gaps-and-follow-ups.md).
4. **Disk bloat issue** (2026-07-09) — first real backfill attempt; silent local
   fallback filled the Mac SSD; cache was discarded; prerequisites runbook was
   updated with required vs optional `config.toml` fields.

## How far we got

### Design and bootstrap

ADR 000 is the genesis architecture: the bucket is the only durable source of
truth; all domain logic lives in `trove-core`; the CLI, `trove-serverd`, and
React UI are thin clients. The workspace was scaffolded (Rust crates, Makefile,
`make dev`, `AGENTS.md`) with a first retrospective in ADR 001.

### Core product (shipped)

- Durable import with `plan` / `run` / `resume` / `status` / `verify` / `commit`
  ([ADR 005](../adr/005-durable-import-and-resume.md))
- Content-addressed audio objects
  ([ADR 004](../adr/004-content-addressed-audio-objects.md))
- Feature-gated S3 plus a local filesystem bucket simulator
  ([ADR 003](../adr/003-s3-object-store.md))
- Playlist flash to a volume, Mixxx `.m3u8`, `sync resume` / `verify`
  ([ADR 006](../adr/006-export-playlist-and-volume-sync.md))
- Import extras: skip hidden/non-music files by default; capture co-located
  cover art (ADR 002 near-term slice; name/artwork analyzers still deferred)
- Operator runbooks under `docs/runbook/`, including a required-vs-optional
  `config.toml` write-up after the disk-full incident
  ([00-prerequisites.md](../runbook/00-prerequisites.md))

### First real backfill (not done)

Started importing `/Volumes/T72/music/library` (~139 GB, ~7,618 files).
Incomplete `~/.trove/config.toml` (`name` without `region`) was silently ignored;
Trove fell back to the local bucket simulator on the Mac SSD. Staging filled the
internal disk at ~30% (~2,297 files). SQLite and `cargo` then failed for lack of
space. The `~/.trove` cache was discarded rather than resumed.

Footgun: a config file that exists but is invalid does not error; it falls back
to `region = "local"` and copies the library into `TROVE_BUCKET_DIR` (default
`~/.trove/bucket-sim`).

### Explicitly deferred

Tag extraction / analyzers, `archive verify`, compare-and-swap on
`schema-version.json`, streaming / multipart part resume, UI flash-drive
workflow, and import regex / multi-path batching. See ADR 999.

## Next action

Restart the **archive backfill**, with config and disk placement fixed first.
ADR 999 still lists that as priority 1. Metadata, analyzers, and USB polish wait
until the library is in the bucket.

1. Write a valid `~/.trove/config.toml` with **both** `name` and `region`.
2. Pick a backend that can hold ~139 GB:
   - real S3: complete bucket config + `CARGO_FEATURES=s3`, or
   - local simulator: `name = "local"` / `region = "local"` and
     `TROVE_BUCKET_DIR` on T72 (or another large volume), not the boot drive.
3. Re-import. One tree is enough
   (`bin/trove import plan /Volumes/T72/music/library`), or chunk by subfolder
   in a shell loop. Import still takes one path at a time; there is no
   regex/glob.

The disk-bloat chat ended on questions only (chunking, then parallelism). No
code change was requested after the runbook 0 update.

## Related

- [ADR 999 — known gaps and follow-ups](../adr/999-known-gaps-and-follow-ups.md)
- [Import runbook](../runbook/02-import.md)
- [Prerequisites / config.toml](../runbook/00-prerequisites.md)
