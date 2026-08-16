# Trove CLI runbooks

Step-by-step operator workflows for the `trove` command-line client. Each
runbook covers the happy path, resume/recovery, and known limitations.

**Architecture and design rationale** live in the ADRs (`docs/adr/`). These
runbooks are copy-paste workflows for day-to-day use.

## Before you start

Read [Prerequisites and setup](00-prerequisites.md) once — config, storage
backends, and how to invoke the CLI.

## Command families

| Runbook | Commands | Typical use |
| --- | --- | --- |
| [Archive](01-archive.md) | `trove archive pull-index`, `push-index`, `verify` | Sync local cache with the bucket; advance the canonical index |
| [Import](02-import.md) | `trove import …` | Backfill a music library into the durable archive |
| [Query](03-query.md) | `trove query …` | Search the archive after reconciliation |
| [Playlist](04-playlist.md) | `trove playlist …` | Build logical crates; export portable playlist files |
| [Volume](05-volume.md) | `trove volume …` | Prepare and inspect performance USB/SSD drives |
| [Sync](06-sync.md) | `trove sync …` | Flash tracks and playlists to a mounted volume |
| [Library](07-library.md) | `trove library …` | Declare the library root; backfill cross-drive identity for existing content |

## End-to-end operator flows

### New laptop, existing bucket

```bash
# 1. Configure ~/.trove/config.toml and build with --features s3 if needed
bin/trove archive pull-index

# 2. Search and build a crate
bin/trove query --text "house"
bin/trove playlist create tonight
bin/trove playlist add tonight <track-id> …

# 3. Flash a drive
bin/trove volume init /Volumes/DJ_USB --label GigDrive
bin/trove sync playlist tonight --to /Volumes/DJ_USB
bin/trove sync verify /Volumes/DJ_USB --playlist tonight
```

### First-time archive backfill

```bash
# Dry-run, then staged import with resume support
bin/trove import plan ~/Music
bin/trove import run <job-id>
bin/trove import commit <job-id>

# Or one-shot (interruptible — use import list + resume if it stops)
bin/trove import ~/Music
```

### Lost job id after interrupted import

```bash
bin/trove import list
bin/trove import status <job-id>
bin/trove import resume <job-id>
bin/trove import commit <job-id>
```

## Global flags

These apply to every subcommand:

| Flag | Effect |
| --- | --- |
| `--json` | Machine-readable JSON on stdout (progress still goes to stderr in human mode) |
| `--offline` | Use the last cached index when the bucket is unreachable |

## Related docs

- [README — config runbook](../README.md#config-runbook) (duplicate of prerequisites; kept for discoverability)
- [ADR 999 — known gaps](../adr/999-known-gaps-and-follow-ups.md)
- [ADR 005 — durable import](../adr/005-durable-import-and-resume.md)
- [ADR 006 — volume sync](../adr/006-export-playlist-and-volume-sync.md)
