# ADR 004: Content-Addressed Canonical Audio Objects

- **Status:** Proposed
- **Date:** 2026-07-08
- **Deciders:** Trove maintainers
- **Supersedes:** —
- **Superseded by:** —
- **Relates to:** [ADR 000](000-bootstrap.md), [ADR 001](001-bootstrap-implementation.md), [ADR 003](003-s3-object-store.md)

## Context

Trove's current import pipeline commits verified objects into the canonical
`music/` namespace using the source file's basename:

- staging object: `.trove/staging/{import-job-id}/{sha256}/{filename}`
- committed object: `music/{filename}`

That was acceptable as a bootstrap shortcut, but it is not a safe canonical
layout for the durable archive.

Three problems fall out of the current design:

1. **Filename collisions can overwrite the archive object.** Two different
   tracks from different folders can legitimately share the same basename
   (`01 - Intro.mp3`, `track01.flac`, etc.). When both commit to
   `music/{filename}`, the later import overwrites the earlier object's bytes.
2. **Duplicate detection is only per import job, not archive-wide.** The
   current planner marks duplicates only within one scan by comparing SHA-256s
   discovered in that job. It does not reuse an already-imported canonical
   object from the existing archive.
3. **The canonical object key is coupled to a user-facing filename.** Source
   names may contain spaces, brackets, casing differences, unicode, or naming
   conventions we later regret. S3 can store those keys, but a durable object
   identity should not depend on a mutable presentation detail.

The root issue is conceptual: **a canonical bucket key is an identity, not a
label.** Trove already computes a strong content hash (`sha256`) for every
imported audio file at import time — by reading the local source bytes and
hashing them client-side before upload. That hash is the natural identity of the
canonical object; the original filename and path should remain metadata and
export concerns.

## Decision

### 1. Canonical committed audio objects become content-addressed

Committed audio objects in the durable archive will no longer use the source
basename. Instead, Trove will store them under a flat key derived from the
file's SHA-256:

```text
music/<sha256>.<ext>
```

Examples:

```text
music/896bed36c49d8cb9ce5fb9d9b87a06b2b515db023040a0ecaea8ccf3ab194d4b.mp3
music/a38c906a9aa307a4737a6a1d15029de40d1b93e20d017b6ec0943976e2981a1e.flac
```

The extension is kept only as a convenience for operator readability and
content-type intuition. The **hash** is the identity; the extension is not.

This matches the existing artwork layout (`artwork/<sha256>.<ext>`) — one
content-addressed object per hash, no extra namespace labels in the path.

The hash is computed **client-side during import** (scan/plan), not taken from
S3 after upload. Trove reads the source file, hashes the bytes, then uses that
digest for dedupe, staging paths, the archive index, and the canonical object
key. S3's `ETag` is opaque metadata only; it is not used as the content
identity.

This means:

- the canonical object key is stable
- the object key does not depend on the source path or presentation name
- two different filenames with identical bytes resolve to the same canonical
  object key
- two different files can never collide unless they share the same SHA-256

### 2. No extra path segments (no `sha256/` label, no fanout)

The canonical key is intentionally flat under `music/`. We are **not** adding:

- a `sha256/` namespace folder (e.g. `music/sha256/<hash>.mp3`) — redundant;
  the hash in the filename already identifies the addressing scheme, and
  artwork does not use such a label either
- fanout/sharding prefixes (e.g. `music/89/<hash>.mp3`) — unnecessary for
  correctness at our scale; S3 handles large flat prefixes well enough for
  now

If bucket scale or tooling later gives us a concrete reason to shard, we can
introduce it as a separate optimization ADR.

### 3. Duplicate handling becomes archive-wide

Import will treat the SHA-256 as the canonical object identity across the whole
archive, not only within the current job.

Before committing a verified file into `music/<sha256>.<ext>`, Trove will
check the existing archive index for that SHA-256:

- **If the SHA already exists in the archive**, Trove will **reuse** the
  existing canonical `object_key` and will not write a second physical object.
- **If the SHA is new**, Trove will promote the verified staging object into the
  canonical hash-derived key and write the new archive entry normally.

This makes the durable bucket effectively **one canonical object per content
hash**.

### 4. Filenames move to metadata and export concerns

The original source name remains useful, but not as canonical storage identity.
Trove will continue to keep:

- `source_path_original`
- extracted metadata (`title`, `artist`, `album`, etc.)
- inferred/display naming from later analyzers

Those are the right sources for:

- UI display
- search
- playlist exports
- volume sync layouts (`Music/Artist/Album/Track.ext`)

The bucket key should remain an infrastructure detail and should not be treated
as the user-facing track name.

### 5. Logical entry policy remains separate from object identity

This ADR decides **physical object identity**, not the final catalog policy for
"duplicate tracks" as user-facing entries.

For the first cut:

- Trove will prevent duplicate **physical objects** in the bucket by reusing the
  canonical object for an already-known SHA.
- Trove may still decide, in implementation, whether a repeat import of the same
  SHA creates:
  - no new archive row at all, or
  - a new logical row that reuses the existing `object_key`

The recommended default is to **not** create a second archive row for the same
SHA unless we later identify a strong use case for multiple logical entries with
shared bytes.

That exact row-level policy is intentionally left to implementation detail in
this pass; the non-negotiable part is that the bucket does not gain duplicate
physical objects and does not use basenames as canonical keys.

## Consequences

### Positive

- **No filename collisions in the durable archive.** Two unrelated tracks named
  `01 - Intro.mp3` no longer fight over one canonical object key.
- **Canonical identity becomes stable and audit-friendly.** The object key now
  says what the object *is* (content hash), not what it happened to be called on
  one source device.
- **Archive-wide dedupe becomes straightforward.** Re-importing the same bytes
  becomes a lookup and reuse, not another object write.
- **Source naming becomes safely disposable.** Weird or messy filenames no
  longer shape the long-term bucket layout.
- **The UI/export path stays flexible.** Human-readable filenames can still be
  derived at export time without constraining durable storage.
- **Consistent with artwork.** Audio and cover art both use `<prefix>/<hash>.<ext>`
  with no redundant labels in the path.

### Negative / trade-offs

- **Bucket paths become less human-friendly.** `music/<hash>.mp3` is far uglier
  to browse manually than `music/Track Name.mp3`.
- **Import needs archive lookups before commit.** The pipeline must consult the
  existing index by SHA-256, which slightly increases implementation complexity.
- **The row-level duplicate policy still needs a call.** "Reuse existing object"
  is settled here; "create or skip a second logical entry" is not fully pinned
  down yet.
- **Extension handling is slightly squishy.** The extension is retained for
  convenience, but the hash is the true identity. If we later normalize or
  transcode audio, this relationship needs revisiting.

## Considered alternatives

- **Keep basename-derived canonical keys and merely sanitize them better.**
  Rejected: sanitization may make keys prettier, but it does not solve the real
  failure mode — collisions and overwrites — and still ties durable identity to
  a presentation label.
- **Use path-derived canonical keys (`Artist/Album/Track.ext`).** Rejected:
  it preserves more human meaning than a bare filename, but it still makes the
  archive key depend on naming conventions, path quality, and future metadata
  cleanup. It also does not solve duplicate bytes imported from different source
  layouts.
- **Add a `sha256/` namespace folder under `music/`.** Rejected: redundant with
  the hash in the object name; inconsistent with `artwork/<hash>.<ext>`; adds
  path length and explanation cost with no correctness benefit.
- **Use content-addressed keys with fanout/sharding now.** Deferred: valid as a
  future scaling optimization, but unnecessary for correctness and harder to
  explain than the flat `music/<sha256>.<ext>` form.
- **Use opaque UUID object keys.** Rejected: removes filename collisions, but
  loses the natural dedupe and auditability benefits of the already-computed
  SHA-256.

## Scope

- **This ADR authorizes:** changing the canonical committed audio namespace from
  `music/{filename}` to `music/<sha256>.<ext>`; treating SHA-256 (computed
  client-side at import) as the archive-wide physical object identity; reusing
  an existing canonical object when the same SHA is re-imported; and keeping
  filenames as metadata/export concerns rather than bucket identity.
- **Explicitly not in scope:** a full redesign of the logical track/catalog
  model for repeat imports of the same SHA; any transcode/normalization policy;
  namespace labels (`music/sha256/...`); or fanout/sharded hash prefixes unless
  scale later justifies it.
