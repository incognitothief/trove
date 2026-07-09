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

Trove reads `~/.trove/config.toml`. The file itself is **optional** — if it is
missing, Trove synthesizes a local-only default (`name = "local"`,
`region = "local"`, filesystem bucket simulator).

**Important:** if `config.toml` **exists but is incomplete or invalid** (for
example, only `name` is set and `region` is missing), Trove **silently ignores
it** and falls back to the same local default. You will not get S3, and import
will copy your entire library into `TROVE_BUCKET_DIR` on the host disk. Always
include **both** `name` and `region` under `[bucket]`, or delete the file and
rely on the explicit local default.

### `config.toml` field reference

#### `[bucket]` — **required section** (when using a config file)

| Field | Required | Default | Notes |
| --- | --- | --- | --- |
| `name` | **yes** | — | S3 bucket name, or `"local"` for filesystem simulator |
| `region` | **yes** | — | AWS region (e.g. `us-east-1`), or `"local"` for simulator |
| `endpoint` | no | (none) | Custom S3-compatible endpoint (MinIO, R2, etc.) |
| `prefix` | no | `.trove` | Prefix for index and staging objects in the bucket |
| `music_prefix` | no | `music` | Prefix for committed audio objects |

**Store selection:** if `region = "local"` **or** `name = "local"`, Trove uses
the filesystem bucket simulator at `TROVE_BUCKET_DIR` (default
`~/.trove/bucket-sim`). Any other `name` + `region` pair requires a build with
the `s3` feature (`CARGO_FEATURES=s3`).

#### `[local]` — optional

| Field | Required | Default | Notes |
| --- | --- | --- | --- |
| `music_folder` | no | (none) | Default local music folder when not targeting a volume |

#### `[export]` — optional

| Field | Required | Default | Notes |
| --- | --- | --- | --- |
| `layout` | no | `artist-album` | `artist-album` or `flat` |
| `playlist_format` | no | `m3u8` | `m3u8` or `m3u` |

#### `[mixxx]` — optional

| Field | Required | Default | Notes |
| --- | --- | --- | --- |
| `relative_paths` | no | `true` | Use relative paths in Mixxx playlists |

#### `[import]` — optional

| Field | Required | Default | Notes |
| --- | --- | --- | --- |
| `include_dotfiles` | no | `false` | Include hidden files/dirs in import scans |
| `capture_artwork` | no | `true` | Capture co-located cover art during import |

#### `[profiles.<name>]` — optional

Named alternate bucket targets. Each profile requires `bucket` and `region`;
`endpoint` is optional. See `config.example.toml` in the repo root.

### Local-only (no AWS)

No config file required. If you write one, **both** fields are required:

```toml
# ~/.trove/config.toml
[bucket]
name = "local"
region = "local"
```

Objects are copied into `TROVE_BUCKET_DIR` (default `~/.trove/bucket-sim`).
Import duplicates every staged file onto that disk — for large libraries, point
`TROVE_BUCKET_DIR` at a volume with enough free space:

```bash
export TROVE_BUCKET_DIR=/Volumes/LargeDrive/trove-bucket
```

### Real AWS S3

1. Create a bucket; note **name** and **region**.
2. Verify credentials outside Trove: `aws sts get-caller-identity`, `aws s3 ls`.
3. Copy `config.example.toml` to `~/.trove/config.toml` and set **both**
   required bucket fields:

```toml
[bucket]
name = "your-bucket"      # required
region = "us-east-1"      # required
# prefix = ".trove"       # optional (default shown)
# music_prefix = "music"  # optional (default shown)
```

4. Build and run with `--features s3` (see above).
5. Smoke test: `bin/trove archive pull-index` — a new bucket reports
   `EmptyBucket`; after the first import, `UpToDate { generation: N }`.

### S3-compatible endpoints (MinIO, R2, etc.)

`name` and `region` are still **required**; add `endpoint`:

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
| Import fills host disk / `database or disk is full` | Incomplete `config.toml` (missing `region`) silently fell back to local simulator; staging copies files to `TROVE_BUCKET_DIR` | Fix config (`name` + `region` both set), or set `TROVE_BUCKET_DIR` to a large external volume for local mode |
| `could not read .trove/schema-version.json` | Network/credentials | Fix AWS access; or use `--offline` for read-only |
| `mount point … not found` | Drive not mounted | Plug in USB; check `/Volumes/…` path |
| Sparse artist/album in query/USB folders | Tag extraction deferred | See [ADR 999](../adr/999-known-gaps-and-follow-ups.md#tag-extraction-and-index-metadata-adr-002--adr-004) |
