# Import runbook

Backfill audio from a local folder into the durable archive. Import is
**staged and resumable**: the canonical index advances only after
`verify → commit`.

Pipeline per file:

```text
scan → fingerprint (SHA-256) → dedupe → upload (staging) → verify → commit (music/) → push index
```

State is persisted in `~/.trove/sync.sqlite`. Job ids are UUIDs printed during
`plan` or the one-shot path.

`plan`, `commit`, and `resume` reconcile the local archive cache against the
bucket before their dedupe checks run (default on). This is what makes
duplicate detection actually archive-wide rather than only as good as
whatever this machine's cache already happened to contain — without it, a
fresh machine or a stale cache could mint a second archive entry for content
another machine already committed. Pass the global `--offline` flag to skip
this and serve the last cached index instead (same fallback semantics as
`archive pull-index --offline`); dedupe then only sees what's already local.

`plan` also keeps a persistent local fingerprint cache
(`~/.trove/fingerprint_cache.sqlite`), keyed by `(path, size, mtime)` and
independent of any job. Re-running `plan` against a path you've already
scanned — even in a brand-new job, not a resume — reuses the recorded SHA-256
instead of re-reading the file, as long as its size and modification time
haven't changed. This is separate from (and in addition to) the per-job
resume cache: that one only helps within a single interrupted-then-resumed
job; this one helps across completely separate `trove import` invocations,
which is the common case for a repeat backfill or re-scanning the same
drive. Purely a performance cache — deleting it is always safe, just costs a
future re-hash.

## Command reference

```bash
# Staged workflow (recommended for large libraries)
bin/trove import plan <path>              # scan/hash/dedupe → persist job
bin/trove import run <job-id>             # upload + verify (no commit)
bin/trove import verify <job-id>          # re-verify staged objects
bin/trove import commit <job-id>          # promote to music/ + push index
bin/trove import resume <job-id>          # continue from last safe state
bin/trove import status <job-id>          # phase + per-state counts
bin/trove import list [--all]             # find job ids
bin/trove import prune <job-id>           # drop local job state (not bucket objects)

# One-shot convenience (plan → run → commit)
bin/trove import <path>
```

### Import options

| Flag | Effect |
| --- | --- |
| `--include-dotfiles` | Include hidden files/dirs (off by default) |
| `--no-artwork` | Skip co-located cover art capture |
| `--artwork <path>` | Explicit cover image or folder (repeatable) |

Config defaults in `[import]` (`config.toml`) are merged with CLI flags.

## Happy path: small library (one-shot)

```bash
bin/trove import ~/Music/DJ-Crates/2024
```

Progress prints to stderr; final summary on stdout:

```text
job <uuid>: committed N track(s), captured M cover-art object(s)
```

Equivalent staged flow:

```bash
bin/trove import plan ~/Music/DJ-Crates/2024
bin/trove import run <job-id>
bin/trove import commit <job-id>
```

## Happy path: large backfill (staged)

For 100+ GB libraries, split `run` and `commit` so you can inspect staging
before advancing the index:

```bash
# 1. Dry-run friendly plan — review file list and duplicates
bin/trove import plan ~/Music

# 2. Upload + verify into staging (no index change yet)
bin/trove import run <job-id>

# 3. Check progress
bin/trove import status <job-id>

# 4. Promote when satisfied
bin/trove import commit <job-id>
```

`plan` output shows per-file state (`pending`, `duplicate`, etc.) and cover-art
candidates.

## Batch import (multiple folders)

Trove has no built-in globbing or batch import. For a tree of sibling folders
(e.g. one crate per year under `~/Music/DJ-Crates/`), use the helper script:

```bash
CARGO_FEATURES=s3 scripts/import-batch.sh ~/Music/DJ-Crates
```

For a local filesystem bucket (`region = "local"`), omit `CARGO_FEATURES=s3`.

The script runs one **one-shot** import per **immediate subdirectory** (not
recursive). Each folder gets the full `scan → upload → verify → commit` cycle.
Progress prints as `[N/total] importing …`; failures are logged and the script
continues with the remaining folders.

Forward import flags to every invocation:

```bash
scripts/import-batch.sh ~/Music/DJ-Crates -- --no-artwork
```

Exit code is the number of failed folders (0 when all succeed). Folders are
processed in C locale sort order (byte-wise, usually alphabetical).

### Resume a batch run

Two cases:

**1. A folder finished cleanly; you want the next folder onward**

Use `--after` with the **full path** to the last folder that completed. Copy it
from the `[N/total] importing …` line in your terminal output:

```bash
CARGO_FEATURES=s3 scripts/import-batch.sh \
  --after "/Volumes/T72/music/library/1600J" \
  /Volumes/T72/music/library
```

**2. A folder died mid-import (Ctrl-C, crash, error)**

Finish that folder's job first, then continue the batch:

```bash
CARGO_FEATURES=s3 bin/trove import list
CARGO_FEATURES=s3 bin/trove import status <job-id>
CARGO_FEATURES=s3 bin/trove import resume <job-id>
CARGO_FEATURES=s3 bin/trove import commit <job-id>
```

Then either `--after` that folder, or `--from` the folder that failed if you
want to re-run it from scratch:

```bash
CARGO_FEATURES=s3 scripts/import-batch.sh \
  --after "/Volumes/T72/music/library/Broken Artist" \
  /Volumes/T72/music/library
```

Re-running folders that already committed is safe (files show as `duplicate`)
but wastes time — prefer `--after` when you know the last success.

For a single deep tree, import the parent path directly instead:

```bash
bin/trove import ~/Music/DJ-Crates/2024
```

## Resume and recovery

### Interrupted one-shot import

```bash
bin/trove import list
# pick the job id for your source path

bin/trove import status <job-id>
bin/trove import resume <job-id>
# resume completes upload/verify; then:
bin/trove import commit <job-id>
```

`resume` continues fingerprinting if interrupted during plan, then runs
upload/verify. It does **not** commit — run `commit` explicitly after `resume`
when using the staged workflow.

### Lost the printed job id

```bash
bin/trove import list          # incomplete jobs (default)
bin/trove import list --all    # include finished jobs
```

### Crash mid-upload

Files already `uploaded` / `verified` are skipped on resume. Files stuck in
`uploading` are re-uploaded (multipart **part** checkpoint resume is not
implemented — the whole file restarts).

### Failed files

```bash
bin/trove import status <job-id>   # check failed count
bin/trove import resume <job-id>   # retries failed (non-terminal) files
```

Permanent failures remain `failed` until the source issue is fixed.

### Clean up local job bookkeeping

Does **not** delete bucket objects or revert committed tracks:

```bash
bin/trove import prune <job-id>
```

Prune only after the job is `done` or abandoned.

## Verify staging before commit

```bash
bin/trove import verify <job-id>              # presence + size (fast, default)
bin/trove import verify <job-id> --deep        # + re-download and re-hash (slow, thorough)
```

Default verification is presence + size via `head` only — cheap, but it
can't catch a truncated or bit-flipped upload that happens to land at the
right byte count. `--deep` re-downloads each staged object and compares a
fresh SHA-256 against the hash computed at fingerprint time — the real
content-addressed identity check, at the cost of reading every byte again.
Same trade-off applies to `bin/trove archive verify [--deep]` (see the
[archive runbook](01-archive.md)).

## JSON / automation

```bash
bin/trove --json import plan ~/Music
bin/trove --json import status <job-id>
bin/trove --json import list --all
```

Progress events are suppressed in `--json` mode (no stderr progress lines).

## What import does not do

| Topic | Status |
| --- | --- |
| Glob / native batch import | Use `scripts/import-batch.sh` for sibling folders |
| Rich metadata (artist, album, BPM) | `StubExtractor` — title from filename only |
| Mid-file multipart resume | Whole file re-uploaded on retry |
| Import manifest in bucket | Local `~/.trove/cache/manifests/` only |
| Delete tracks from archive | No CLI for removal |

## Troubleshooting

| Symptom | Action |
| --- | --- |
| All files `duplicate` | Already in archive (SHA-256 match) — expected |
| `commit` commits 0 tracks | Run `verify` first; check `status` for failed files |
| Source folder moved | Job stores absolute `source_root` — remount or re-plan |
| Out of memory on huge files | Known gap — whole file read into RAM before upload |

## See also

- [Archive runbook](01-archive.md) — `pull-index` after manual recovery
- [ADR 005 — durable import](../adr/005-durable-import-and-resume.md)
- [ADR 004 — content-addressed objects](../adr/004-content-addressed-audio-objects.md)
