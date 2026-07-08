# Trove

A flexible, portable DJ library recovery and export tool.

Keep your canonical music archive somewhere durable (an S3 bucket), query and
build subsets locally, and quickly materialize a performance-ready USB/SSD when
a drive is lost, corrupted, or unavailable.

> Query archive → select tracks/crate → plug in new SSD → export files → write
> playlists → open in Mixxx

The architecture is described in [`docs/adr/000-bootstrap.md`](docs/adr/000-bootstrap.md).
The guiding principle: **everything but the bucket is disposable.**

## Workspace layout

```
trove/
├── crates/
│   ├── trove-core/      # the one authoritative implementation (library)
│   ├── trove-cli/       # thin CLI client  → binary `trove`
│   └── trove-serverd/   # thin local HTTP/JSON API → binary `trove-serverd`
├── ui/                  # React web UI (thin client over trove-serverd)
├── docs/adr/            # architecture decision records
└── config.example.toml  # sample local config (copy to ~/.trove/config.toml)
```

All domain logic lives in `trove-core`. The CLI, daemon, and UI are thin clients
that hold no business logic — they only translate input into core calls.

### `trove-core` modules

| Module      | Responsibility                                             |
| ----------- | ---------------------------------------------------------- |
| `config`    | Local `~/.trove/config.toml`                               |
| `model`     | Domain types (tracks, entries, playlists, volumes)         |
| `store`     | Durable object store (bucket) abstraction + fs/stub impls  |
| `db`        | Disposable local SQLite caches/bookkeeping                 |
| `archive`   | Canonical bucket index + reconciliation                    |
| `query`     | Declarative search over the archive                        |
| `playlist`  | Logical crates/playlists                                   |
| `volume`    | Removable performance volumes                              |
| `sync`      | Resumable transfer/export planning                         |
| `import`    | Resumable, crash-safe bulk import (`scan→…→commit`)        |
| `facade`    | The `Trove` entry point clients drive                      |

## Prerequisites

- Rust (stable) — <https://rustup.rs>
- Node 18+ (for the UI)

## Build & test

```bash
cargo build --workspace     # build core + cli + daemon
cargo test  --workspace     # run tests
cargo clippy --workspace --all-targets
```

## Bootstrap storage backend

The production object store will be S3 (`aws-sdk-s3`, multipart + staging).
Until that lands, the CLI and daemon use a **filesystem-backed store** that
simulates the bucket as a local directory, so everything runs end-to-end with no
AWS credentials:

- `TROVE_HOME` — local cache/config root (default `~/.trove`)
- `TROVE_BUCKET_DIR` — simulated bucket directory (default `~/.trove/bucket-sim`)

## Try the CLI

```bash
# Import a folder (scan → hash → dedupe → upload → verify → commit → push index)
cargo run -p trove-cli -- import ~/Music --plan     # dry-run: show the plan
cargo run -p trove-cli -- import ~/Music            # run it

# Search (reconciles the local cache with the bucket first)
cargo run -p trove-cli -- query --artist "Theo Parrish"
cargo run -p trove-cli -- query --bpm 118:124 --genre house

# Playlists
cargo run -p trove-cli -- playlist create tonight
cargo run -p trove-cli -- playlist add tonight <track-id>

# Volumes
cargo run -p trove-cli -- volume init /Volumes/DJ_USB --label GigDrive
```

Run `cargo run -p trove-cli -- --help` for the full surface.

## Run the daemon + UI

```bash
# Terminal 1: local API (binds 127.0.0.1:7377 by default)
cargo run -p trove-serverd

# Terminal 2: web UI (proxies /api to the daemon)
cd ui && npm install && npm run dev
# open http://localhost:5273
```

## Status

This is the bootstrap scaffold from ADR 000. Fully wired end-to-end:
config, local SQLite indexes, bucket reconciliation, declarative query,
playlists, and the bulk-import pipeline (against the filesystem store).

Next steps (clearly marked `NotImplemented`/TODO in code):

- S3-backed `ObjectStore` with multipart + resumable uploads
- Real audio metadata extraction (`lofty`/`symphonia`)
- Transfer execution (download loop, retry/backoff, verification)
- Playlist export (`.m3u8`) and per-volume diff/status
