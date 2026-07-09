# Query runbook

Search the archive. Every query **reconciles** the local cache with the bucket
first (same as `archive pull-index`), then runs against `archive.sqlite`.

## Command

```bash
bin/trove query [filters…]
```

## Filters

| Flag | Example | Notes |
| --- | --- | --- |
| `--text` | `--text "deep house"` | Free-text search |
| `--artist` | `--artist "Theo Parrish"` | **Unreliable** until tag extraction ships |
| `--album` | `--album "First Floor"` | **Unreliable** until tag extraction ships |
| `--genre` | `--genre house` | Sparse metadata |
| `--key` | `--key Am` | Sparse metadata |
| `--bpm` | `--bpm 118:124` | Min:max range; either side optional (`118:` or `:124`) |
| `--limit` | `--limit 50` | Cap result count |

Combine flags — all provided filters must match (AND semantics).

## Happy path

```bash
# Reconcile + search
bin/trove query --text "motorcity"

# BPM range for a set
bin/trove query --bpm 120:128 --genre house --limit 100
```

Human output (one track per line):

```text
<track-id>  ? - Track Title [128 BPM Am]
```

`?` appears when artist metadata is missing (common today).

## Offline query

When the bucket is down but you have a cached index:

```bash
bin/trove query --text "warehouse" --offline
```

## Use query results elsewhere

Track ids from query output feed playlists and volume diff:

```bash
bin/trove query --text "detroit" --limit 20 --json | jq -r '.[].track_id'
bin/trove playlist add my-crate <track-id> …
```

Query-sourced **sync execution** is not wired yet — use `sync query --plan`
to preview, then build a playlist for actual flash (see [Sync runbook](06-sync.md)).

## JSON output

```bash
bin/trove --json query --artist "Moodymann" --limit 10
```

Returns an array of `ArchiveEntry` objects with `track_id`, `metadata`,
`object_key`, etc.

## Metadata limitations

Until post-backfill tag extraction (ADR 002 / ADR 999):

- `--artist` and `--album` filters often return empty or sparse results.
- Prefer `--text` (matches title/filename-derived fields).
- USB folder names may show `Unknown Artist/Unknown Album` while Mixxx still
  reads embedded tags from the file bytes.

## Troubleshooting

| Symptom | Action |
| --- | --- |
| `no matches` | Try `--text`; verify import committed; run `archive pull-index` |
| Reconcile error | Check bucket credentials; or `--offline` |
| Slow first query | Expected — pulls index on first reconcile after stale cache |

## See also

- [Playlist runbook](04-playlist.md) — build crates from query results
- [Sync runbook](06-sync.md) — `sync query --plan`
- [ADR 999 — tag extraction deferred](../adr/999-known-gaps-and-follow-ups.md#tag-extraction-and-index-metadata-adr-002--adr-004)
