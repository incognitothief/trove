# Import runbook

Backfill audio from a local folder into the durable archive. Import is
**staged and resumable**: the canonical index advances only after
`verify → commit`.

Pipeline per file:

```text
scan → slug+size fast path → fingerprint cache → fingerprint (SHA-256) → dedupe → upload (staging) → verify → commit (music/) → push index
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

If a [library root](07-library.md) is declared, `plan` checks it *before*
the fingerprint cache: a file whose path (relative to the root) and size
match an already-archived entry is marked `duplicate` without reading the
file at all — not even a cache lookup. This is what makes re-pointing
import at a cloned or replacement drive (same relative layout, different
mount point) cheap instead of a full re-hash of the whole library. Plan
output labels *why* a file was recognized as a duplicate:

```text
  duplicate  /Volumes/T72/Artist/Track.mp3  (slug+size, not re-hashed)
```

versus a file whose bytes were actually read and hashed against the
archive:

```text
  duplicate  /Volumes/T72/Artist/Other.mp3  (hash-confirmed)
```

`--json` plan output carries the same distinction as a `duplicate_reason`
field (`"slug+size, not re-hashed"`, `"hash-confirmed"`, or `null` for a
duplicate within the same scan that isn't archived yet). A slug match with
a *different* size is never trusted — that's treated as a genuine
replacement file at the same catalog position, not a duplicate, and falls
through to a real hash.

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

For a tree of sibling folders (e.g. one crate per year under
`~/Music/DJ-Crates/`), use the [`library` command family](07-library.md)
instead of running `import` by hand per folder. This replaced the old
`scripts/import-batch.sh` wrapper (ADR 007, Group E5) — the same job, but
durable, resumable without copy-pasting a folder path out of terminal
output, and coordinated correctly across more than one machine working the
same library.

```bash
bin/trove library root --set ~/Music/DJ-Crates      # 1. declare the root
bin/trove library shape                              # 2. see real structure/size first
bin/trove library plan create                         # 3. push a durable chunk plan
bin/trove library plan claim <plan-id>                 # 4. claim + import chunks, repeatedly
```

Each `plan claim` runs one ordinary `scan → upload → verify → commit` cycle
per chunk (by default, one immediate subdirectory) and picks the next
untouched chunk automatically — just keep re-running it:

```bash
bin/trove library plan claim <plan-id>
bin/trove library plan claim <plan-id>
# ... repeat until it refuses with "no untouched chunks remain"
```

Forward import flags the same way `import` accepts them:

```bash
bin/trove library plan claim <plan-id> --no-artwork
```

### Resume a batch run

No folder paths to copy-paste — pull status and let pick-next handle it:

```bash
bin/trove library plan status <plan-id>
# → per-chunk completed / claimed / untouched

bin/trove library plan claim <plan-id>
# → automatically picks the next untouched chunk
```

A chunk that died mid-import (Ctrl-C, crash, error) never got a `completed`
event, so it's still `claimed` in `plan status` — a stale-claim hint, not a
lock. Re-run it explicitly by chunk id, or opt into picking it up
automatically:

```bash
bin/trove library plan claim <plan-id> <chunk-id>     # a specific chunk
bin/trove library plan claim <plan-id> --include-claimed  # first non-completed chunk
```

Re-running a chunk that already committed is safe and cheap — the
slug+size fast path (D3) recognizes already-archived content without a
full re-hash, so redundant work doesn't cost much. See the
[Library runbook](07-library.md) for the full command family, including
`--chunk-folders` (group more than one subdirectory per chunk) and the
chunk-level stat sanity check.

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
| Glob / native batch import | Use `library plan create` + `library plan claim` for sibling folders — see [Batch import](#batch-import-multiple-folders) above |
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
