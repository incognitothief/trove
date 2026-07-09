# Playlist runbook

Manage **logical playlists** in host-local `playlists.sqlite`. Playlists are
Trove's working crates; portable `.m3u8` files are produced at export/sync time.

## Commands

```bash
bin/trove playlist create <name>
bin/trove playlist add <name> <track-id> [track-id …]
bin/trove playlist remove <name> <track-id>
bin/trove playlist list
bin/trove playlist export <name> [--format m3u8|m3u]
```

There is **no** `playlist show` or `playlist delete` yet (ADR 999). Use
`playlist list` + `playlist export`, or the daemon `GET /playlists/:name` for
inspection.

## Happy path: build a crate

```bash
# 1. Find tracks
bin/trove query --text "berlin" --limit 30

# 2. Create and populate
bin/trove playlist create friday-set
bin/trove playlist add friday-set trk_abc123 trk_def456

# 3. Confirm it exists
bin/trove playlist list
```

## Export to stdout (preview)

```bash
bin/trove playlist export friday-set
```

Writes M3U body to stdout. Paths are **relative** (Mixxx-friendly) per
`[mixxx].relative_paths` in config.

**Caveats:**

- `--format m3u8|m3u` is accepted but export currently follows
  `[export].playlist_format` in config — the flag is misleading until wired.
- Stdout export is a **preview** — paths assume the configured volume layout,
  not a file on disk. For a Mixxx-importable file on a USB drive, use
  `sync playlist` (writes `Playlists/<name>.m3u8` on the volume).

```bash
bin/trove playlist export friday-set --json
# → { "body": "...", "relative_paths": [...] }
```

## Remove a track

```bash
bin/trove playlist remove friday-set trk_abc123
```

## JSON output

```bash
bin/trove --json playlist list
bin/trove --json playlist export friday-set
```

## Recovery

Playlists are **not** pushed to the bucket today (`playlists.jsonl` is a known
gap). Back up `~/.trove/playlists.sqlite` if crates matter.

After losing `playlists.sqlite`:

1. Rebuild crates from query results (if you have track ids elsewhere).
2. Or restore from backup.

## Two export paths (mental model)

| Command | Output | Use when |
| --- | --- | --- |
| `playlist export` | stdout (or JSON) | Preview paths; pipe to a file manually |
| `sync playlist --to <mount>` | bytes on volume + `Playlists/*.m3u8` | **Flash a performance drive** |

A design revisit is planned (ADR 999) to reduce confusion between these paths.

## See also

- [Query runbook](03-query.md) — find track ids
- [Sync runbook](06-sync.md) — flash playlist to USB
- [ADR 006 — playlist export](../adr/006-export-playlist-and-volume-sync.md)
