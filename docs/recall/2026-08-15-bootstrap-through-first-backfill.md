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

---

## Cross-reference (desktop, 2026-08-15)

> **DISCLAIMER — MACHINE OF ORIGIN:** This section was written on the
> **desktop computer** after the laptop addendum was merged into this note and
> `feature/scripting` was merged into `feature/recall`. It does not replace
> either perspective above. Local job counts, `~/.trove`, and T72 paths remain
> **host-specific**. The bucket was **not** queried for this recap.

- **Date recorded:** 2026-08-15
- **Kind:** cross-machine recap of the same backfill episode + scripting merge
- **Machine of origin:** desktop computer
- **Repo state:** `feature/recall` includes `origin/feature/scripting`
  (`b9baafa` scripts, `197f6ef` script enhancements)

This is one episode seen from two machines, not two separate backfills.

### Two perspectives of the same episode

| | Desktop (this machine) | Laptop |
| --- | --- | --- |
| When | 2026-07-07 through 2026-07-09 | 2026-07-08 through 2026-07-10 (confirmed 2026-08-15) |
| What happened | First import hit silent local-simulator fallback; internal SSD filled; `~/.trove` was **trashed** | Restarted against real S3; batch-walked artist folders on T72; overnight run through `yves-tumor` |
| Local evidence | Discarded. This desktop has no surviving import-job DB from that attempt | Laptop `~/.trove`: 623 jobs `done`, 1 job in `commit` (`Teebs`, 25 files `verified`), 7,640 tracks in `archive.sqlite` |
| Stated next action then | Restart the whole-library backfill with valid `name` + `region` | Do **not** restart; commit job `53769506-2355-4660-87df-9f1077ae549f` |

From the desktop's July 9 vantage, restarting was the right next step. From
the laptop's later vantage, that restart already ran. The laptop work is the
**offline continuation** the desktop never saw.

### What `feature/scripting` actually added

Merged here without executing it:

- `scripts/import-batch.sh` — one one-shot `bin/trove import` per **immediate**
  subdirectory of a parent path (C-locale sort). Failures are logged; the walk
  continues. That matches the laptop story that Teebs could stall at commit
  while later folders still finished.
- Resume is path-based: `--from PATH` (inclusive) / `--after PATH` (exclusive).
  The later enhancement (`197f6ef`) requires a **canonical full path** that is
  a direct child of the parent, not a basename.
- Runbook 00: `CARGO_FEATURES=s3` must be set **at invoke time** for
  `bin/trove` / the batch script. A prior `make build CARGO_FEATURES=s3` is not
  enough.
- Runbook 02: documents the batch helper as the glob/multi-folder workaround.

### Speculative reading (do not treat as bucket truth)

Local SQLite (`archive.sqlite`, `sync.sqlite`) is a **best-effort cache**.
Reads are supposed to reconcile against the bucket; this recap did **not**
call `archive pull-index` or inspect S3. Until the cloud index is checked:

- The laptop's 7,640 committed / 623-done figures **might** match the bucket,
  or they might be ahead, behind, or host-only. They are not canonical.
- The Teebs job at `commit` with 25 `verified` files **might** mean those
  objects are already in staging (or even `music/`) and only the local index
  push is unfinished — or the job never landed remotely. Unknown without the
  bucket.
- Desktop `~/.trove` was discarded, so this machine cannot confirm or deny the
  laptop counts. Rehydrating this desktop would mean pulling the **bucket**
  index, not copying the laptop DB.
- Re-running `import-batch.sh` over folders the laptop already committed
  **should** be safe via SHA-256 dedupe **if** those objects are in the
  archive; that "if" is exactly what we have not verified.

### Next action (desktop view, still speculative)

Do **not** start another full-library backfill from this desktop on the
strength of either note. The likely operator step, after a bucket check (not
done here), is:

1. Reconcile this machine from the cloud (`archive pull-index` with
   `CARGO_FEATURES=s3`) — trust that over any host SQLite.
2. If the bucket agrees Teebs is the hole, finish that job from a machine
   that still has the job id in `sync.sqlite` (probably the laptop), or
   re-import `/Volumes/T72/music/library/Teebs` and let dedupe no-op the rest.
3. Only then treat the initial backfill as closed and return to ADR 999
   follow-ups.

Related: [Import runbook — batch](../runbook/02-import.md#batch-import-multiple-folders),
[ADR 000 — bucket is source of truth](../adr/000-bootstrap.md),
[ADR 999](../adr/999-known-gaps-and-follow-ups.md).
