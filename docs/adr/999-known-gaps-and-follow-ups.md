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
| Richer `import status` during fingerprint | No hashed/pending counts while fingerprinting; only visible via progress stderr. |
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

## Archive / storage

| Gap | Notes |
| --- | ----- |
| Compare-and-swap on `schema-version.json` | Concurrent importers can race (ADR 001 follow-up). |
| Real metadata extraction | Stub extractor only; weak artist/album folders on export. |
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
