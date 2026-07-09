# ADR 005: Durable Import State and Resume

- **Status:** Proposed
- **Date:** 2026-07-08
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000](000-bootstrap.md), [ADR 001](001-bootstrap-implementation.md), [ADR 004](004-content-addressed-audio-objects.md)

## Context

[ADR 000](000-bootstrap.md) treats the first (and large) import as a **resumable
bulk migration**, not a one-shot upload. The guiding rule is: never make success
implicit. Every file carries an explicit state; the canonical archive index only
advances after verify → commit. Splitting `plan` / `run` / `verify` / `commit`
lets a 100–500 GB backfill be interrupted without leaving the archive ambiguous.

[ADR 001](001-bootstrap-implementation.md) scaffolded the phase machine and the
`import_jobs` / `import_files` schema in `~/.trove/sync.sqlite`, but left import
bookkeeping **in memory**. A crash mid-job restarts from scratch — the opposite
of ADR 000's headline guarantee. That gap was listed as follow-up #1.

Today (post ADR 004):

- The pipeline `scan → hash → dedupe → upload(staging) → verify → commit(music/) → push index` runs end-to-end.
- Canonical keys are content-addressed (`music/<sha256>.<ext>`).
- Dedupe is archive-wide by SHA-256 at plan time.
- The CLI is still a single `trove import [--plan]` convenience path; `--bulk`
  / `--resume` flags exist but do not wire into durable state.
- Uploads of large buffers use multipart, but part state is not persisted and
  whole files are still read into memory before `put`.

This ADR authorizes closing the durability gap: **persist import state, expose
the resume-safe CLI surface, and make crashes resume rather than restart.** It
does not redesign content addressing (ADR 004) or the staging→commit layout.

## Decision

### 1. Persist import jobs to `~/.trove/sync.sqlite`

Use the existing `import_jobs` and `import_files` tables as the source of truth
for in-flight work. Opening or creating a job always goes through this DB; the
in-memory `ImportJob` becomes a loaded view of those rows, not the authority.

**Job row (`import_jobs`):** job id, source root, phase, staging prefix, total
file count, created/updated timestamps. Optionally extend with option flags
(`include_dotfiles`, `capture_artwork`) so resume recreates the same behavior.

**File row (`import_files`):** path, size, mtime, sha256, state, staging object
key, etag (opaque), error message, and enough fields to skip rework:

| Prior state on resume | Action |
| --------------------- | ------ |
| `pending` / `scanning` | Rescan or re-hash as needed |
| `hashed` | Proceed to upload (skip hash if size+mtime+sha still match source) |
| `duplicate` | Skip |
| `uploading` | Treat as incomplete; re-upload (or resume multipart when available) |
| `uploaded` / `verified` | Skip upload; continue from verify/commit |
| `committed` | Skip |
| `failed` | Retry on `resume` unless operator opts out |

**Skip-rehash rule:** if a file row already has `sha256`, and the on-disk
`size` + `mtime` still match the recorded values, do not re-read the whole file
just to re-hash on resume. If size/mtime diverge, re-hash and replace the row.

### 2. Commit remains last; index advances only after successful commit

No change from ADR 000: a crash before `commit` leaves the bucket index
untouched. On resume:

1. Reload the job from SQLite.
2. Advance only files that are not yet terminal (`duplicate` / `committed` /
   optionally keep `failed` for retry).
3. Re-enter the earliest incomplete phase (upload → verify → commit).
4. Call `push_index` only after commit succeeds for newly committed files
   (same as today's facade).

Artwork capture remains part of the post-commit durable path (ADR 002); resume
must not re-upload identical content-addressed art objects.

### 3. Split the CLI surface (and keep a convenience path)

Expose the ADR 000 command shape as first-class (thin over core), plus a job
history command so a lost job id is recoverable from local state:

```
trove import plan <path>              # scan/hash/dedupe → persist job (dry-run friendly)
trove import run <job-id>             # upload + verify (no commit yet — see below)
trove import verify <job-id>          # re-verify staged objects
trove import commit <job-id>          # promote to music/ + capture art + push_index
trove import resume <job-id>          # continue from last safe state
trove import status <job-id>          # per-phase / per-state counts
trove import list [--all]             # history of import jobs (id, source, phase, counts, times)
trove import prune <job-id>           # drop local job bookkeeping (see addendum)
trove import <path> [--bulk]          # convenience: plan + run + commit, resumable mid-flight
```

**`import list` is load-bearing for resume.** Job ids are UUIDs; without a
history surface, interrupting a one-shot `import <path>` and losing the printed
id leaves the operator unable to call `resume`. Default `list` shows recent /
incomplete jobs (phase not `done`, or done-but-failed files remaining);
`--all` includes finished jobs. Output must include at least: job id, source
root, phase, file totals (and preferably committed/failed/duplicate counts),
created_at / updated_at. That is enough to pick a row and run
`trove import resume <job-id>` or `status <job-id>`.

**Phase boundary for `run` vs `commit`:** prefer the split genesis intended —
`run` uploads and verifies into staging; `commit` promotes and advances the
index. The one-shot `trove import <path>` continues to mean plan→run→commit for
operators who want a single command; if interrupted, `trove import list` then
`trove import resume <job-id>` picks up.

Replace the current ad hoc `import --plan` / ignored `--resume` flags with this
subcommand layout (or an equivalent clap structure that matches the above
verbs).
### 4. Retry with exponential backoff on transient store errors

During upload (and optionally verify/`CopyObject`), transient `ObjectStore`
failures retry with exponential backoff and jitter, with a capped attempt count
persisted on the file row (`attempts` or reuse `error` + counter). Permanent
failures mark `failed` without advancing the job phase if any file remains
incomplete for that phase.

Exact backoff schedule is an implementation detail; the requirement is that a
flaky network does not fail the whole job after one error.

### 5. Checksum / verification bar for this ADR

Minimum bar (aligned with today, made durable):

- After upload: `head` confirms size (and presence).
- Persist opaque `etag` when the store returns one.
- Identity remains the **client-side SHA-256** (ADR 004); do not trust S3 ETag
  as a content hash.

### 5a. Why multipart *part* resume is out of scope (not multipart itself)

Important distinction:

- **Multipart uploads already exist** in `S3Store` (ADR 003): large buffers are
  chunked into `UploadPart`s for a single `put`. That stays.
- What this ADR **does not** include is **checkpointed multipart resume**:
  persisting upload-id + completed part numbers so a crash *mid-file* can
  continue the same multipart upload without resending finished parts.

That deeper resume is deferred on purpose:

1. **Different problem, different seam.** Job/file durability lives in SQLite and
   works with the existing byte-oriented `ObjectStore::put`. Mid-file resume
   needs an upload API that is streaming and checkpointable (upload session
   state, part ETags) — an evolution of the trait / store API, not just more
   rows in `import_files`.
2. **Land the bigger win first.** For a 100–500 GB library with many files, a
   crash mid-backfill losing *all* progress is catastrophic. Losing *one*
   in-flight large file's partial parts and re-uploading that single object is
   painful but acceptable once every other completed file is skipped on
   `resume`. Job-level durability unlocks the migration; part-level polish is
   optimization for giant single-object uploads.
3. **Whole-file restart is still safe.** With durable `import_files`, an
   interrupted `uploading` file is marked incomplete and **re-uploaded from
   scratch** on resume (reusing the planned staging key / sha). Staging +
   verify + commit still prevent the archive index from referencing a partial
   object.

Follow-up after this ADR: streaming-from-disk `put` + persisted multipart
session state. Not required before we call import "resumable" in the ADR 000
sense (resume the *job* without rescanning/rehashing/re-uploading finished
files).
### 6. Dry-run and manifest

- `import plan` (or `--plan`) writes/persists the job at `dedupe` and prints
  counts without uploading.
- After plan (and optionally after commit), emit a machine-readable import
  manifest under `~/.trove/cache/manifests/` (or bucket `.trove/manifests/` as
  genesis sketched) listing job id, source root, per-file sha256/state/keys.
  Exact path is an implementation detail; the requirement is an inspectable
  artifact operators can keep for an interrupted migration.

### 7. Core API shape

Extend `trove-core` (facade + `import` module) so clients do not invent SQL:

- `import_plan` → persists and returns job id + stats
- `import_run` / `import_verify` / `import_commit` / `import_resume` by job id
- `import_status` → structured stats (totals by `FileState`, current phase)
- `import_list` → job summaries for history / rediscovery after losing a printed id

Daemon HTTP routes should mirror these for the UI progress surface (minimal
`GET /import/:id/status` and `GET /import` for list are enough for a first UI
pass).
## Consequences

### Positive

- Crash mid-import no longer implies a full rehash of a library.
- The CLI matches the genesis mental model (`plan` → `run` → `commit` /
  `resume`).
- Staging→commit remains the only path that advances the archive index.
- Builds on existing schema and `FileState` machine instead of inventing a new
  store.

### Negative / trade-offs

- More SQLite write churn during import (every state transition); acceptable
  for durability.
- Until streaming/part-checkpoint multipart lands, very large files can still
  force full re-upload after a crash mid-`put` — but job-level progress is
  preserved for all other files.
- CLI migration: operators using `trove import --plan` need the new subcommands.
- Convenience one-shot import and the explicit multi-step flow both must be
  kept correct so they do not diverge.

## Considered alternatives

- **Keep in-memory only and tell operators to "run smaller batches."** Rejected:
  contradicts ADR 000 for the 100–500 GB case.
- **Persist only completed jobs / manifests, not live state.** Rejected: resume
  still requires redoing expensive hash/upload work for in-flight files.
- **Put durable import state in the bucket.** Rejected for now: local
  `import_files` is disposable bookkeeping; staging objects already live in the
  bucket. Host SQLite matches ADR 000 and keeps credentials/offline work local.
- **Streaming + multipart *part* checkpoints in the same ADR.** Deferred: that
  needs a richer upload seam than `put(&[u8])` (see §5a). Job durability first
  so progress is never lost at the file/job level; a single interrupted object
  may still re-upload in full.
- **Discoverability only via printed UUID / manifest files.** Rejected: operators
  lose terminal scrollback. `import list` against `import_jobs` is the reliable
  rediscovery path.

## Scope

- **This ADR authorizes:** wiring `import_jobs` / `import_files` into the live
  pipeline; resume that skips done work using size/mtime/sha short-circuit;
  split CLI (`plan` / `run` / `verify` / `commit` / `resume` / `status` /
  **`list`**) plus a one-shot convenience; retry/backoff on transient store
  errors; dry-run plan and an import manifest; thin facade/HTTP surface for
  status and job history.
- **Explicitly not in scope:** streaming-from-disk uploads; multipart **part**
  checkpoint resume (multipart chunking for large `put`s already ships —
  ADR 003); analyzer toolkit; compare-and-swap on `schema-version.json`
  (still ADR 001 follow-up #3); real metadata extraction; volume sync/export
  (ADR 006).

---

## Addendum: Implementation refinements (2026-07-08)

The core ADR 005 surface shipped in the same session. The following
refinements and bug fixes were discovered during real-library testing and
are recorded here so operators and future implementers do not have to reverse
engineer behavior from git history.

### Single-file import

`import plan <path>` and the one-shot `import <path>` now accept **either**
a directory or a **single audio file**. Previously, scan treated a non-directory
path as empty, producing jobs with `total_files=0` that could still advance
through `run` / `commit` without importing anything.

**Operator implication:** a broken pre-fix job stuck at `phase=commit` with
`files=0` did not touch the archive but still appeared in `import list`. Start
a new `plan` with the fixed binary; do not `resume` the ghost job.

### Cover art policy (single file vs directory)

Co-located cover capture (ADR 002) applies **by default only to directory
imports**. When the source is a single audio file, artwork is **not** gathered
from the file's parent folder — that avoids pulling unrelated images from shared
folders (e.g. `~/Downloads`).

| Source | Default artwork | Opt-out | Opt-in |
| ------ | --------------- | ------- | ------ |
| Directory | Co-located images in each track's parent folder | `--no-artwork` | — |
| Single file | None | (already off) | `--artwork PATH` |

**`--artwork PATH`** names an explicit image file or folder to scan
(non-recursive). It does **not** re-enable implicit parent-folder scanning for
single-file imports. Example:

```bash
trove import plan Album/01.flac --artwork Album/cover.jpg
```

Repeatable for multiple paths. Values persist on the job row as `artwork_paths`
(JSON) so `resume` recreates the same behavior. `--no-artwork` disables the
automatic directory behavior only; it does not block explicit `--artwork`
paths.

### Job history and `import prune`

**When a job leaves the default queue (`import list` without `--all`):**

- After `commit` sets `phase=done`, **and**
- No file rows remain in `failed` state.

Jobs that finished successfully stay in SQLite until pruned; use `import list
--all` to see them. Jobs with failures remain visible even at `done`.

**`import prune <job-id>`** removes local bookkeeping only: the row in
`~/.trove/sync.sqlite` (cascades `import_files`) and
`~/.trove/cache/manifests/<job-id>.json`. It does **not** delete staging or
canonical bucket objects or undo committed archive entries. Use it to drop
ghost or abandoned jobs from the operator queue. HTTP mirror: `DELETE
/import/:id` → `{ "pruned": "<id>" }`.

### Progress output (CLI)

Import steps emit structured progress events (core `ImportProgress` hook).
Human CLI mode prints line-by-line to stderr:

- `job_id:` is emitted **before** fingerprinting begins (so the id is copyable
  immediately).
- Phase headers use **present-tense verbs** for readability:
  `fingerprinting`, `uploading`, `verifying`, `committing` (via
  `Phase::progress_label()`). Machine-facing phase strings in status, JSON, and
  SQLite remain the short nouns (`fingerprint`, `upload`, …).

Fingerprinting persists the job shell and per-file rows incrementally so
Ctrl+C during plan can be resumed with `import resume` (completes fingerprint
before upload).

### Schema extensions (additive)

Beyond the columns sketched in §1, live `import_jobs` also stores:

- `artwork_paths TEXT` — JSON array of explicit `--artwork` paths (nullable).
- `artwork_json TEXT` — serialized artwork candidates after plan.
- `attempts` on `import_files` — upload retry counter.

Migrations are additive (`ALTER TABLE … ADD COLUMN`) for existing
`sync.sqlite` files.

### Build / S3 testing note

Real S3 import requires building with the `s3` feature on every invocation,
e.g. `CARGO_FEATURES=s3 bin/trove …` or `make cli CARGO_FEATURES=s3 …`. The
Makefile and `bin/trove` wrapper pass `CARGO_FEATURES` through consistently.
