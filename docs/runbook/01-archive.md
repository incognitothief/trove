# Archive runbook

Manage the **canonical archive index** in the bucket. Local `archive.sqlite`
is a disposable cache; reconciliation keeps it aligned with
`schema-version.json` and `archive-index.jsonl` in the bucket.

## Commands

```bash
bin/trove archive pull-index    # reconcile local cache ← bucket
bin/trove archive push-index    # push local index → bucket (new generation)
bin/trove archive verify        # NOT IMPLEMENTED (stub)
```

Global flags: `--json`, `--offline` (see [README](README.md#global-flags)).

## Happy path: new machine

After configuring `~/.trove/config.toml` and S3 credentials:

```bash
bin/trove archive pull-index
```

Expected outcomes:

| Report | Meaning |
| --- | --- |
| `EmptyBucket` | Fresh bucket — no index yet. Import first. |
| `Rehydrated { generation, entries_loaded }` | Pulled index from bucket into local cache |
| `UpToDate { generation }` | Local cache already matches bucket |

Then query or sync as usual — those commands reconcile automatically, but an
explicit `pull-index` is useful after manual bucket changes or on a new laptop.

## Happy path: after import commit

Import `commit` calls `push_index` internally. You normally do **not** need a
manual `push-index` after a successful import.

Use `push-index` when you have **local-only** archive changes that must become
canonical (e.g. future metadata backfill tools that upsert `archive.sqlite`
locally):

```bash
bin/trove archive push-index
# → pushed canonical index at generation N
```

**Warning:** concurrent `push-index` from multiple machines can race on
`schema-version.json` (known gap — see ADR 999). Serialize pushes until
compare-and-swap lands.

## Offline reads

When the bucket is unreachable but you have a prior cache:

```bash
bin/trove archive pull-index --offline
# → OfflineFallback { local_generation: Some(N) }

bin/trove query --text "house" --offline
```

Staleness is possible — you are serving the last pulled generation.

## Recovery scenarios

### Lost `~/.trove` cache

```bash
rm -rf ~/.trove/archive.sqlite ~/.trove/cache   # optional clean slate
bin/trove archive pull-index
```

All indexed tracks reappear after rehydration. Playlists in `playlists.sqlite`
are **not** in the bucket today — those are lost unless you have a backup of
`~/.trove` (known gap).

### Suspect local index drift

1. `bin/trove archive pull-index` — force rehydrate from bucket.
2. If still wrong, inspect bucket objects under `.trove/` prefix manually.
3. `archive verify` (when implemented) will checksum every indexed object.

## `archive verify` (not implemented)

```bash
bin/trove archive verify
# error: archive verify is not implemented in this bootstrap yet
```

Planned behavior: every `sha256` in the index exists in the bucket; optional
byte re-read vs hash; report orphans and drift. Use `import verify <job-id>` for
in-flight staging objects only.

## JSON output

```bash
bin/trove --json archive pull-index
```

Emits the `ReconcileReport` debug form on stdout.

## See also

- [Import runbook](02-import.md) — how the index advances during backfill
- [ADR 000 — bootstrap architecture](../adr/000-bootstrap.md)
- [ADR 999 — archive verify gap](../adr/999-known-gaps-and-follow-ups.md#archive-integrity-and-bucket-index-form)
