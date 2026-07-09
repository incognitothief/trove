# Volume runbook

Prepare and inspect **performance volumes** (USB/SSD). The drive stays portable:
`Music/`, `Playlists/`, and `.trove-volume.json` only. Per-volume sync state
lives host-side in `~/.trove/volumes/{volume_id}.sqlite`.

## Commands

```bash
bin/trove volume init <mount> [--label <name>]
bin/trove volume status <mount>
bin/trove volume list
bin/trove volume diff <mount> [--playlist <name> | query filters…]
```

## Happy path: new USB drive

```bash
# 1. Plug in drive — confirm mount point exists
ls /Volumes/DJ_USB

# 2. Initialize layout + identity
bin/trove volume init /Volumes/DJ_USB --label GigDrive
# → initialized volume <volume-id> at /Volumes/DJ_USB

# 3. Flash tracks (see sync runbook)
bin/trove sync playlist tonight --to /Volumes/DJ_USB

# 4. Inspect
bin/trove volume status /Volumes/DJ_USB
```

`init` creates:

```text
/Volumes/DJ_USB/
  .trove-volume.json    # volume id + optional label
  Music/                # audio files (filled by sync)
  Playlists/            # .m3u8 files (filled by sync)
```

Re-running `init` on an already-initialized volume returns the existing identity
(idempotent).

## Status

```bash
bin/trove volume status /Volumes/DJ_USB
```

```text
volume <id> (label: GigDrive)  copied=N pending=P stale=S failed=F
```

Counts come from the host-side volume DB, updated during sync/verify.

## List known volumes (host)

Volumes you have initialized on this machine:

```bash
bin/trove volume list
```

Shows volume id and label from `~/.trove/volumes/*.sqlite`. Does not require
the drive to be plugged in.

## Diff (plan without transferring)

Compare what **would** need syncing without writing bytes:

```bash
# Against a playlist
bin/trove volume diff /Volumes/DJ_USB --playlist tonight

# Against a query
bin/trove volume diff /Volumes/DJ_USB --text "warehouse" --limit 50
```

Output states per track: `present`, `missing`, `stale`, etc.

Use before a long sync to estimate transfer size, or to audit a drive after a
partial flash.

## Recovery scenarios

### New laptop, same USB drive

The drive has `.trove-volume.json` but this host has no volume DB:

```bash
bin/trove volume init /Volumes/DJ_USB
# recreates host-side DB; existing Music/ files remain

bin/trove volume diff /Volumes/DJ_USB --playlist tonight
bin/trove sync playlist tonight --to /Volumes/DJ_USB   # fills gaps
```

### Drive reformatted

```bash
bin/trove volume init /Volumes/DJ_USB --label GigDrive
bin/trove sync playlist tonight --to /Volumes/DJ_USB
```

Old host volume DB rows may be stale — `init` on a blank drive gets a new
volume id.

### Wrong mount path

macOS mount points vary (`/Volumes/DJ_USB` vs `/Volumes/DJ_USB 1`). Always
pass the current path; identity is read from `.trove-volume.json` on the drive.

## JSON output

```bash
bin/trove --json volume init /Volumes/DJ_USB --label GigDrive
bin/trove --json volume status /Volumes/DJ_USB
bin/trove --json volume diff /Volumes/DJ_USB --playlist tonight
```

## Known gaps

| Gap | Notes |
| --- | --- |
| Volume prune | No host-side cleanup of stale DB rows / orphan files |
| Folder names | `Unknown Artist/Unknown Album` common until tag extraction |

## See also

- [Sync runbook](06-sync.md) — transfer bytes and write playlists
- [ADR 006 — volume layout](../adr/006-export-playlist-and-volume-sync.md)
