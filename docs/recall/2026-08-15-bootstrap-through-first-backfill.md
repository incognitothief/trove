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

---

## Laptop perspective

> **DISCLAIMER — MACHINE OF ORIGIN:** Everything below was written on the
> **laptop** (2026-08-15). It does not edit or replace the desktop sections
> above. Paths such as `~/.trove` and `/Volumes/T72`, and the import-job
> counts below, describe **this laptop**, not the desktop.

- **Date recorded:** 2026-08-15
- **Period covered (laptop view):** 2026-07-08 through 2026-07-10
  (confirmed from local chats + `~/.trove` on 2026-08-15)
- **Kind:** cross-chat recap + local-state confirmation
- **Machine of origin:** laptop
- **Signed:** Composer

From this machine, the story after the desktop's failed first attempt is
different: the archive backfill was restarted for real S3, batch tooling was
built here, and the library walk mostly finished overnight. It is **not**
fully closed — one folder is still stuck.

### Sessions (laptop)

These are the chats that lived in this workspace after / alongside the
desktop bootstrap arc:

1. **Make / `bin/trove` wrapper** (2026-07-08) — `make cli` could not pass
   flags like `--plan` cleanly; added `trove/bin/trove` so import/query work
   from any cwd without fighting make.
2. **S3 + durable import arc** (2026-07-07–08, continued here) — ADR 003
   implementation, ADR 005/006 authorship and durable import wiring; early
   real imports against `/Volumes/T72/music/library/...`.
3. **Batch import script** (2026-07-09) — wrote
   `scripts/import-batch.sh` and documented it in runbook 02; fixed macOS
   bash empty-array `set -u` failure; required `CARGO_FEATURES=s3` at
   invoke time (build alone is not enough for `bin/trove`).
4. **Mid-backfill resume** (2026-07-09 evening) — batch run interrupted
   around artist folder `1600J`; added `--from` / `--after` (path-based) so
   the script can skip completed folders in C-locale sort order.
5. **Overnight continuation** (2026-07-09 → 2026-07-10, no chat) — batch
   continued through the alphabet; last successful job was
   `/Volumes/T72/music/library/yves-tumor` at `2026-07-10T04:12:02Z`.
6. **Status confirmation** (2026-08-15) — this laptop session scanned chats
   and `~/.trove` to answer whether the backfill completed.

### How far we got (laptop)

After config/S3 were correct on this machine, the backfill did run:

| Local state (`~/.trove` on laptop) | Count |
| --- | ---: |
| Import jobs `done` | 623 |
| Import jobs not `done` | 1 (`commit`) |
| Tracks in `archive.sqlite` | 7,640 |
| Files still `verified` (uncommitted) | 25 |
| Files `committed` across jobs | 7,640 |

Incomplete job:

- **id:** `53769506-2355-4660-87df-9f1077ae549f`
- **phase:** `commit`
- **source:** `/Volumes/T72/music/library/Teebs`
- **files:** 25 at `verified` (ready to commit)
- **last update:** `2026-07-10T02:42:25Z`

Interpretation: the batch script continues on folder failure, so Teebs
likely failed at commit while the walk kept going and finished later
folders through `yves-tumor`. The desktop note's "restart the backfill"
next action was the right call *then*; on this laptop that restart already
happened and is ~99% done.

### Next action (laptop)

Do **not** restart the whole library import. Finish the one stuck job, then
treat the initial backfill as closed:

1. Remount T72 if needed.
2. `CARGO_FEATURES=s3 bin/trove import status 53769506-2355-4660-87df-9f1077ae549f`
3. `CARGO_FEATURES=s3 bin/trove import commit 53769506-2355-4660-87df-9f1077ae549f`
4. Confirm with `import list` / `import list -- --all` that nothing remains
   outside `done`.

After that, product work returns to the deferred lane (metadata/analyzers,
export/USB polish, ADR 999 items) — not another full-library backfill.
