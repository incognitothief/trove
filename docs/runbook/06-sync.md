# Sync runbook

Flash tracks from the archive onto a mounted performance volume. Sync downloads
canonical bucket objects and writes human-facing paths under `Music/` per
`[export].layout` in config. Playlist sync also writes `Playlists/<name>.m3u8`.

## Commands

```bash
bin/trove sync playlist <name> --to <mount> [--plan]
bin/trove sync query --to <mount> [--plan] [query filters…]
bin/trove sync resume
bin/trove sync verify <mount> [--playlist <name> | query filters…]
```

Prerequisite: `volume init` on the mount (see [Volume runbook](05-volume.md)).

## Happy path: flash a playlist

```bash
# 1. Optional dry-run
bin/trove sync playlist tonight --to /Volumes/DJ_USB --plan

# 2. Execute — transfers files + writes Playlists/tonight.m3u8
bin/trove sync playlist tonight --to /Volumes/DJ_USB
# → synced 'tonight' -> /Volumes/DJ_USB: N transferred, 0 failed, M skipped

# 3. Verify
bin/trove sync verify /Volumes/DJ_USB --playlist tonight
# → verify /Volumes/DJ_USB: N present, 0 missing, 0 stale
```

`--plan` prints transfers without writing:

```text
sync plan for 'tonight' -> /Volumes/DJ_USB: 12 transfer(s), 450000000 byte(s), 3 already present
  music/<sha>.flac -> Music/Artist/Album/Title.flac
  …
```

Skipped = same `sha256` already on volume at the expected path.

## Query sync (plan only)

Preview sync for a query — **execution is not wired**:

```bash
bin/trove sync query --to /Volumes/DJ_USB --plan --bpm 120:128 --limit 50
```

Without `--plan`:

```bash
bin/trove sync query --to /Volumes/DJ_USB --text "house"
# error: query sync execution is not wired yet; use --plan to preview
```

**Workaround:** build a playlist, then `sync playlist`:

```bash
bin/trove query --bpm 120:128 --json   # collect track ids
bin/trove playlist create usb-set
bin/trove playlist add usb-set <ids…>
bin/trove sync playlist usb-set --to /Volumes/DJ_USB
```

## Resume interrupted sync

Transfer progress is persisted in `~/.trove/sync.sqlite`:

```bash
bin/trove sync resume
# → resumed sync: N transferred, M failed
```

Resumes the **latest active** sync job for the last mount/playlist. Completed
transfers are not re-downloaded.

If resume fails with `active sync job` not found, re-run the original
`sync playlist` command — already-present files are skipped.

## Verify

```bash
# Whole playlist scope
bin/trove sync verify /Volumes/DJ_USB --playlist tonight

# Query scope
bin/trove sync verify /Volumes/DJ_USB --text "detroit" --limit 100
```

Reports `present`, `missing`, and `stale` counts. Use after unplug/replug or
suspected copy errors.

## Export layout

Controlled by `~/.trove/config.toml`:

```toml
[export]
layout = "artist-album"   # Music/Artist/Album/Title.ext
# layout = "flat"        # Music/Title.ext
playlist_format = "m3u8"

[mixxx]
relative_paths = true
```

Download source is always `music/<sha256>.<ext>` in the bucket. Destination
paths use metadata (often sparse) — see ADR 999 for tag extraction status.

## JSON output

```bash
bin/trove --json sync playlist tonight --to /Volumes/DJ_USB --plan
bin/trove --json sync playlist tonight --to /Volumes/DJ_USB
```

## End-to-end: lost drive before a gig

```bash
bin/trove archive pull-index
bin/trove volume init /Volumes/NEW_USB --label EmergencyGig
bin/trove sync playlist tonight --to /Volumes/NEW_USB
bin/trove sync verify /Volumes/NEW_USB --playlist tonight
# Eject; open Mixxx; import Playlists/tonight.m3u8
```

## Troubleshooting

| Symptom | Action |
| --- | --- |
| `volume identity on mount` not found | Run `volume init` first |
| Transfers fail | Check bucket access; `sync resume` after fixing network |
| Mixxx paths broken | Ensure `relative_paths = true`; import `.m3u8` from `Playlists/` |
| `Unknown Artist` folders | Expected until metadata backfill; Mixxx reads embedded tags |
| Large library slow | Full-object download today — no streaming get yet |

## `playlist export` vs `sync playlist`

| | `playlist export` | `sync playlist` |
| --- | --- | --- |
| Writes audio files | No | Yes |
| Writes `Playlists/*.m3u8` on volume | No | Yes |
| Mixxx-ready on USB | No (stdout preview) | **Yes** |

## See also

- [Volume runbook](05-volume.md) — init, status, diff
- [Playlist runbook](04-playlist.md) — build crates
- [ADR 006 — sync design](../adr/006-export-playlist-and-volume-sync.md)
