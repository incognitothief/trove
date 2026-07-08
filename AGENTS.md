# AGENTS.md

Orientation for AI agents and new contributors working in this repo. Read this
first, then the ADRs in `docs/adr/`.

## What Trove is

A portable DJ library recovery/export tool. The bucket (S3) is the only durable
source of truth; the local host cache (`~/.trove`), performance drives, and
local indexes are all disposable and rebuild themselves from the bucket on the
next read ("reads reconcile with the bucket"). See
[ADR 000](docs/adr/000-bootstrap.md) for the initial design notes, and following ADR's for progress.

## The one rule

**All domain logic lives in `trove-core`.** `trove-cli`, `trove-serverd`, and the
React `ui/` are thin clients: they translate input into a single `trove_core`
call and render the result. If you're about to write query/reconcile/sync/import
logic in a client, stop — it belongs in the core.

## Project structure

```
crates/
  trove-core/            # authoritative library — ALL logic
    src/
      facade.rs          # `Trove` — the entry point clients drive
      config.rs          # ~/.trove/config.toml
      model.rs           # domain types (TrackId, ArchiveEntry, Playlist, …)
      error.rs           # Error / Result; clients map these to output
      query.rs           # QuerySpec (declarative; compiled to SQL in db)
      store/             # ObjectStore trait (the bucket) + fs.rs, stub.rs
      metadata.rs        # MetadataExtractor trait + StubExtractor
      db/
        schema.rs        # SQL DDL for all local SQLite DBs
        archive.rs       # archive.sqlite: cache + query + generation stamp
      archive/
        index.rs         # JSONL / schema-version interchange + BucketPaths
        reconcile.rs     # local-cache ↔ bucket reconciliation
      import/            # bulk import: state.rs (Phase/FileState) + pipeline
      playlist.rs        # playlists.sqlite CRUD
      volume.rs          # removable volume layout + identity
      sync.rs            # transfer/export planning
  trove-cli/             # `trove` binary (clap). src/runtime.rs = client wiring
  trove-serverd/         # axum HTTP/JSON API. src/runtime.rs = client wiring
ui/                      # Vite + React + TS; src/api.ts wraps the daemon
docs/adr/                # decision records (000 architecture, 001 retrospective)
Makefile                 # dev tasks (see `make help`)
```

## Build / run / test

```bash
make deps     # install Rust + UI deps
make build    # cargo build --workspace
make test     # cargo test --workspace
make lint     # cargo clippy --workspace --all-targets
make dev      # daemon + UI together
make cli ARGS="query --limit 20"
```

Always run `make lint` and `make test` before finishing a change; both are clean
today and should stay that way.

## Conventions

- **Errors:** fallible core functions return `trove_core::error::Result<T>`.
  Use `Error::NotImplemented("…")` for deliberate stubs (clients surface it).
- **Backends are traits, not concretions.** Depend on `ObjectStore` /
  `MetadataExtractor`, never on S3 or a codec directly. New backends are new impls.
- **Local DBs are disposable.** Opening a DB applies its schema idempotently;
  never treat `~/.trove` as authoritative. Only the bucket is.
- **Reads reconcile first.** Facade read methods (`query`, `plan_playlist_sync`)
  call `reconcile` before serving. Preserve that ordering.
- **Client wiring stays in clients.** Store/config selection lives in each
  client's `runtime.rs`, not in `trove-core`.
- **serde on wire types:** request/response structs need `#[serde(default)]` on
  optional fields (see `QuerySpec`) so partial JSON from the UI deserializes.

## Testing pattern

Core tests use in-memory state and a seeded stub bucket:
`Trove::in_memory_with_store(config, store)` with a `StubStore` pre-loaded with
`archive-index.jsonl` + `schema-version.json`. See the `tests` module in
`crates/trove-core/src/lib.rs` for the template.

## Current stubs / follow-ups (priority order)

Tracked in ADR 001; grep for `NotImplemented` to find seams.

1. Persist import state to `import_jobs`/`import_files`; wire `import resume`
   (in-memory job state today means a crash restarts, not resumes).
2. S3-backed `ObjectStore` (multipart, staging→commit, verify).
3. Compare-and-swap on `schema-version.json` for safe concurrent index writes.
4. Real metadata extraction; transfer execution (download loop, retry/backoff).
5. Playlist `.m3u8` export; per-volume DB (diff/status).

## Gotchas

- The daemon opens the SQLite cache at startup. If you import via the CLI while
  it's running, trigger a reconcile (UI Search) or restart it to see new data.
- `sync`, `archive verify`, and `playlist export` intentionally report
  `NotImplemented` — that's expected, not a bug.
- Keep new modules behind the facade; clients should not gain new imports from
  deep in `trove-core`.

## Notes from a developer friend

- Always record design decisions in ADR format at `docs/adr`
