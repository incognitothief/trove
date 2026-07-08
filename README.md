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

## Storage backend (bootstrap)

Production storage will be S3 (`aws-sdk-s3`, multipart + staging). Until that
lands, the CLI and daemon use a **filesystem-backed store** that simulates the
bucket as a local directory, so everything runs end-to-end with no AWS
credentials:

- `TROVE_HOME` — local cache/config root (default `~/.trove`)
- `TROVE_BUCKET_DIR` — simulated bucket directory (default `~/.trove/bucket-sim`)

These are also honored by the Makefile, so you can isolate a demo:

```bash
make dev TROVE_HOME=/tmp/trove-demo/home TROVE_BUCKET_DIR=/tmp/trove-demo/bucket
```

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

This is the bootstrap scaffold. Fully wired end-to-end (against the filesystem
store): config, local SQLite indexes, bucket reconciliation, declarative query,
playlists, and the bulk-import pipeline. Deliberately deferred work (S3 store,
real metadata extraction, transfer execution, `.m3u8` export, persistent import
resume) is tracked in [ADR 001](docs/adr/001-bootstrap-implementation.md) and
marked `NotImplemented` in code.
