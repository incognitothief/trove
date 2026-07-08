# ADR 002: Import Enhancements and the Analyzer Toolkit

- **Status:** Proposed
- **Date:** 2026-07-07
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000](000-bootstrap.md), [ADR 001](001-bootstrap-implementation.md)

## Context

The bootstrap import pipeline (ADR 001) scans a folder, hashes and dedupes,
uploads to staging, verifies, commits into `music/`, and writes archive index
entries. Metadata is currently filename-only, and the committed layout uses the
bare filename.

Real archives are messier than that. A representative entry from a live import:

```json
{
  "object_key": "music/17 - Sigh. [Explicit].mp3",
  "metadata": { "title": "17 - Sigh. [Explicit]", "artist": null, "album": null, "file_type": "mp3" },
  "source_path_original": "/Volumes/T72/music/library/Ahwlee/1991 [Explicit]/17 - Sigh. [Explicit].mp3"
}
```

Two problems show up immediately:

1. **Junk gets imported.** Archive folders carry non-music files — `.zip`/`.rar`
   bundles, cover images, `.cue`/`.nfo`/`.m3u` sidecars — and external drives
   (especially macOS) add dotfiles like `.DS_Store`, `.Trashes`, and AppleDouble
   `._*` files that even share an audio extension. None should become archive
   objects.
2. **Structure in the path is thrown away.** The path clearly encodes
   `artist = Ahwlee`, `album = 1991 [Explicit]`, and a track title, but none of
   it lands in the index. Folder conventions are also inconsistent: one archive
   may mix `artist/album/track`, `album/track`, `artist/track`, and loose files,
   and the same artist may appear under differently-spelled folders.

Separately, we want a place to *derive* richer metadata (BPM, key, loudness,
cover art, …) after import without bloating or destabilizing the import path.

The key realization shaping this ADR: import **already durably records
`source_path_original`**. So nothing about names needs to happen at import time —
path-derived metadata and name reconciliation can be produced later, from data
we already keep, as a thoughtful post-import action. That lets import stay lean
and avoids growing a thicket of import CLI flags (layout hints, strip toggles,
…). We therefore split the work:

- **Import changes** are kept to the minimum that *must* happen at ingest time:
  ingest hygiene (music-only, no dotfiles) and capturing cover art — the latter
  because folder images exist only on the disposable source.
- **Everything derivable after the fact** — name inference/reconciliation,
  audio analysis, artwork curation — lives in a separate, extensible **analyzer
  toolkit**.

## Decision

### 1. Import scan: music in, noise out, art preserved (defaults)

Near-term import behavior. None of it touches hashing, dedupe, or commit, and it
adds no layout/naming flags.

**Allowlist audio types.** Import selects files by a known set of audio
extensions; make this an explicit guarantee rather than incidental behavior:
anything that is not a recognized audio file is ignored *as a track*. Archives
(`.zip`, `.rar`, `.7z`), sidecars/metadata (`.cue`, `.nfo`, `.m3u`, `.txt`), and
stray images never become archive **tracks**. The allowlist is the single,
auditable knob; the set of things imported *as tracks* is never widened to
arbitrary files.

**Exclude dotfiles / hidden dirs.** Exclude any entry whose final path component
begins with `.`, and do not descend into hidden directories — dropping
`.DS_Store`, `.Trashes`, and AppleDouble `._*` sidecars (which otherwise share an
audio extension) before they can become archive objects. `--include-dotfiles`
opts them back in.

**Capture co-located cover art — a durability measure, not an option.** This is
the one deliberate exception to "music only," and it exists for exactly the
reason we upload the audio at all: the source device is disposable. Folder images
(`cover.jpg`, `front.png`, …) live *only* on the source and cannot be recovered
once the drive is gone, so import captures them **while it still can** — the same
class of guarantee as recording `source_path_original`. Kept dumb:

- For every folder an imported track came from, also capture the image files in
  that folder as **content-addressed art objects** (`artwork/<sha256>.<ext>`),
  recording only which source folder each came from.
- Content addressing dedupes automatically: an album's cover is stored once and
  shared by every track from that folder.
- Import makes **no** decision about *which* image is "the cover," about embedded
  art, or about associations — that curation is deferred to the analyzer (§2c).
- `--no-artwork` exists only as an escape hatch (e.g. re-imports); capture is on
  by default precisely because a miss is unrecoverable.

Embedded art needs no special capture — it already travels inside the audio
bytes and can be extracted post-import at any time.

### 2. Analyzer toolkit (future feature)

An extensible, **post-import** surface for deriving and repairing metadata. It
keeps import lean and rarely-changing while richer, more experimental derivation
evolves independently and is run deliberately by the operator.

Shared shape for every analyzer:

- **Trait seam**, analogous to `MetadataExtractor` / `ObjectStore`: an
  `Analyzer` consumes committed archive entries (with lazy access to audio bytes
  via the object store when needed) and returns metadata/tag enrichments.
- **Runs after the import event**, over a selected set of entries (e.g. a query
  result or the whole archive), writing results back through the facade so the
  canonical index advances normally. Analyzers are **not** wired into the import
  phases.
- **Toolkit, not a monolith:** analyzers are registered and individually
  selectable, so each can ship and change without touching import or each other.
- **Precedence:** an analyzer only overwrites fields it is authoritative for, and
  never clobbers higher-confidence data. Explicitly extracted tags outrank
  derived/inferred values.

Planned analyzers (named, not fully designed here):

#### 2a. Name derivation & reconciliation (from `source_path_original`)

The post-import home for the "inherit names from location" need. Because
`source_path_original` is durable, this analyzer can, at any time and without
re-reading audio:

- **Derive** `artist` / `album` / `title` from the recorded path. Titles come
  from a conservative, reversible cleanup of the filename stem (strip a leading
  track number `^\s*\d{1,3}\s*[-._)]\s*` and trailing bracketed qualifiers like
  `[Explicit]`; `17 - Sigh. [Explicit]` → `Sigh.`).
- **Handle layout ambiguity thoughtfully.** Since `artist/album/track`,
  `album/track`, and `artist/track` coexist, the analyzer does not silently
  commit a single guess. It records path components naively, applies operator-
  chosen interpretation where confident, and marks the rest low-confidence for
  review — an interactive, user-paced operation rather than a blocking import
  step.
- **Cover the awkward path shapes explicitly.** The analyzer must not choke on
  or fabricate data for the real cases that occur in one archive:
  - **Bare `track`** with no ancestor folders → derive a title only; leave
    `artist`/`album` unknown (`null`) rather than inventing them.
  - **Single intermediate dir** (`artist/track` vs `album/track`) → genuinely
    ambiguous which field it is; record naively and flag low-confidence for
    review, never a hard guess.
  - **Self-titled release** where `artist` and `album` components are identical
    (e.g. `rockman/rockman`) → legitimate. Keep `artist = album = "rockman"`;
    never treat the equality as an error, a duplicate, or something for
    reconciliation to collapse.
- **Reconcile duplicates as a batch.** Two folders for the same artist
  (`Ahwlee`, `ahwlee`, `Ahwlee `) are clustered by a normalized key; the analyzer
  proposes merges and, on the operator's confirmation, rewrites the canonical
  index (advancing its generation).
- **Fallback semantics:** derived names only fill empty fields; real extracted
  tags always win. Provenance (distinguishing `tags` vs `path` vs `naive`) is
  recorded so repeated runs and other analyzers know what is safe to overwrite;
  the exact representation (e.g. a small `metadata_source` map vs reserved
  `trove:*` tags) is a follow-up detail. Nothing is lost regardless, since
  `source_path_original` always allows re-derivation.

Framing this as an analyzer (rather than import flags) means the messy,
heuristic, cross-entry work happens deliberately and can be re-run, without
CLI-flag sprawl on the crash-safe import path.

#### 2b. Audio analyzers

BPM detection is the motivating first case; musical key, loudness/ReplayGain, and
waveform generation follow as additional implementations. These read audio bytes
lazily via the object store and enrich the entry.

#### 2c. Artwork curation

Import preserves the raw art bytes (§1); this analyzer turns them into a chosen,
associated cover — none of which needs the source device, since everything is
already in the bucket:

- **Extract embedded art** from bucket audio bytes.
- **Select** the best cover per album from the captured candidates + embedded
  art — name priority (`cover`/`front`/`folder`/`album`), embedded-vs-folder
  precedence with an operator override, recorded as provenance like names.
- **Associate** by setting `artwork_object_key` on entries; album grouping is
  shared with the name analyzer (§2a), so a cover attaches to every track of an
  album at once.
- **Thumbnail** into the disposable `~/.trove/cache/artwork/` for the UI; the
  content-addressed art object in the bucket stays the source of truth.

Illustrative future surface (not implemented here):

```
trove analyze names   --review          # derive + reconcile names, interactively
trove analyze artwork --review          # pick/associate covers from candidates
trove analyze bpm     --query --genre house
trove analyze key     --all
```

This section is **planning only** — no implementation is proposed now beyond
naming the seam and the constraints it must satisfy.

## Consequences

### Positive

- Imports stop ingesting drive junk by default.
- Import stays lean and crash-safety-critical logic rarely changes; all
  heuristic/enrichment work lives behind a separate, extensible seam.
- Name work becomes a deliberate, re-runnable, user-paced action instead of
  import-time flags — no CLI-flag sprawl, and ambiguity never blocks a migration.
- Nothing needed for later name work is lost, because import already records the
  durable `source_path_original` from which everything is re-derivable.
- Cover art survives the disposable source: folder images are captured while the
  drive is mounted (the only time possible) and content-addressed for dedup,
  while the heuristic choice of *which* cover wins is deferred to curation.

### Negative / trade-offs

- Right after import, entries have sparse metadata (title-from-filename only)
  until an analyzer pass is run. This is acceptable given analysis is a conscious
  follow-up step.
- Name derivation remains heuristic; provenance + low-confidence marking +
  operator review contain the blast radius but do not eliminate wrong guesses.
- A new provenance concept adds a small amount of surface to finalize; kept
  optional and minimal on purpose.
- The dotfile default is opinionated; the `--include-dotfiles` escape hatch
  mitigates.

## Considered alternatives

- **Path-name inference as an import-time CLI enhancement** (layout-hint flags,
  strip toggles applied during import). Rejected: it grows the lean import path
  into CLI hell and bakes heuristic guesses into the durable index at ingest
  time. A post-import analyzer is cleaner, re-runnable, and user-paced — and
  loses nothing, since `source_path_original` is already durable.
- **Auto-detect the folder scheme per file at import.** Rejected: unreliable
  across mixed archives, with no audit trail or chance for review.
- **A separate durable "path hints" store.** Rejected as unnecessary:
  `source_path_original` already makes components re-derivable.
- **Resolve name ambiguity interactively at import time.** Rejected: blocks large
  migrations; batch reconciliation after the fact is the intended model.
- **Treat cover art entirely as a post-import analyzer concern (no import
  capture).** Rejected for folder/sidecar images specifically: they live only on
  the disposable source and are unrecoverable once it's gone, so the *bytes* must
  be captured while the source is mounted. Only the curation is deferred.
  (Embedded art genuinely is post-import, since it rides in the audio bytes.)

## Scope

- **Near-term (this ADR authorizes):** import ingests only allowlisted audio
  types as tracks (zips, sidecars, stray files ignored), excludes dotfiles/hidden
  dirs, and captures co-located cover art as content-addressed durability — all
  on by default, with `--include-dotfiles` / `--no-artwork` escape hatches.
- **Future (named, not designed in full):** the analyzer toolkit — including the
  name derivation & reconciliation analyzer, artwork curation, and audio
  analyzers starting with BPM.
