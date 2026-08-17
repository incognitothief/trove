# Library runbook

Manage the declared **library root** — the stable anchor cross-drive
identity is computed relative to (ADR 007, Group D1). Full command family:

```bash
bin/trove library root                                # show the current root
bin/trove library root --set <path>                    # declare or change it
bin/trove library backfill-slugs                       # backfill existing entries
bin/trove library shape [<path>]                        # inspect structure (defaults to the declared root)
bin/trove library plan create [<path>]                  # push a new Backfill Plan
bin/trove library plan list [--library-root <path>]     # list known plans
bin/trove library plan status <plan-id>                 # per-chunk completed/claimed/untouched
bin/trove library plan claim <plan-id> [<chunk-id>]     # claim a chunk and actually import it
```

## Why declare a library root at all

Content-addressing (ADR 004) makes path irrelevant to what a file *is* —
that's correct and deliberate. But it means recognizing "this is the same
library, just remounted somewhere else" (a replacement drive, a clone via
`rsync`) has no cheap path without one: everything falls back to a full
re-hash of the whole library. The library root is what lets Trove compute a
portable, cross-drive identity signal (the `library_relative_path` slug)
instead.

## `trove library root`

```bash
bin/trove library root
# → no library root declared — set one with `trove library root --set <path>`

bin/trove library root --set /Volumes/T7/music/library
# → library root set to /Volumes/T7/music/library

bin/trove library root
# → /Volumes/T7/music/library
```

Purely a local `config.toml` operation — **no bucket connection required**.
Works without S3 credentials, without a `--features s3` build, without
network. If `~/.trove/config.toml` doesn't exist yet, it's created with the
same local-default bucket stanza Trove already assumes when no config
exists; if it does exist, only the `[library]` section is added or updated —
every other line, including comments, is left exactly as written.

When you replace a drive (T7 dies, T72 takes over with the same structure),
re-point the root at the new mount:

```bash
bin/trove library root --set /Volumes/T72/choons
```

## `trove library backfill-slugs`

Retroactively computes `library_relative_path` for entries committed before
a root was declared (or before this feature existed) — the common case for
an already-backfilled archive, not a hypothetical.

```bash
bin/trove library backfill-slugs
# → 130/130 backfilled (0 already had a slug, 0 outside the library root, 0 with no recorded source path)
```

**Requirements and behavior:**

- Refuses with a clear error if no root is declared yet — it never guesses
  one. Run `library root --set <path>` first.
- Local, metadata-only, and fast: it reads `source_path_original` (already
  recorded at commit time) and re-derives the slug — **no re-download,
  no re-hash, no re-upload of any audio**, regardless of archive size.
- Reconciles first, then pushes the updated index once at the end — only if
  anything actually changed. Running it again when everything's already
  backfilled is a clean no-op.
- An entry whose `source_path_original` doesn't fall under the declared root
  stays `None` — correctly distinguished from "not backfilled yet": it's an
  ad hoc import that was never really part of "the library," not a gap to
  chase down.

### JSON output

```bash
bin/trove --json library backfill-slugs
```

```json
{
  "total": 130,
  "backfilled": 130,
  "already_had_slug": 0,
  "outside_root": 0,
  "no_source_path": 0
}
```

## `trove library shape`

Read-only inspection of a library's structure — per-subtree file counts,
byte totals, and depth (ADR 007, Group E1). This is what answers "how is
this actually going to be scanned" *before* committing to a real `import`
run, instead of only finding out from stderr scroll partway through one.

```bash
bin/trove library shape /Volumes/T7/music/library
```

```text
/Volumes/T7/music/library: 4213 audio file(s) (128.4 GB), 4310 file(s) total (129.1 GB), max depth 2
  (loose files in root)                    3 audio      1.2 MB  depth 0
  Kyle Hall                              212 audio      6.8 GB  depth 2
  Theo Parrish                           340 audio     11.2 GB  depth 1
```

Omit the path to use the declared library root:

```bash
bin/trove library shape
```

**Behavior:**

- `readdir` + `stat` only — **never opens a file's contents**. Cheap even on
  a huge library.
- One line per immediate subdirectory of the root, alphabetical by name —
  the same order the eventual Backfill Plan (Group E2) will chunk against.
  A `(loose files in root)` line appears separately for anything sitting
  directly in the root outside any subdirectory, since there's no folder to
  chunk by.
- Each line's `audio` count/bytes is what `import` would actually pick up
  (same extension allowlist); the total file count/bytes covers everything
  really on disk underneath, including non-audio files like cover art.
- Excludes dotfiles/hidden directories by default, matching `import`'s
  default — pass `--include-dotfiles` to include them.
- Local only — **no bucket connection required**, same as `library root`.

### JSON output

```bash
bin/trove --json library shape /Volumes/T7/music/library
```

```json
{
  "root": "/Volumes/T7/music/library",
  "subtrees": [
    { "relative_path": "Kyle Hall", "audio_file_count": 212, "audio_bytes": 7301234567, "total_file_count": 214, "total_bytes": 7305000000, "max_depth": 2 }
  ],
  "root_files": { "relative_path": null, "audio_file_count": 3, "audio_bytes": 1200000, "total_file_count": 3, "total_bytes": 1200000, "max_depth": 0 },
  "audio_file_count": 4213,
  "audio_bytes": 137890000000,
  "total_file_count": 4310,
  "total_bytes": 138600000000,
  "max_depth": 2
}
```

## `trove library plan`

A **Backfill Plan** turns a shape scan into durable, bucket-pushed chunk
boundaries for coordinating a backfill — across sessions, or across
multiple machines working the same library (ADR 007, Group E2). Chunk
boundaries are folder-boundary: each chunk is one or more immediate
subdirectories of the root. A Plan is immutable once created — it describes
*scope*, not *progress* — so creating a new plan for a root you've already
planned is always a fresh, independent plan, never an update to a prior
one.

```bash
bin/trove library plan create /Volumes/T7/music/library
```

```text
plan 8f14e45f-...  root=/Volumes/T7/music/library  212 chunk(s), 1 folder(s) per chunk
     0  Kyle Hall                                              212 audio      6.8 GB
     1  Theo Parrish                                           340 audio     11.2 GB
   ...
```

Group N subdirectories into each chunk with `--chunk-folders`:

```bash
bin/trove library plan create /Volumes/T7/music/library --chunk-folders 5
```

Loose files sitting directly in the root (no subdirectory of their own)
are folded into the first chunk rather than given a synthetic chunk of
their own — shown as `+ loose root files` in that chunk's line.

**Requirements and behavior:**

- **Needs a live bucket connection** (unlike `root`/`backfill-slugs`/
  `shape`) — a plan is pushed as soon as it's created, as a single
  create-only write. It is never rewritten afterward.
- Always mints a fresh `plan_id`. There's no "resume the plan for this
  root" behavior — reference a prior plan explicitly by id instead of
  relying on a root-matching heuristic.
- Defaults to the declared library root when no path is given.
- Each chunk carries the shape scan's rough audio/total file-count and
  byte-size estimates — a cheap, `stat()`-only planning aid, not a
  correctness guarantee.

List known plans:

```bash
bin/trove library plan list
bin/trove library plan list --library-root /Volumes/T7/music/library
```

```text
8f14e45f-...  2026-08-16T12:00:00Z  212 chunk(s)  root=/Volumes/T7/music/library
```

Scans the bucket's `backfill-plans/` prefix directly — no separate index
object, since these are small JSON documents and cheap to list.

### JSON output

```bash
bin/trove --json library plan create /Volumes/T7/music/library
```

```json
{
  "plan_id": "8f14e45f-...",
  "library_root": "/Volumes/T7/music/library",
  "created_at": "2026-08-16T12:00:00Z",
  "chunk_folders": 1,
  "chunks": [
    {
      "chunk_id": "0",
      "folders": ["Kyle Hall"],
      "includes_root_files": false,
      "estimated_audio_file_count": 212,
      "estimated_audio_bytes": 7301234567,
      "estimated_total_file_count": 214,
      "estimated_total_bytes": 7305000000
    }
  ]
}
```

## `trove library plan status`

Per-chunk status, reconstructed by pulling a plan and folding its
append-only event log (ADR 007, Group E3) — "what have I missed," even on
a machine that's never touched this plan before.

```bash
bin/trove library plan status 8f14e45f-...
```

```text
     0  completed     by laptop at 2026-08-16T12:04:00Z
     1  claimed        by desktop at 2026-08-16T12:05:00Z
     2  untouched
```

**Behavior:**

- Every chunk is `completed` / `claimed` / `untouched`. `completed` wins
  over `claimed` regardless of event ordering — a chunk with both a claim
  and a completion event is `completed`.
- Claiming is **non-exclusive**: nothing prevents (or dedupes) two machines
  both claiming, or even both completing, the same chunk. That's wasteful,
  not wrong — content-addressing and the slug+size fast path (D2/D3) make
  redundant work cheap to absorb — so the event log records all of it
  rather than silently collapsing it. `--json` output includes every claim
  and completion event, not just the latest.
- Needs a live bucket connection: this pulls the plan document and lists
  its full event log.

## `trove library plan claim`

Claim a chunk and actually drive it through the ordinary import pipeline —
`import plan → run → commit` — against its resolved folder(s) (ADR 007,
Group E3a). This is the "do the backfill" command; everything above this
point in the family is inspection and coordination bookkeeping around it.

```bash
bin/trove library plan claim 8f14e45f-...
```

```text
claimed chunk 3 of plan 8f14e45f-...: 212 track(s) committed across 1 target(s)
  /Volumes/T7/music/library/Kyle Hall
```

With no chunk id, picks the next chunk per the **pick-next rule**: prefer a
chunk with no events at all yet, in the plan's own order (the shape scan's
alphabetical folder order). If nothing is untouched, `claim` refuses:

```text
error: claiming a chunk of plan 8f14e45f-...: configuration error: no untouched chunks remain in this plan — pass an explicit chunk id, or --include-claimed to pick up a possibly-still-in-progress chunk
```

Claim a specific chunk directly (bypasses the pick-next rule entirely):

```bash
bin/trove library plan claim 8f14e45f-... 3
```

Recover a stale claim (a machine died mid-chunk and never completed it) —
without an explicit chunk id, this needs an opt-in, so "no steal" stays the
real default:

```bash
bin/trove library plan claim 8f14e45f-... --include-claimed
```

**Requirements and behavior:**

- Needs a live bucket connection — this reads the plan and event log, and
  actually runs import jobs against the resolved folder(s).
- **Non-exclusive by design.** Nothing here checks whether another machine
  already claimed or even completed the same chunk (unless you opt in via
  `--include-claimed`). Two machines redundantly working the same chunk is
  wasteful, not wrong — content-addressing and the slug+size fast path
  (D2/D3) make the redundant upload/hash work cheap to absorb rather than
  something that needs preventing.
- A chunk covering more than one folder (`--chunk-folders N > 1`) or the
  library's loose root files runs one ordinary import job per target, in
  sequence, under a single claim/completion pair for the whole chunk.
- `completed` is only recorded if every target in the chunk succeeds. A
  failure partway through leaves the earlier `claimed` event standing as a
  stale-claim hint — safe to retry with `--include-claimed` or an explicit
  chunk id later.
- Accepts `--include-dotfiles`/`--no-artwork`/`--artwork`, same as `import`
  — this really is the same pipeline.
- `--by <name>` labels who's claiming (defaults to `$USER`). Purely
  informational — never used to grant or deny a claim.
- **Stat sanity check (ADR 007, Group E4):** compares what was actually
  scanned against the shape scan's original estimate for this chunk. A
  mismatch (file count or byte total off by 2x or more in either direction)
  prints a warning but **never blocks the claim** — it's a coarse hint that
  a folder's contents may not be what was originally scoped (the one real
  risk case: a same-named folder with unrelated content underneath, e.g.
  after a drive swap), not a correctness check.

```text
claimed chunk 0 of plan ea835505-...: 11 track(s) committed across 1 target(s)
  /Volumes/T7/music/library/Artist
  warning: stat sanity check mismatch — expected ~1 audio file(s) (7 B), actually scanned 11 (588 B). Not a failure — a folder's contents may have changed since this plan was created.
```

### JSON output

```bash
bin/trove --json library plan claim 8f14e45f-...
```

```json
{
  "plan_id": "8f14e45f-...",
  "chunk_id": "3",
  "targets": ["/Volumes/T7/music/library/Kyle Hall"],
  "tracks_committed": 212,
  "stat_check": {
    "estimated_audio_file_count": 212,
    "estimated_audio_bytes": 7301234567,
    "actual_audio_file_count": 212,
    "actual_audio_bytes": 7301234567,
    "mismatch": false
  }
}
```

## See also

- [ADR 007 — desktop shell, IO durability, multi-device backfill coordination](../adr/007-tauri-shell-and-io-durability.md)
- [Archive runbook](01-archive.md) — `push-index`, `verify`
- [Import runbook](02-import.md) — the fingerprint cache and reconcile-before-import behavior this same ADR added
