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
bin/trove import verify <job-id>
```

Re-runs head/size checks on staged objects. Verification is **weak** today:
presence + size via `head` only — no re-download SHA-256 compare (known gap).

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
