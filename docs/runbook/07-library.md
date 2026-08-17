# Library runbook

Manage the declared **library root** — the stable anchor cross-drive
identity is computed relative to (ADR 007, Group D1). This is a growing
command family: `root`, `backfill-slugs`, and `shape` exist today; the
backfill Plan (ADR 007 Group E2) will live here too.

```bash
bin/trove library root                        # show the current root
bin/trove library root --set <path>            # declare or change it
bin/trove library backfill-slugs               # backfill existing entries
bin/trove library shape [<path>]               # inspect structure (defaults to the declared root)
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

## See also

- [ADR 007 — desktop shell, IO durability, multi-device backfill coordination](../adr/007-tauri-shell-and-io-durability.md)
- [Archive runbook](01-archive.md) — `push-index`, `verify`
- [Import runbook](02-import.md) — the fingerprint cache and reconcile-before-import behavior this same ADR added
