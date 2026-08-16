# ADR 007: Desktop Shell, IO Durability, and Multi-Device Backfill Coordination

- **Status:** Proposed
- **Date:** 2026-08-16
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000](000-bootstrap.md), [ADR 001](001-bootstrap-implementation.md), [ADR 004](004-content-addressed-audio-objects.md), [ADR 005](005-durable-import-and-resume.md), [ADR 006](006-export-playlist-and-volume-sync.md), [ADR 999](999-known-gaps-and-follow-ups.md)

## How to read this ADR

This document grew from a single pivot decision (Tauri) into the full design
for everything multi-device usage exposed while working through it. It is
written to be picked up cold — by a different session, a different agent, or
the maintainer weeks later — so every decision below cites the exact file and
line it touches today, and the [Implementation sequencing](#implementation-sequencing--hand-off)
section tracks status explicitly rather than assuming context. Nothing in this
ADR is implemented yet; it is a design authorized for building, not a record
of work already done. Check that section, not memory, for what's actually
landed.

## Context

### Why Tauri (original scope)

[ADR 000](000-bootstrap.md) chose a React web UI over a local HTTP daemon
(`trove-serverd`) as the genesis UI path, but explicitly reserved a different
packaging as a future option rather than ruling it out:

> packaging the React UI and `trove-core` together as a single Tauri desktop
> app is appealing and would let the UI call core directly instead of over
> HTTP. We are deliberately not doing this now; the thin-client boundary above
> keeps that option open without committing to it.

Two things changed since genesis:

1. **Real usage is multi-device, not single-machine-web.** Trove is meant to
   run on more than one physical machine (home rig, gig laptop), rebuilding
   from the bucket each time. A `git clone` + `cargo build` + `npm install` +
   two dev servers setup is friction every device pays; a packaged installable
   binary is not.
2. **The top open UI gap is platform-blocked, not backend-blocked.** Per
   [ADR 999](999-known-gaps-and-follow-ups.md), the flash-drive workflow
   (`sync query --to`, volume UI) is the highest-priority missing surface, and
   it needs arbitrary local folder access and mounted-volume enumeration — a
   browser sandbox cannot do either. `trove-core` and the CLI already support
   this; only native UI access is missing.

### Why this expanded past Tauri

Leaning into multi-device usage as the reason for this pivot forced a closer
look at what actually happens when more than one machine touches the same
archive, and that surfaced a chain of related gaps, each discovered while
tracing the previous one through the real code:

1. **The canonical index can race.** `Trove::push_index` reads the remote
   generation, then issues two unconditional writes with no atomicity across
   the read and the writes (`facade.rs:602-619`). Two machines committing
   near-simultaneously can silently clobber one another's index write. →
   [Group B](#group-b--archive-index-durability).
2. **Archive-wide dedupe is only as good as the local cache, and nothing warms
   it.** `import_plan`/`import_run_full` never call `self.reconcile()` (unlike
   `query()`, which explicitly does — `facade.rs:106-108`). A cold local cache
   — a fresh machine, a wiped `~/.trove`, or simply not having pulled since
   another device committed the same content — makes the dedupe check at
   `import/mod.rs:242-247` and `import/mod.rs:662-670` miss even when the
   bucket already has the object, minting a second `ArchiveEntry`/`TrackId`
   for content that's already archived. ADR 999's own note that "dedupe and
   resume work on content hash" (under the metadata-backfill section) is an
   overclaim once traced this closely — it's true only when the local cache
   happens to be warm. → [Group C](#group-c--import-cost-and-cache-correctness).
3. **Re-scanning known content is always full price.** `can_skip_fingerprint`
   (`import/mod.rs:291-300`) only skips re-hashing within a *resumed job*
   (`resume_job_id` populated, `import/mod.rs:155-163`). A brand-new
   `trove import` invocation against a path you've already fully imported —
   even byte-for-byte unchanged — always re-reads and re-hashes every file.
   There is no path-keyed cache that survives across separate invocations. →
   [Group C](#group-c--import-cost-and-cache-correctness).
4. **The same library on a replacement drive has no cheap identity path.**
   Content-addressing (ADR 004) correctly treats path as irrelevant to *what a
   file is*, but that means recognizing "this is the library I already have,
   just remounted somewhere else" costs a full re-hash of everything, every
   time — genuinely slow for a large library on a replacement/cloned drive. →
   [Group D](#group-d--cross-drive-content-identity).
5. **The operator experience for a large, possibly multi-day, possibly
   multi-machine backfill is a bash workaround, not a designed capability.**
   [`scripts/import-batch.sh`](../../scripts/import-batch.sh) chunks at exactly
   one directory level (`find … -maxdepth 1 -type d | sort`), with zero
   visibility into subtree size/shape before running, and its `--after`/`--from`
   resume flags require copying exact folder paths out of prior terminal
   output. Worse: its ordering has nothing to do with the *actual* traversal
   order `trove-core` uses internally — `discover_audio`/`scan_dir`
   (`import/mod.rs:811-855`) recurses the whole subtree and
   `discovered.sort()` (`import/mod.rs:190`) does one global path sort across
   it, a completely different, invisible-until-it-runs ordering. This isn't
   just rough UX: the chunking/ordering/resume logic living entirely in bash,
   outside `trove-core`, is a direct instance of the thing `AGENTS.md`'s "one
   architectural rule" warns against — *"If you find yourself writing
   reconcile/query/sync/import logic in a client, move it into the core."* A
   Tauri UI has no way to expose this workflow without either shelling out to
   bash (fragile, defeats the direct-core-command IPC this ADR chose) or
   re-implementing chunking a second time in TypeScript (exactly the
   duplicated-logic failure ADR 000 rejected outright). → [Group E](#group-e--backfill-coordination-and-operator-ux).

Each of these is a consequence of the same root cause this ADR is already
about — multi-device usage was always the premise, and the premise turned out
to touch more of the system than the index alone. They're grouped below in
dependency order, not arbitrary order: each group is buildable once the ones
before it exist, and each is independently useful even if later groups slip.

## Decision

### Group A — Desktop shell

**A1. Tauri is the desktop shell; the UI calls `trove-core` directly.** The
existing React UI (`ui/`) is bundled with `trove-core` in a Tauri shell. UI
actions become `#[tauri::command]` calls into `trove-core` (in-process Rust
IPC), not HTTP requests to a local daemon — removing the "client/daemon
boundary cost" ADR 000 named as the price of the HTTP path. Existing UI code
carries over largely unchanged: [`ui/src/api.ts`](../../ui/src/api.ts)'s typed
request wrapper is replaced with typed Tauri `invoke()` calls; the domain
types (`ArchiveEntry`, `Playlist`, `QuerySpec`, …) and the component code in
[`App.tsx`](../../ui/src/App.tsx) are unaffected.

**A2. `trove-serverd` is retired from the primary path — parked, not deleted.**
The daemon crate stays in the workspace, keeps building, keeps its tests, and
stays in `make check`. It simply stops being how the UI talks to core. No new
HTTP routes are added going forward (the flash/volume routes ADR 999 lists as
missing will **not** be built there); existing routes keep compiling against
`trove-core`'s public API and get fixed if a core signature change breaks
them, but receive no new feature work. Kept rather than deleted because it's
working, tested code, and because headless/remote access (Trove reachable over
a network rather than run locally per-device) is a plausible future need this
pivot doesn't resolve either way. If `trove-core`'s API drifts far enough that
keeping it compiling becomes a real, recurring cost rather than incidental
breakage, that's grounds for a *future* ADR to decide on deletion — not a
decision made here.

### Group B — Archive index durability

**B1. Compare-and-swap protecting the canonical index — not just the marker.**
This unit was originally scoped as "CAS on `schema-version.json`" alone. That
is wrong, caught in review before implementation: `schema-version.json` is a
small pointer, but the actual payload — `archive-index.jsonl` — lives at a
single, fixed, never-generation-scoped key (`archive_index_jsonl()`,
`index.rs:39-41`) and would still be written with a plain, unconditional
`store.put` (`facade.rs:605`) even after CAS-ing the pointer. Protecting the
pointer alone does not protect what it points to: A and B can both read
marker generation N, both write *different* content to the *same*
`archive-index.jsonl` key, and whichever `put` physically lands last wins
that object's content — independent of which machine's marker CAS succeeds.
Whoever wins the marker race can end up pointing at the *other* machine's
index content, silently dropping entries neither writer intended to lose.
This is precisely the failure this whole group exists to prevent, and the
original B1 as scoped did not prevent it.

**Fix: make the index itself immutable and generation-keyed, then CAS the
pointer to it.** This is the same pattern already used for the Backfill Plan
in Group E (immutable content once written, mutable state tracked
separately) — it should have been applied here first, since this is the more
safety-critical instance of the same shape.

- `archive-index.jsonl`'s fixed key is replaced by a key derived from
  generation number: `archive-index/<generation>.jsonl`. No new field on
  `SchemaVersion` is needed — `generation: u64` already exists
  (`model.rs:139-143`) and deterministically derives the key.
- `push_index()`'s corrected flow:
  1. Read the current marker (`schema-version.json`) → `remote_generation`,
     its etag.
  2. `next_generation = remote_generation + 1`.
  3. Write the full serialized index to the **never-before-used** key
     `archive-index/<next_generation>.jsonl` via
     `put_if_match(key, expected_etag: None, bytes)` — create-only. Because
     generation numbers are never reused, this key has never existed, so a
     create-only conditional write here protects the *payload* itself, not
     just the pointer. If it fails, someone else already claimed
     `next_generation` — re-read the marker and retry with a fresh
     generation number, before ever attempting the marker write.
  4. Only after that succeeds, CAS the marker (`schema-version.json`) to
     point at the new generation, using the etag read in step 1.
  5. If the marker CAS fails (rare once step 3 already filters out the
     common race), re-read and retry from step 1.

Walking the exact two-writer scenario through this fix: A and B both read
generation N and both compute `next_generation = N+1`. Both attempt to create
`archive-index/N+1.jsonl`. Exactly one succeeds (say A) — B's create-only
write is rejected *before B ever reaches the marker*, forcing B to re-read
current state and retry with a fresh generation number. B's content can never
silently occupy the same key as A's, because there is no longer a shared
mutable key for the payload. A crash between step 3 and step 4 (index
written, marker not yet advanced) leaves a harmless orphaned object — readers
only trust the marker's generation, so they keep reading the last
fully-committed index until the marker actually advances; nothing
inconsistent is ever visible. This is a strict improvement over today's
`push_index` too: today, a crash between its two existing unconditional
writes can already leave the marker and the index content out of sync with
each other — the corrected design makes that class of bug structurally
impossible, not merely less likely.

**Bucket-layout change — real migration implication for an already-backfilled
archive.** This is not internal-only. Today's fixed key `archive-index.jsonl`
is replaced by the generation-keyed scheme going forward, and `reconcile`
/pull-index must change to read the marker first and derive the index key
from its generation, rather than assuming a fixed key. Every machine that
will push or pull against a given bucket needs to be on the updated code
together — a mixed fleet (some machines still on the old fixed-key
assumption) would silently diverge, with old clients stuck reading a key
that stops being updated. For an already-backfilled archive (the 130GB case
discussed earlier in this design): the current `archive-index.jsonl` object
simply becomes an inert, orphaned leftover after the first post-upgrade
`push_index` — nothing to migrate by hand, but this needs a coordinated
upgrade across every machine touching the bucket, not a rolling one.

**Conditional-write support cannot be assumed on S3-compatible endpoints.**
AWS S3 itself only gained native `If-Match`/`If-None-Match` conditional-write
support on `PutObject` in 2024 — this is a recent capability, not one that
has always existed. Trove's `endpoint` config (ADR 003) explicitly targets
S3-compatible services (MinIO, R2, and similar) with a promise of "no code
change," but conditional-write behavior on non-AWS endpoints varies by
product and version and must not be assumed correct without verification
against the actual configured target. If `put_if_match` is issued against an
endpoint that silently ignores the conditional header rather than honoring
or rejecting it, this entire unit's protection disappears with no error —
the worst possible failure mode, since it looks identical to working
correctly. Decision: `S3Store` must verify conditional-write behavior is
actually honored (a startup capability probe, or documented per-endpoint
support) before relying on it, and must **fail loudly** — consistent with
this project's existing convention of surfacing gaps explicitly rather than
faking success (`AGENTS.md`) — rather than silently falling back to an
unconditional put when support can't be confirmed. The exact probe mechanism
is left to implementation; failing loud rather than silently degrading is
not.

No migration concern for the *etag itself* on already-archived data,
separate from the bucket-layout point above: an S3 ETag is a protocol-level
property S3 assigns automatically to every object at write time — it has
existed on every object in the bucket since the moment each was uploaded,
including everything committed before this ADR. This is categorically
different from an app-level field like the D2 slug — there is no such thing
as an S3 object without an ETag, so `schema-version.json` (and every
`music/…` object from the existing archive) already has one, readable right
now via `S3Store::head()` (`store/s3.rs:140-149`, already correctly reading
`resp.e_tag()` from the SDK response).

`ObjectMeta.etag` (`store/mod.rs:25`) already exists on every backend; no
*conditional-write logic* consumes it yet, which is what this unit adds:

```rust
fn put_if_match(&self, key: &str, expected_etag: Option<&str>, bytes: &[u8])
    -> Result<PutOutcome>;   // PutOutcome::Written(ObjectMeta) | Conflict { current_etag }
```

`expected_etag: None` means create-only (used both for the bucket-genesis
case and for the generation-keyed index writes in step 3 above).

- `S3Store` implements this with S3's native conditional-write headers, once
  verified supported by the configured endpoint per the requirement above —
  genuinely atomic server-side, no client locking needed.
- `FsStore` implements it locally: stat/hash the current file for a
  stand-in etag, write to a temp file, re-check, then atomically `rename()`
  into place. Good enough for a single-host dev simulator; not a claim of true
  multi-writer atomicity there, which is fine since `FsStore`'s job is local
  dev, not concurrent-writer correctness — this distinction matters directly
  for the [Validation plan](#validation-plan) below, since a `FsStore`-based
  test proves the retry *control flow* is correct, not that real concurrent
  writers are safe. **Known gap to fix as part of this unit:**
  `FsStore::head()` currently hardcodes `etag: None` (`store/fs.rs:64-71`)
  even though `FsStore::put()` a few lines above correctly computes one —
  `head()` needs to actually produce a comparable etag (or a cheaper local
  surrogate such as size+mtime) before the read-then-conditional-write flow
  can work against `FsStore` at all. This is local-simulator-only; it has no
  bearing on real S3 data.

**B2. Stronger `archive verify` / import verify.** Move past the current
size/`head`-only check (`import verify` today, and the CLI's `archive verify`,
which currently just bails with "not implemented in this bootstrap yet") to a
real SHA-256 re-check option: re-download (or re-read locally where possible)
and compare against the recorded hash, so "everything but the bucket is
disposable" is something Trove actually verifies, not just asserts.

### Group C — Import cost and cache correctness

**C1. Persistent, path-keyed stat cache.** Before `import/mod.rs:240-241`
(`std::fs::read` + `hash_bytes`) runs for a *newly discovered* file (not a
job-resume — that's `can_skip_fingerprint`, which stays as-is), check a local
cache keyed by `(absolute path, size, mtime) → sha256`, independent of any
`job_id`. On a match, reuse the stored hash instead of re-reading the file.
This is a strictly free correctness-preserving optimization — if size and
mtime are unchanged, nothing about the file's identity is being assumed, only
avoided-recomputing. Closes the "re-running `trove import` against an
unchanged path re-hashes everything" gap (confirmed: a fresh, non-resumed job
always starts with an empty `existing` map at `import/mod.rs:170-173`).

**C2. Reconcile-before-import, closing the cold-cache duplicate-entry gap.**
`import_plan` / `import_run_full` currently never reconcile
(`facade.rs:360-380`, `516-527` — no `self.reconcile()` call anywhere in the
path, unlike `query()` at `facade.rs:106-108`). This means the archive-wide
dedupe checks in Group C1's neighborhood and in `import/mod.rs:662-670` are
only as good as whatever the local cache happens to already contain. Decision:
`import_plan` gains an explicit reconcile step before scanning begins (default
on; an explicit offline/skip flag remains available for the already-documented
offline-import story, since ADR 005's resumable-offline design must not be
broken by this). This directly closes the gap where a cold cache — new
machine, wiped `~/.trove`, or a machine that hasn't pulled since another
device committed matching content — mints a second `ArchiveEntry`/`TrackId`
for content the bucket already has (`import/mod.rs:699-713`), even though
`store.exists` (`import/mod.rs:682`) correctly prevents a duplicate *byte*
write. Note `scripts/import-batch.sh`'s existing preflight
(`archive pull-index --offline`) does **not** already cover this — `--offline`
deliberately skips contacting the bucket; it exists only to fail fast on an
S3-config/feature mismatch.

### Group D — Cross-drive content identity

**D1. Explicit, persisted "library root" concept.** Today, `ImportJob.source_root`
(`import/mod.rs:124`) is just whatever path was passed to `trove import <path>`
for that one invocation — it is *not* a stable concept across differently
granular invocations. Batch imports (one job per immediate subdirectory) and
single deep-tree imports compute different `source_root`s for identical files,
which breaks any scheme that derives portable identity from "path relative to
root." Decision: introduce an explicit, named library root — set once (config
default or an explicit flag, separate from whatever path a given `import`
command happens to be pointed at) — so relative-path identity means the same
thing regardless of how deep a particular invocation scans. This is the
anchor both Group D2 and Group E depend on.

**D2. Library-relative-path "slug" as a portable identity signal.** New field
on `ArchiveEntry`: `library_relative_path: Option<String>` — the file's path
relative to the declared library root (D1), computed and stored at commit
time alongside the existing `source_path_original` (which stays as the
absolute, single-machine, one-shot snapshot the metadata-backfill plan in ADR
999 already wants). A new local index (`find_by_relative_path(slug) -> Vec<ArchiveEntry>`,
plural — two unrelated libraries could coincidentally share a catalog
position, so this stays a candidate set, never a single trusted answer by
itself) backs the fast path below.

**Canonical slug format — required before the column exists, not an
implementation afterthought.** `library_relative_path` must be computed and
stored in a normalized, OS-independent form: forward-slash (`/`) separated
regardless of host OS, no leading slash, Unicode-normalized to NFC before
both storage and every comparison. This is a real, silent-failure-shaped risk
if skipped: `source_path_original` already stores `Path::display()`'s
native-separator string today (`import/mod.rs:711`), which is fine for a
display-only field but wrong for a comparison key — a Windows-mounted copy
using `\` natively, or a filesystem that hands back NFD-decomposed filenames
(a real macOS/APFS gotcha for accented characters), would produce a
byte-different slug for the identical logical path under `T7`/`T72`. The
failure mode is a silent miss, not a false match — the fast path just quietly
falls back to a full hash, which is safe but defeats the entire reason this
group exists. Needs to be nailed down before the column exists, not
discovered later from a support report.

**D3. Slug + size fast path — full skip, confirmed by the user.** Before
reading a file's bytes, compute its slug and do a cheap `stat()` for size. If
both match an existing archive entry, treat it as a duplicate and **skip the
read and hash entirely** (full skip, not deferred-but-still-hashed — this was
an explicit choice: the point is avoiding the re-scan cost on a cloned/rsynced
replacement drive, and batching-but-still-hashing would buy nothing toward
that). If the slug matches but size differs, fall through to a real hash — the
coarse filter's job is to flag "needs a closer look," never to make the final
call on an ambiguous case. If there's no slug match, no shortcut applies; same
cost as today. `import plan`'s dry-run output must visibly distinguish
`duplicate (slug+size, not re-hashed)` from `duplicate (hash-confirmed)`, so
what was trusted vs. verified is never silently invisible — this matches
`AGENTS.md`'s existing convention for surfacing deliberate gaps honestly.
Migration for new imports going forward is purely additive — existing
`ArchiveEntry` rows have `library_relative_path = None` until re-touched, and
nothing breaks in the meantime. `archive.sqlite` gains this via the same
idempotent `ALTER TABLE ... ADD COLUMN` pattern already used in
`db/import.rs:455-515` and `db/transfer.rs:301-328` — not a new mechanism.

**Scan-time check order — D3, then C1, then a full hash, in that order,
decided explicitly here rather than left ambiguous.** D3 and C1 answer two
different questions, not redundant ones: D3 asks "does this file match
something *already archived*" (via slug + size); C1 asks "have I *hashed*
this exact file before, archived or not." Left unordered, an implementation
could silently pick either, and the UX table further down would misattribute
which unit actually did the work. The defined pipeline for a newly
discovered file:

1. Compute the slug (D1/D2) and check size against archive entries sharing
   that slug (D3). Match → full skip, labeled
   `duplicate (slug+size, not re-hashed)` in plan output.
2. No D3 match → check the local `(absolute path, size, mtime) → sha256`
   cache (C1). Match → reuse the cached hash, skip the read, then continue
   through the normal archive-wide `find_by_sha256` check exactly as today.
   This is what covers a file fingerprinted in an earlier, possibly-abandoned
   plan that was never committed — D3 has nothing to match against yet for
   that file, since it has no slug until it's archived.
3. Neither matches → read and hash the file, as today.

C1's actual remaining job, once D3 exists, is narrower than "the general
rescan skip": it's specifically for files hashed before but not (yet, or
ever) committed. Once a file *is* committed, D3 alone is what recognizes it
on every future scan — same path, or a cloned drive at a different one. The
[UX changes](#ux-changes) table reflects this split rather than crediting
the general rescan case to C1 alone.

**D2a. One-time retroactive slug backfill for already-archived content.**
Because `source_path_original` has always stored the file's full absolute
path at commit time (`import/mod.rs:711` — `file.path`, not anything relative
to a job's `source_root`), computing a slug for content archived *before*
this ADR is a cheap, local, offline, metadata-only operation once a library
root is declared (D1): for each existing entry whose `source_path_original`
falls under the declared root, strip the prefix and set
`library_relative_path`, then push the updated index once. No re-read, no
re-hash, no re-upload of audio — the entire pass costs roughly one string
operation per archived track plus a single `push_index()` call, regardless of
how large the underlying library is. Entries whose recorded path doesn't fall
under the declared root (ad hoc imports predating a consistent library
layout) correctly stay `None` — not broken, just not eligible for the fast
path, same as any other file that was never part of "the library" concept.
Illustrative surface: `trove archive backfill-slugs --library-root <path>`.
This exists specifically so already-backfilled archives (the common case,
not a hypothetical) don't need to choose between staying on the old path
forever or re-importing everything to get the new one.

### Group E — Backfill coordination and operator UX

**E1. Library shape — a read-only inspection capability.** A new,
first-class `trove-core` operation (not a bash script) that walks a library
root (`readdir` + `stat` only — no file reads, no hashing) and reports
structure: per-subtree file counts, byte totals, depth. Cheap even on a huge
library because it never opens file contents. This directly answers the
operator pain that motivated this whole group: "how is this actually being
scanned," visible *before* committing to a run, instead of only becoming
apparent from stderr scroll partway through.

**E2. The Backfill Plan — durable, bucket-pushed, generated from E1.** A Plan
is created from a shape scan against a declared library root (D1): chunk
boundaries are **folder-boundary**, with a folder-count lever (default 1
subdirectory per chunk, tunable to group N subdirectories per chunk) — chosen
explicitly over byte-precise batching, which was judged harder for an
operator to reason about for no real benefit at this stage. The Plan document
(`plan_id`, `library_root`, chunk list with rough file-count/byte-size
estimates from the shape scan — this stat data is captured for free during
E1 and reused by E4 below) is written locally first, then pushed **once** to
the bucket as an immutable object (e.g. `backfill-plans/<plan_id>.json`,
parallel to how `archive-index.jsonl`/`schema-version.json` live under the
bucket prefix). It is never rewritten after creation — it describes *scope*,
not *progress* — so it needs no CAS at all.

**E3. Append-only event log for chunk progress — no CAS required by design.**
Progress is tracked as small, independently-keyed event objects appended
under the plan (e.g. `backfill-plans/<plan_id>/events/<uuid>.json`), not by
mutating a shared document: a `claimed` event when a machine starts a chunk
(optional but default-on — it costs one small write and is the only thing
that gives a second machine real-time visibility to avoid picking the same
chunk), and a `completed` event when a chunk's normal `import plan/run/commit`
pipeline finishes successfully. Each event is a pure create with a unique key
— never a read-modify-write — so, unlike `schema-version.json`, this entire
coordination layer needs **zero** compare-and-swap. This is safe specifically
*because* claiming isn't required to be exclusive: two machines redundantly
working the same chunk is wasteful, not incorrect — content-addressing
(`store.exists` at commit) and, once shipped, the Group D2/D3 fast path make
that redundancy cheap to absorb rather than something that needs preventing.
Any machine (including one joining later) can reconstruct current status —
completed / claimed-but-not-completed (a stale-claim hint, not a lock) /
untouched — by pulling the Plan document once and folding its event log; this
is what answers "what have I missed," durably and without hashing anything.
Existing `ImportJob`/`sync.sqlite` machinery (ADR 005) is unchanged and sits
underneath this unmodified — a chunk's actual import work is a completely
ordinary job, just one whose folder boundary came from the Plan instead of an
ad hoc CLI argument.

**E4. Chunk-level stat sanity check.** Because folder names can coincidentally
collide across genuinely different content (a machine's drive having a
same-named folder with unrelated contents underneath — the one real risk case
identified while stress-testing this design), record the rough file
count/byte-size the shape scan (E1) observed for each chunk in the Plan
document, and check it at claim/completion time against what's actually being
imported. A large mismatch (e.g. a chunk recorded as 8 tracks / ~45MB
completing with 40 tracks / ~300MB underneath) is a cheap, `stat()`-only
signal that something doesn't match what was originally scoped — not a hard
guarantee, a proportionate sanity check using data already collected for
free, consistent with this whole layer's role as a coarse, best-effort hint
rather than a correctness boundary.

**E3a. Illustrative CLI surface and an explicit claim policy — naming is
revisable, the behavioral decisions are not.** Left fully unspecified, this
is exactly the kind of ambiguity that produced `import-batch.sh` in the first
place. Concrete enough to implement against:

- `trove library shape <root>` (E1) — read-only, as already named.
- `trove library plan <root> [--chunk-folders N]` (E2) — **always creates a
  new `plan_id`; there is no implicit "resume the existing plan for this
  root" behavior.** Silently reusing or mutating a prior plan based on a
  root-matching heuristic is exactly the kind of implicit magic that made
  `import-batch.sh` hard to reason about. To continue prior work, reference
  that plan explicitly by id. (This decides what was previously left as an
  open call in the validation table below.)
- `trove library plan list [--library-root <path>]` — lists known plans by
  scanning the `backfill-plans/` prefix (`ObjectStore::list` already exists,
  `store/mod.rs:49`; these are small JSON documents, cheap to list and
  filter — no separate index object needed), optionally filtered by root.
- `trove library plan status <plan-id>` — pulls the Plan document and its
  event log, folds them into per-chunk status (completed /
  claimed-and-by-whom-and-when / untouched). This is the "what have I
  missed" answer.
- `trove library plan claim <plan-id> [<chunk-id>]` — with `chunk-id`
  omitted, picks the next chunk per the pick-next rule below, writes the
  claim event (E3), then drives the *ordinary* `import plan → run → commit`
  pipeline against that chunk's resolved folder(s), writing the completion
  event on success. A chunk becoming a real `ImportJob` is not new or
  separate machinery — it *is* the existing pipeline (ADR 005, unchanged),
  just pointed at a folder the Plan chose instead of a raw CLI argument.

**Claim policy, decided explicitly rather than left implicit:** claiming is
default-on but **non-exclusive, has no TTL, and nothing ever auto-steals a
claim.** A machine that dies mid-chunk leaves a permanent `claimed`-with-no-
`completed` record — an accepted, correct outcome of this model (redundant
work is wasteful, not unsafe), but it still means the CLI needs a
deterministic default for "what do I work on next," which claim-policy alone
doesn't supply:

- **Pick-next rule:** prefer chunks with no `claimed` event at all, in the
  Plan's listed order (itself the shape scan's alphabetical folder order —
  consistent, not incidental). If none remain untouched, `claim` without an
  explicit chunk id refuses rather than silently redoing another machine's
  possibly-still-in-progress work — picking up a stale claim requires an
  explicit chunk id, or an explicit `--include-claimed` opt-in. This keeps
  "no steal" a real default rather than a suggestion, while still leaving
  stale-claim recovery possible on purpose.

**E5. Retire `scripts/import-batch.sh`.** Once E1–E4 land, the script's
entire reason to exist — ad hoc chunking, ordering, and resume-by-copying-a-path
— is superseded by first-class, resumable, cross-machine-aware core + CLI
commands. Per `AGENTS.md`'s architectural rule, keeping two implementations of
the same chunking/ordering logic (one in bash, one in core) is exactly the
kind of drift that gets a fix applied once and silently missed in the other.
The script should be deleted, not deprecated-in-place, once its replacement
ships — kept only long enough to validate the new commands cover its actual
use cases (see [Validation plan](#validation-plan)).

## UX changes

The point of this section is what an operator actually experiences differently
— this is what "feeling out the ergonomics" means to validate, concretely,
not just "the code is different."

| Today | After this ADR |
| --- | --- |
| `bin/trove import <path>` re-reads and re-hashes every file, every run, even an unmodified rescan. | Once a file is committed, any future scan recognizes it via slug+size (D3) and skips it — same path, or a cloned drive at a different one. A file hashed but never committed (an abandoned plan, say) is still skipped via the path-keyed cache (C1) instead of re-hashed. See the ordered pipeline defined under D3. |
| Re-pointing import at a cloned/replacement drive (different mount, same structure) pays full re-hash cost for the entire library. | The slug + size fast path (D2/D3) recognizes matching relative structure and skips reading/hashing entirely — visibly marked as *trusted*, not *confirmed*, in plan output. |
| Multi-folder backfills are driven by `scripts/import-batch.sh`: one chunk = one immediate subdirectory, no visibility into subtree size before running, resume requires copying an exact folder path out of prior terminal output. | `trove library shape <root>` shows real structure/size up front; `trove library plan <root>` generates a durable, chunk-sized-your-way Plan; resuming means pulling the Plan and picking the next open chunk — no copy-pasted paths. |
| A second machine helping with the same backfill has zero shared state — coordination happens by memory or chat, and nothing stops (or reveals) two machines duplicating the same folder's work. | Any machine can pull the same Plan, see exactly which chunks are done/claimed/open, and safely pick up an unclaimed one — real shared state, durable in the bucket. |
| A cold cache (new machine, wiped `~/.trove`) can silently create a duplicate archive entry for content that's already archived — invisible until you notice a track appears twice in `query`. | `import_plan` reconciles first by default (C2); the cold-cache duplicate-entry path is closed. |
| "What have I imported vs. what's left" has no answer without reconstructing it from scattered local job histories across however many machines were involved. | Pull the Plan + fold its event log — one durable artifact, correct regardless of which machine did which chunk. |
| `scripts/import-batch.sh` exists as a bash workaround nobody designed on purpose. | Retired — its job is done by first-class, resumable, cross-machine-aware commands that a Tauri UI can actually call. |

## Consequences

### Positive

- Removes the daemon hop and JSON (de)serialization cost for the primary
  desktop path.
- Unblocks the flash-drive/volume UI workflow, blocked by browser filesystem
  sandboxing, not by missing `trove-core` logic.
- Closes a real correctness gap (`schema-version.json` race) before, not
  after, multi-device usage grows into the condition that triggers it.
- Closes a real, previously undocumented correctness gap (cold-cache duplicate
  archive entries) that "archive-wide dedupe" was silently not guaranteeing.
- Materially reduces the cost of the operations a DJ actually repeats often —
  rescanning, rebuilding onto a replacement drive, running a large backfill —
  without weakening the archive's canonical identity (SHA-256 stays the
  bucket's ground truth; the fast paths only skip *recomputing* it under
  specific, cheap-to-check conditions).
- The entire new coordination layer (Plan + event log) needs no CAS by
  construction — the blast radius of "things that need true compare-and-swap"
  stays exactly where it already was, at `schema-version.json`.
- Moves chunking/ordering/resume logic into `trove-core`, where a Tauri UI can
  actually call it — closing an architectural-rule violation, not just a UX
  gap.

### Negative / trade-offs

- Tauri packaging (build pipeline, per-OS builds, eventual code
  signing/notarization/updater) is new engineering surface unrelated to
  `trove-core`.
- A parked `trove-serverd` can drift toward silent rot if "keep it compiling"
  isn't actually enforced — mitigated by `make check`, but requires
  discipline, not just intent.
- The slug/size fast path is a deliberate, named trade of certainty for speed
  — it must stay visibly distinguished from hash-confirmed duplicates in
  output, or it quietly weakens the archive's identity guarantee without
  anyone noticing.
- Reconcile-before-import (C2) adds a network/bucket dependency to the start
  of every import that wasn't there before; the offline path must be
  preserved explicitly, not accidentally broken.
- The chunk stat sanity check (E4) is a heuristic, not a guarantee — a
  coincidental folder-name collision with a *plausible* matching size would
  still slip through. Accepted as proportionate; not solved here.
- This is a large surface for one ADR. Each group is independently useful and
  buildable on its own, which mitigates the risk of the whole thing stalling
  together, but it does mean partial completion is a normal, expected state,
  not a sign something went wrong.

## Considered alternatives

- **Keep `trove-serverd` as an internal Tauri sidecar.** Rejected as the
  primary path: keeps exactly the serialization/daemon cost ADR 000 flagged
  without gaining anything Tauri's direct-command path doesn't already
  provide.
- **Delete `trove-serverd` outright.** Rejected: throws away tested, working
  code for no present benefit; parked costs little to keep compiling.
- **Ship the Tauri pivot without pulling CAS forward.** Rejected: multi-device
  usage — the motivating rationale — is the same condition that triggers the
  race; shipping one without the other undercuts the pivot's own reasoning.
- **Trust `(size, mtime)` alone as a cross-drive identity signal (no slug).**
  Considered and set aside in favor of the slug approach: mtime is fragile
  (many copy tools don't preserve it; a plain drag-copy resets it), whereas a
  relative-path slug reflects an intentional structural/provenance signal
  that matches how the operator actually clones drives (rsync, which
  preserves structure) rather than an incidental timestamp.
- **Byte-precise or size-target chunking for the Backfill Plan.** Rejected for
  now: harder for an operator to conceptualize than folder boundaries; a
  folder-count lever gets most of the benefit (coarser or finer chunks) with a
  much simpler mental model. Not ruled out permanently — noted as possible
  future refinement, not adopted now.
- **CAS on `schema-version.json` alone, with `archive-index.jsonl` left as a
  plain unconditional write at a fixed key.** This was the original scoping
  of B1 and is rejected: a review caught that it protects the pointer while
  leaving the payload it points to fully unprotected, which does not close
  the race this group exists to close (see B1 for the exact scenario).
  Replaced with an immutable, generation-keyed index plus a CAS'd pointer to
  it — the same immutable-content/mutable-pointer shape already chosen for
  the Backfill Plan (E2/E3), applied to the case that actually needed it
  first.
- **A single, mutable, CAS-protected Plan-progress document** (one shared file
  tracking every chunk's status, updated in place). Rejected in favor of the
  append-only event log: because redundant chunk work is safe (not just
  tolerable) given content-addressing and the slug fast path, the
  coordination layer doesn't need mutual exclusion — an append-only log gets
  the same visibility with a strictly simpler write model and no CAS
  dependency at all.
- **Auto-reconcile silently inside every import call with no way to skip it.**
  Rejected: would break ADR 005's resumable-offline import story. The
  reconcile-before-import default in C2 needs an explicit opt-out preserved,
  not an unconditional network dependency.

## Scope

- **This ADR authorizes:** Tauri as the desktop shell with direct
  `trove-core` command bindings (A1); retiring — parking, not deleting —
  `trove-serverd` from the primary UI path while keeping it building and
  tested (A2); an immutable, generation-keyed canonical index
  (`archive-index/<generation>.jsonl`) with a CAS'd `schema-version.json`
  pointer to it, via a new `ObjectStore::put_if_match` (B1); a required
  conditional-write capability check (fail loud, never silently degrade) for
  any S3-compatible `--endpoint` before relying on B1's protection there;
  stronger SHA-256-based archive/import verify (B2); a persistent path-keyed
  stat cache for hashed-but-uncommitted re-scans (C1); an explicitly ordered
  scan pipeline (D3 → C1 → hash → archive lookup); reconcile-before-import by
  default, with a preserved offline opt-out (C2); an explicit, persisted
  library-root concept (D1); a `library_relative_path` slug field, stored in
  a canonical `/`-separated, NFC-normalized, no-leading-slash form, and its
  local index (D2); a one-time retroactive backfill of that field for
  already-archived content (D2a); the slug+size full-skip fast path with
  visible trusted-vs-confirmed labeling (D3); a read-only library shape
  operation in `trove-core` (E1); a durable, bucket-pushed, immutable Backfill
  Plan generated from shape data, chunked by folder boundary with a
  folder-count lever (E2); an append-only, CAS-free event log for chunk
  claim/completion (E3); an illustrative CLI surface (`shape`/`plan`/
  `plan list`/`plan status`/`plan claim`) with a decided, non-exclusive,
  no-TTL, no-steal claim policy and a deterministic pick-next rule (E3a); a
  chunk-level stat sanity check using data already captured by the shape scan
  (E4); retiring `scripts/import-batch.sh` once its replacement is validated
  (E5).
- **Explicitly not in scope:** the flash-drive/volume UI implementation
  itself; metadata/tag extraction; Tauri auto-update or code-signing/
  notarization pipeline details; a decision to eventually delete
  `trove-serverd`; byte-precise or size-target chunking for the Backfill Plan;
  the exact conditional-write capability-probe mechanism (only that it must
  fail loud, not silently degrade); any locking/mutual-exclusion mechanism for
  chunk claims beyond the
  best-effort event log; a full redesign of the row-level duplicate policy
  ADR 004 §5 left open (this ADR closes the *cold-cache* instance of that gap
  via C2, but does not revisit ADR 004's broader policy).

## Validation plan

Concrete, runnable checks — this is how "feeling out the ergonomics" gets
turned into pass/fail rather than vibes, and how a later session (or agent)
confirms a given group actually works before moving to the next.

| Check | Confirms |
| --- | --- |
| Run `trove library shape` against a real multi-GB tree; confirm it returns in seconds, not minutes, and reports sane per-subtree counts/sizes. | E1 is genuinely read-only and cheap. |
| Run `trove library plan` against the same root twice; confirm it always produces two distinct `plan_id`s, never an implicit merge or resume of the first. | E3a's decided (not left open) plan-creation semantics. |
| With no chunk id given, run `trove library plan claim` repeatedly against a Plan with a mix of untouched and already-claimed chunks; confirm it only ever picks untouched chunks, and refuses (rather than silently redoing work) once none remain, requiring an explicit chunk id or `--include-claimed` to proceed. | E3a's pick-next rule and "no steal by default" claim policy. |
| Two separate `TROVE_HOME`s pointed at the same `TROVE_BUCKET_DIR`, each claiming different chunks of one Plan; confirm no collision, and confirm a third pull of the Plan + event log folds into a coherent, correct status view. | E3's coordination model actually works end-to-end. |
| Plan (fingerprint) a folder but abandon before commit, then start a fresh `import plan` against the same folder as a new job; confirm the fresh job's files are recognized via the path-keyed cache (C1) and not re-hashed, since nothing is archived yet for D3 to match. | C1 correctly covers hashed-but-uncommitted content — the case D3 can't reach. |
| Run `trove import` twice against the same unchanged, already-committed folder; confirm the second run is fast, doesn't re-read file bytes, and that plan output attributes the skip to D3 (slug+size), not C1. | The ordered pipeline (D3 → C1 → hash) correctly prioritizes D3 for already-archived content. |
| Clone a folder tree with `rsync -a` (preserving structure) to a different mount point, then import from the new location; confirm the slug+size fast path fires and plan output labels it *trusted*, not *confirmed*. | D2/D3 work as designed for the actual T7→T72 scenario. |
| Repeat the above clone test with accented/non-ASCII filenames, and again from a Windows-mounted copy if available; confirm the slug still matches (NFC-normalized, `/`-separated) rather than silently missing. | The cross-OS slug canonicalization requirement actually holds. |
| Wipe local `~/.trove`, re-run import against a source whose content is already in the bucket; confirm no duplicate `ArchiveEntry` is created. | C2 closes the cold-cache duplicate-entry gap. |
| Construct two folders with the same name but genuinely different contents at the same relative position under two different library roots referencing the same Plan; confirm the chunk stat sanity check (E4) flags the mismatch. | The one identified sharp edge is actually caught. |
| Run `make check` after A2; confirm `trove-serverd` still builds and its existing tests still pass with no new routes added. | A2's "parked, not rotting" claim holds. |
| Force the exact two-writer scenario B1 was originally vulnerable to — both read generation N, both attempt to write index content for N+1 — against the *corrected* generation-keyed scheme; confirm the loser's create-only index write is rejected before it ever reaches the marker, and confirm no committed entries are lost regardless of which machine's marker CAS wins. | The actual bug found in review is closed, not just the marker race. |
| Simulate the same race against `FsStore` (two `Trove` instances committing against the same `TROVE_BUCKET_DIR` in quick succession); confirm the conflict-detect-then-retry *code path* fires correctly. **This proves control flow, not concurrency safety** — `FsStore`'s CAS is explicitly not claimed to be truly multi-writer-atomic (see B1), so a passing result here is not evidence the design is safe under real concurrent writers. | The retry logic itself is implemented correctly. |
| Run the same race against a real S3 bucket (or a verified S3-compatible target) under genuine concurrent load, not a serialized local simulation. | B1 actually prevents the race under real concurrency — the thing the `FsStore` check above cannot demonstrate. |
| Before relying on `put_if_match` against any configured `--endpoint` (MinIO, R2, or similar), verify conditional-write support is actually honored by that specific target; confirm `put_if_match` fails loudly rather than silently succeeding without protection if support can't be confirmed. | ADR 003's "S3-compatible, no code change" promise doesn't silently break under this unit. |

## Implementation sequencing / hand-off

Dependency order, with rough file surface per unit. **Status is honest as of
this ADR's writing: nothing below is implemented.** Update this table as work
lands so a cold pickup — a new session, a different agent — knows exactly
where things stand without re-deriving it from conversation history.

| Unit | Depends on | Rough files touched | Status |
| --- | --- | --- | --- |
| B1: Immutable generation-keyed index + CAS'd pointer, incl. endpoint capability check | — | `store/mod.rs` (trait + both impls), `store/s3.rs` (capability check), `store/fs.rs` (`head()` etag gap), `index.rs` (generation-derived key), `facade.rs:602-619` (`push_index`), `archive/reconcile.rs` (pull-index must read marker-then-derive-key) | Not started |
| B2: Stronger verify | — | `import/mod.rs` verify path, CLI `archive verify` | Not started |
| C1: Path-keyed stat cache | — | `import/mod.rs` scan loop (~line 240), a new local table | Not started |
| C2: Reconcile-before-import | — | `facade.rs` `import_plan`/`import_run_full` | Not started |
| D1: Explicit library root | — | config, `ImportOptions`/CLI surface | Not started |
| D2: Slug field + index (canonical `/`-separated, NFC-normalized format) | D1 | `model.rs` (`ArchiveEntry`), `db/archive.rs`, `import/mod.rs` commit path | Not started |
| D2a: Retroactive slug backfill for existing archive | D1, D2 | new `trove-core` migration op, CLI surface | Not started |
| D3: Slug+size fast path, ordered ahead of C1 in the scan pipeline | D2 | `import/mod.rs` scan + commit paths, plan output/CLI display | Not started |
| E1: Library shape | D1 | new `trove-core` module, CLI/daemon surface | Not started |
| E2: Backfill Plan | D1, E1 | new `trove-core` module, bucket key layout | Not started |
| E3: Event log | E2 | same module as E2 | Not started |
| E3a: CLI surface (`shape`/`plan`/`plan list`/`plan status`/`plan claim`) + claim/pick-next policy | E1, E2, E3 | `trove-cli`, same core module as E2/E3 | Not started |
| E4: Chunk stat sanity check | E1, E2 | same module as E2 | Not started |
| E5: Retire `import-batch.sh` | E1–E4 validated | delete `scripts/import-batch.sh`, update `docs/runbook/02-import.md` | Not started |
| A1: Tauri shell | none of the above strictly required first, but see note | `ui/`, new Tauri project scaffold | Not started |
| A2: Retire `trove-serverd` from primary path | A1 | `crates/trove-serverd` (no deletions), `Makefile`/`scripts/dev.sh` | Not started |

Note on A1/A2 ordering: they don't functionally depend on B–E, but building
the Tauri shell *first* and wiring E1–E4's commands into it as they land is
likely more useful for the "feel out the ergonomics" validation goal than
building the whole backend first and bolting a UI on at the end — worth
deciding deliberately rather than defaulting to strict dependency order.

---

## Looking forward (orientation only — not authorized scope)

What's still genuinely deferred, unaffected by everything above:

1. **Flash/volume workflow as a feature built in the new shell** — the ADR
   006 gap that was always platform-blocked, not backend-blocked; the
   clearest real validation of the Tauri pivot specifically.
2. **Metadata/tag extraction** — deliberately not front-loaded. Expected to be
   shaped by concrete requirements the UI surfaces once real crate-browsing
   screens exist against today's sparse `StubExtractor` metadata, rather than
   designed speculatively ahead of that need.
3. **Analyzer toolkit (ADR 002), streaming multipart upload** — later polish,
   per ADR 999's existing priority list; unaffected by this ADR.
4. **ADR 004 §5's broader row-level duplicate policy** — this ADR closes the
   cold-cache instance of that question (C2) but doesn't revisit the general
   policy of whether a repeat SHA-256 should ever create a second logical row
   on purpose (e.g. multiple playlist-facing entries sharing one physical
   object) — left as ADR 004 left it.
