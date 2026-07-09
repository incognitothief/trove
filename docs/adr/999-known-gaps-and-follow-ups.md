# ADR 999: Known Gaps and Follow-ups

- **Status:** Living document (not a decision ADR)
- **Date:** 2026-07-08
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** all ADRs; this file is the operator-facing backlog when something is missing, stubbed, or undocumented

## Purpose

This is Trove's **running list of gaps** — things we know are missing, incomplete,
or awkward today. When a gap is closed (shipped in code or explicitly rejected in
another ADR), remove or strike it here and link to the ADR/commit that resolved
it.

**How to use:**

- Add a row when you hit friction in real use or spot a seam the code never wired.
- Prefer one line + pointer over long prose; link to the ADR that owns the area.
- Do not duplicate full design write-ups — those stay in numbered ADRs.

---

## Reconciliation with early genesis gap analysis

Cross-check against the pre-ADR-005/006 “material gaps” inventory (S3 integration
notes). **Closed** items are listed for context only — remove from mental backlog.

| Original gap | Status | Notes |
| --- | --- | --- |
| Crash-safe / resumable bulk import | **Closed** | ADR 005: durable `ImportDb`, `plan/run/resume/status/verify/commit`, retry/backoff on uploads. |
| Flash / sync to performance volume | **Mostly closed** | ADR 006: byte transfer, `sync resume`/`verify`, volume DB, `volume list`/`diff`, Mixxx `.m3u8` on volume. Remaining: `sync query --to` execution, UI/daemon routes. |
| Playlist export / Mixxx | **Closed** | `sync playlist` writes volume `Playlists/*.m3u8` with paths relative to playlist file (Mixxx-importable). |
| Real metadata / tags | **Deferred** | See [Tag extraction](#tag-extraction-and-index-metadata-adr-002--adr-004) — not blocking backfill. |
| Index write safety (CAS) | **Open** | See tag section + [Archive integrity](#archive-integrity-and-bucket-index-form). |
| Archive integrity (`archive verify`) | **Open** | CLI stub; import verify is size/`head` only. |
| Bucket SQLite form (`archive-index.sqlite`) | **Open** | Constant defined; only JSONL written/pulled today. |
| Streaming / checkpointed multipart | **Open** | Whole file in RAM; no part-level resume (ADR 005 §5a). |
| Analyzer toolkit | **Open** | ADR 002 — post-genesis. |
| UI vs genesis workflows | **Partially open** | See [UI](#ui-vs-genesis-workflows). |
| `playlists.jsonl` in bucket | **Open** | Path constant only; playlists live in local `playlists.sqlite`, not pushed with index. |
| Shared client wiring duplicated | **Open** | `runtime::open_trove` pattern copied CLI ↔ `trove-serverd`. |
| Tauri desktop shell | **Out of scope** | Intentionally deferred; React + `trove-serverd` is the UI path. |

### Suggested priority (revised)

1. **Archive backfill** — run durable import at scale (resume already shipped).
2. **Tag extraction / index metadata** — after backfill; unlocks query + USB folder layout.
3. **CAS on `schema-version.json`** — before multi-machine or parallel import/backfill jobs.
4. **`archive verify` + stronger object checksum checks** — trust but verify at archive level.
5. **`sync query --to` + flash UI/daemon routes** — complete query → USB loop in UI.
6. **Bucket portable index** — write/pull `archive-index.sqlite`; push `playlists.jsonl` if genesis still wants it.
7. **Analyzers** (names / art / BPM) — ADR 002.
8. **Streaming multipart + disk-backed upload** — huge-library polish.

---

## Operator documentation

| Gap | Notes |
| --- | ----- |
| **Runbooks for all command sets** | Need step-by-step runbooks (happy path + resume/recovery) for every CLI surface: `archive`, `import`, `query`, `playlist`, `volume`, `sync`. Today README/ADRs describe architecture; operators still lack copy-paste workflows per command family. |
| README status section stale | Lists import resume, `.m3u8` export, and sync execution as deferred — several are shipped (ADR 005/006). README should be refreshed or point here. |

---

## Playlist

| Gap | Notes |
| --- | ----- |
| **`trove playlist show <name>`** | No CLI to inspect playlist contents. Core has `playlist_get`; daemon exposes `GET /playlists/:name`. CLI only has `list` (names/ids) and `export` (m3u8 body). Need human-readable track listing (artist/title/BPM) and `--json` with resolved entries. |
| **`trove playlist delete <name>`** | No way to remove a logical playlist from `playlists.sqlite`. |
| **`--format` on export vs config** | CLI accepts `--format m3u8\|m3u` but export currently follows `[export].playlist_format` in config; flag is misleading until wired. |

---

## Import (ADR 005)

| Gap | Notes |
| --- | ----- |
| Multipart **part** checkpoint resume | Job/file durability shipped; mid-file multipart resume still deferred (ADR 005 §5a). |
| **Upload verify is weak** | `import verify` checks object presence + size via `head` only — no re-download and SHA-256 compare against staged/uploaded bytes (ADR 004 identity is hash-at-upload-time). |
| Richer `import status` during fingerprint | No hashed/pending counts while fingerprinting; only visible via progress stderr. |
| Import manifest not in bucket | Local JSON under `~/.trove/cache/manifests/` only; not a durable bucket artifact for cross-machine audit. |
| Flip ADR 005 status to Accepted | Implementation landed; doc still Proposed. |

---

## Export / sync (ADR 006)

| Gap | Notes |
| --- | ----- |
| **Revisit sync vs export paradigm** | `playlist export` (stdout preview) vs `sync playlist` (bytes + `Playlists/*.m3u8` on volume) feels wrong in practice — two paths, easy to confuse, and export output is not Mixxx-importable on its own. Need a design pass: one operator mental model, when each command applies, and whether export should exist separately at all. Deferred — do not patch ad hoc until revisited. |
| **`sync query --to` execution** | Plan-only today; playlist sync executes. |
| **Daemon HTTP routes for flash workflow** | No sync/volume/export routes on `trove-serverd` yet. |
| **UI flash-drive workflow** | React UI has search/playlists/import only. |
| **Streaming download** | Sync uses full-object `get`; costly for large libraries. |
| **Volume prune** | No host-side cleanup of stale volume DB rows / orphan files on drive. |
| Flip ADR 006 status to Accepted | First cut shipped; doc still Proposed. |

---

## Archive integrity and bucket index form

| Gap | Notes |
| --- | ----- |
| **`trove archive verify`** | CLI bails (“not implemented in this bootstrap yet”). Need checksum-level pass: every indexed `sha256` exists in bucket, optional byte re-read vs hash, report orphans/drift. |
| **`archive-index.sqlite` not written** | Genesis (ADR 000) keeps JSONL + SQLite in bucket for fast restore. `push_index` / `reconcile` use JSONL only; `ARCHIVE_INDEX_SQLITE` path is unused. |
| **`playlists.jsonl` not pushed** | Playlists are host-local (`playlists.sqlite`). Bucket key constant exists; no push/pull with index generation. |
| Compare-and-swap on `schema-version.json` | Also tracked under tag section — concurrent `push_index` can clobber (ADR 001 follow-up). |

---

## UI vs genesis workflows

| Workflow | Gap |
| --- | --- |
| Library search | Basic query works; metadata thin until tag extraction (artist/album filters unreliable). |
| Playlist builder | Create/add via UI + daemon; no export or sync-to-volume from UI. |
| Flash drive | Missing entirely — no volume init, sync, or verify in UI or daemon HTTP API. |
| Import | Daemon exposes plan/run + status/list/prune; no rich progress UI, step review, or resume controls in React. |
| Recovery | `archive pull-index` / reconcile works; no end-to-end “lost laptop → backfill → flash Mixxx USB” operator flow in UI. |

---

## Packaging / architecture

| Gap | Notes |
| --- | ----- |
| Duplicated Trove wiring | `runtime::open_trove` + config loading duplicated in `trove-cli` and `trove-serverd` (ADR 001 acknowledged). Small shared crate or module would reduce drift. |
| Tauri desktop shell | Intentionally out of scope; web UI + local daemon is the genesis UI path. |

---

## Tag extraction and index metadata (ADR 002 / ADR 004)

**Decision for now:** defer real tag extraction until **after** the initial archive
backfill. Backfill is not blocked — audio objects and embedded tags are preserved
in the bucket; only Trove’s **index metadata** is thin today.

### What we have today

| Layer | Behavior |
| --- | -------- |
| **Import commit** | `StubExtractor` only: title from filename stem, `file_type` from extension. No artist, album, BPM, key, or embedded artwork read into the index. |
| **Bucket objects** | Content-addressed bytes (`music/<sha256>.<ext>`). **Embedded ID3/Vorbis/etc. tags stay in the file** — sync copies bytes unchanged. |
| **Archive index** | `archive.sqlite` + `archive-index.jsonl` carry sparse `metadata` on each `ArchiveEntry`. |
| **Export / USB layout** | Folder paths use index metadata → often `Unknown Artist/Unknown Album/` until extraction lands. |
| **Mixxx / other players** | Read **embedded tags from the file**, not Trove’s index — so UI can look correct even when Trove’s DB is sparse. |

### What “backfill metadata later” means

This is **index enrichment**, not re-upload:

1. For each `ArchiveEntry` (by `track_id` / `sha256`), read tags from:
   - **Preferred:** original local path (`source_path_original`) if the library is still mounted, or
   - **Fallback:** download the object from the bucket and parse in place / temp file.
2. Run a real extractor (planned: `lofty` or similar behind `MetadataExtractor`).
3. **`ArchiveDb::upsert`** — update `metadata`, optionally `artwork_object_key`, tags.
4. **`push_index`** — advance canonical `archive-index.jsonl` + `schema-version.json`.

Objects in S3 **do not change** if `sha256` is unchanged. `track_id` stays stable.

### Scope when implemented (post-backfill)

| In scope | Out of scope for v1 extractor |
| --- | --- |
| artist, album, title, year, genre from tags | BPM/key analysis (analyzer / ADR 002) |
| duration, comment where present | Mixxx DB import/export |
| optional embedded cover → `artwork_object_key` or sidecar policy | Re-encoding or rewriting audio tags on export |
| forward path: extract on **new** imports at commit time | |

### Operator impact until then

- **`trove query --artist` / `--album`** — unreliable; don’t depend on them for migration QA.
- **USB folder names** — may not match tags; Mixxx display can still be correct.
- **Archive safety** — unaffected; dedupe and resume work on content hash.

### Related gaps (same phase)

| Gap | Notes |
| --- | ----- |
| Compare-and-swap on `schema-version.json` | Concurrent importers / backfill jobs can race on index push (ADR 001 follow-up). |
| Streaming / disk-backed `ObjectStore::put` | Import reads whole files into memory before upload. |
| Async core or blocking-native S3 client | Sync-over-async bridge smell (ADR 003 §2). |

---

## Analyzer (ADR 002)

| Gap | Notes |
| --- | ----- |
| Analyzer toolkit | Artwork curation, name reconciliation, audio analysis — all deferred to post-genesis analyzer. |
| `trove analyze artwork --review` | Illustrative only in ADR 002; not implemented. |

---

## Changelog

| Date | Change |
| --- | --- |
| 2026-07-08 | Created ADR 999. Added playlist `show` gap and runbooks-for-all-command-sets note. Seeded from ADR 001–006 and recent implementation work. |
| 2026-07-08 | Added tag extraction / metadata backfill section: defer until after archive backfill; index enrichment via upsert + push_index without re-upload. |
| 2026-07-08 | Reconciled early genesis gap analysis: closed import/sync/export items; added archive verify, bucket index form, UI workflow table, packaging gaps, revised priority. |
