# Prerequisites and setup

Everything you need before running any Trove CLI workflow.

## Install and build

```bash
cd trove
make deps
make build
```

For a real S3 bucket, build with the S3 feature:

```bash
make build CARGO_FEATURES=s3
# or per-invocation:
CARGO_FEATURES=s3 bin/trove archive pull-index
```

## Invoke the CLI

**Recommended:** use the `bin/trove` wrapper from the repo root (or add `bin/`
to your `PATH`):

```bash
bin/trove --help
bin/trove import --help
```

**Alternative:** `make cli` — place `--` before any flag that starts with `-`:

```bash
make cli query -- --artist "Theo Parrish"
make cli import ~/Music -- --plan   # invalid today; use subcommands instead
```

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `TROVE_HOME` | `~/.trove` | Local cache, config, SQLite databases |
| `TROVE_BUCKET_DIR` | `~/.trove/bucket-sim` | Simulated bucket directory (filesystem store only) |
| `CARGO_FEATURES` | (none) | Set to `s3` on `bin/trove` / `make cli` for real buckets |
| `RUST_LOG` | `warn` | Increase verbosity, e.g. `RUST_LOG=trove_core=info` |

Isolate a demo environment:

```bash
export TROVE_HOME=/tmp/trove-demo/home
export TROVE_BUCKET_DIR=/tmp/trove-demo/bucket
bin/trove archive pull-index
```

## Configuration

Trove reads `~/.trove/config.toml`. If the file is missing, it synthesizes a
local-only default (`region = "local"`, filesystem bucket simulator).

### Local-only (no AWS)

No config file required. Optional explicit config:

```toml
# ~/.trove/config.toml
[bucket]
name = "local"
region = "local"
```

Objects live under `TROVE_BUCKET_DIR` (default `~/.trove/bucket-sim`).

### Real AWS S3

1. Create a bucket; note **name** and **region**.
2. Verify credentials outside Trove: `aws sts get-caller-identity`, `aws s3 ls`.
3. Copy `config.example.toml` to `~/.trove/config.toml`:

```toml
[bucket]
name = "your-bucket"
region = "us-east-1"
prefix = ".trove"
music_prefix = "music"
```

4. Build and run with `--features s3` (see above).
5. Smoke test: `bin/trove archive pull-index` — a new bucket reports
   `EmptyBucket`; after the first import, `UpToDate { generation: N }`.

### S3-compatible endpoints (MinIO, R2, etc.)

Add `endpoint` to the bucket section:

```toml
[bucket]
name = "trove-test"
region = "us-east-1"
endpoint = "https://s3.example.com"
```

## Local state layout

Everything under `TROVE_HOME` is **disposable** except as a performance cache.
The bucket is the source of truth.

| Path | Purpose |
| --- | --- |
| `config.toml` | Bucket and export settings |
| `archive.sqlite` | Local archive index cache |
| `playlists.sqlite` | Logical playlists (host-local today) |
| `sync.sqlite` | Import jobs and sync transfer progress |
| `volumes/{id}.sqlite` | Per-drive sync state (host-side) |
| `cache/` | Pulled index snapshots, import manifests |
| `bucket-sim/` | Simulated bucket (filesystem store only) |

## Mental model

1. **Reconcile first** — `query`, `sync`, and `playlist export` reconcile with
   the bucket (via `archive pull-index` semantics) unless `--offline`.
2. **Import is staged** — upload/verify happen before `commit` advances the
   canonical index.
3. **Volumes are presentation** — bucket keys are content-addressed; USB paths
   come from metadata and `[export].layout`.

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `no S3 support` | Real bucket config, build without `s3` feature | `CARGO_FEATURES=s3 bin/trove …` |
| `could not read .trove/schema-version.json` | Network/credentials | Fix AWS access; or use `--offline` for read-only |
| `mount point … not found` | Drive not mounted | Plug in USB; check `/Volumes/…` path |
| Sparse artist/album in query/USB folders | Tag extraction deferred | See [ADR 999](../adr/999-known-gaps-and-follow-ups.md#tag-extraction-and-index-metadata-adr-002--adr-004) |
