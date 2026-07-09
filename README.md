# Trove

A flexible, portable DJ library recovery and export tool.

Keep your canonical music archive somewhere durable (an S3 bucket), query and
build subsets locally, and quickly materialize a performance-ready USB/SSD when
a drive is lost, corrupted, or unavailable.

> Query archive → select tracks/crate → plug in new SSD → export files → write
> playlists → open in Mixxx

The guiding principle: **everything but the bucket is disposable.** Any laptop,
its `~/.trove` cache, and any performance drive can be thrown away and rebuilt
from the bucket on the next read.

Architecture and rationale live in the ADRs:

- [ADR 000 — Bootstrap Architecture](docs/adr/000-bootstrap.md)
- [ADR 001 — Bootstrap Implementation Retrospective](docs/adr/001-bootstrap-implementation.md)

For contributor/agent orientation, see [`AGENTS.md`](AGENTS.md).

## Workspace layout

```
trove/
├── crates/
│   ├── trove-core/      # the one authoritative implementation (library)
│   ├── trove-cli/       # thin CLI client  → binary `trove`
│   └── trove-serverd/   # thin local HTTP/JSON API → binary `trove-serverd`
├── ui/                  # React web UI (thin client over trove-serverd)
├── bin/trove            # convenience wrapper → runs the CLI from anywhere
├── docs/adr/            # architecture decision records
├── config.example.toml  # sample local config (copy to ~/.trove/config.toml)
├── scripts/             # dev/boot scripts (e.g. scripts/dev.sh)
└── Makefile             # developer task shortcuts
```

All domain logic lives in `trove-core`. The CLI, daemon, and UI are thin clients
that hold no business logic — they only translate input into core calls, so a
bug is fixed once and every surface benefits.

## Prerequisites

- **Rust** (stable) — <https://rustup.rs>
- **Node 18+** (for the UI)

## Quick start

```bash
make deps      # install Rust + UI dependencies
make build     # build the Rust workspace
make dev       # run the daemon + web UI together (Ctrl-C stops both)
# then open http://localhost:5273
```

Run `make` (or `make help`) to list every task. Common ones:

| Task              | What it does                                        |
| ----------------- | --------------------------------------------------- |
| `make deps`       | Install Rust (`cargo fetch`) + UI (`npm install`)   |
| `make build`      | Build core + CLI + daemon                           |
| `make server`     | Run `trove-serverd` (default `127.0.0.1:7377`)      |
| `make ui`         | Run the Vite dev server (proxies `/api` → daemon)   |
| `make dev`        | Boot the stack via `scripts/dev.sh` (waits + banner)|
| `make cli …`      | Run the CLI (see [Try the CLI](#try-the-cli) for arg rules)|
| `make test`       | Run the Rust test suite                             |
| `make lint`       | `cargo clippy --workspace --all-targets`            |
| `make check`      | build + test + lint (pre-commit sanity)             |

## Storage backend

Trove has two bucket backends:

- **Filesystem store** (`FsStore`) — the default bootstrap/dev path. Trove
  simulates the bucket as a local directory, so everything runs end-to-end with
  no AWS credentials.
- **S3 store** (`S3Store`) — the production backend, implemented behind the
  same `ObjectStore` seam. It is compiled only when you build with
  `--features s3`.

The core mental model is the same in both cases: the **bucket** is the durable
source of truth; local SQLite files are disposable caches.

Environment variables:

- `TROVE_HOME` — local cache/config root (default `~/.trove`)
- `TROVE_BUCKET_DIR` — simulated bucket directory for the filesystem store
  (default `~/.trove/bucket-sim`)

These are also honored by the Makefile, so you can isolate a demo:

```bash
make dev TROVE_HOME=/tmp/trove-demo/home TROVE_BUCKET_DIR=/tmp/trove-demo/bucket
```

## Config runbook

`config.toml` tells Trove **where the durable bucket lives**. If no config file
exists, Trove synthesizes a local-only default:

```toml
[bucket]
name = "local"
region = "local"
```

That means:

- no config file → filesystem store
- real bucket config + `--features s3` build → S3 store

### 1. Local-only / no AWS

You do not need a config file for local testing. Trove falls back to the local
filesystem store automatically.

If you want to make that explicit, create `~/.trove/config.toml`:

```toml
[bucket]
name = "local"
region = "local"
```

This keeps all durable objects under `TROVE_BUCKET_DIR` (default
`~/.trove/bucket-sim`).

### 2. Real AWS S3 bucket

1. Create a bucket and note its **name** and **region**.
2. Ensure AWS credentials work outside Trove first:
   - `aws sts get-caller-identity`
   - `aws s3 ls`
3. Copy `config.example.toml` to `~/.trove/config.toml`.
4. Set the bucket fields to your real bucket:

```toml
[bucket]
name = "your-bucket"
region = "us-east-1"
prefix = ".trove"
music_prefix = "music"
```

5. Build or run Trove **with** the S3 feature:

```bash
cargo run -p trove-cli --features s3 -- archive pull-index
```

If the bucket is brand new, `archive pull-index` should report `EmptyBucket`.
After your first import it should report a generation such as
`UpToDate { generation: 1 }`.

Important:

- A real bucket config **without** `--features s3` will fail with a clear error.
- Pass the feature via `CARGO_FEATURES=s3` on `make cli`, `make build`, `make dev`, or
  `bin/trove` (e.g. `make cli CARGO_FEATURES=s3 import list`).

### 3. S3-compatible endpoints (MinIO, R2, etc.)

Set `endpoint` in the bucket config:

```toml
[bucket]
name = "trove-test"
region = "us-east-1"
endpoint = "https://s3.example.com"
```

Trove will force path-style addressing for custom endpoints.

## Try the CLI

There are two ways to run the CLI. The `bin/trove` wrapper is the easy one — it
forwards every argument straight to the CLI and works from any directory, so you
can use flags naturally:

```bash
# Import a folder (scan → hash → dedupe → upload → verify → commit → push index)
bin/trove import ~/Music --plan          # dry-run: show the plan
bin/trove import ~/Music                  # run it

# Search (reconciles the local cache with the bucket first)
bin/trove query --artist "Theo Parrish"
bin/trove query --bpm 118:124 --genre house

# Playlists / volumes
bin/trove playlist create tonight
bin/trove volume init /Volumes/DJ_USB --label GigDrive
```

Put `bin/` on your `PATH` (or symlink `bin/trove` into a dir already on it) to
drop the prefix and just run `trove import ~/Music --plan` from anywhere:

```bash
export PATH="$PWD/bin:$PATH"   # add to ~/.zshrc to make it permanent
```

The same commands are also available through `make cli`, with one catch: because
`make` parses leading-dash arguments as its *own* options, you must place a `--`
before any CLI flags. Positional arguments need no `--`.

```bash
make cli import ~/Music -- --plan        # note the `--` before --plan
make cli query -- --limit 20
make cli ARGS="query --artist 'Theo Parrish'"   # or pass everything via ARGS
```

`bin/trove --help` (or `make cli -- --help`) prints the full command surface.

## Status

This is still the bootstrap scaffold, but it now has a real S3-backed storage
path in addition to the filesystem simulator.

Working today:

- local config + disposable SQLite caches
- bucket reconciliation via `schema-version.json`
- declarative query
- playlists
- bulk import pipeline (`scan → hash → dedupe → upload → verify → commit → push index`)
- filesystem-backed bucket simulation (`FsStore`)
- feature-gated S3-backed bucket (`S3Store`, `--features s3`)

Still deliberately deferred:

- streaming / checkpointed resumable multipart uploads
- compare-and-swap protection on `schema-version.json` for concurrent importers
- real metadata extraction
- transfer execution / retry / verify loops for sync-to-volume
- `.m3u8` export
- persistent import resume

See [ADR 001](docs/adr/001-bootstrap-implementation.md) for the bootstrap
retrospective and [ADR 003](docs/adr/003-s3-object-store.md) for the S3 design
and trade-offs.
