# Library runbook

Manage the declared **library root** — the stable anchor cross-drive
identity is computed relative to (ADR 007, Group D1). This is a growing
command family: `root` and `backfill-slugs` exist today; the shape scan and
backfill Plan (ADR 007 Groups E1/E2) will live here too.

```bash
bin/trove library root                        # show the current root
bin/trove library root --set <path>            # declare or change it
bin/trove library backfill-slugs               # backfill existing entries
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

## See also

- [ADR 007 — desktop shell, IO durability, multi-device backfill coordination](../adr/007-tauri-shell-and-io-durability.md)
- [Archive runbook](01-archive.md) — `push-index`, `verify`
- [Import runbook](02-import.md) — the fingerprint cache and reconcile-before-import behavior this same ADR added
