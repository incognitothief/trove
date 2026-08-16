# AGENTS.md

Durable guidance for working in Trove. This file holds principles, not
inventory. When it disagrees with the code or the ADRs, they win — and you
should update this file only if an actual principle changed. If you're tempted
to add a file path, a module list, or a status checklist here, it belongs in the
code, the ADRs, or `make help` instead.

## Orient yourself first

- **Read the ADRs in `docs/adr/`.** They are the source of truth for the
  architecture and the running record of decisions. Read in order; the latest
  ones describe current state and open follow-ups.
- **Read `docs/recall/`** for episodic session memory — what actually happened
  last, what failed, and the next action. It is not architecture; ADRs still win
  on decisions.
- **Discover the structure, don't memorize it.** Skim the tree and run
  `make help`. Any map written here would go stale — explore instead.

## The invariant that defines this project

**Everything but the bucket is disposable.** The remote object store (the
bucket) is the only durable source of truth. Local caches, indexes, host state,
and performance drives are all rebuildable and must be treated as throwaway.
Reads reconcile against the bucket before serving. Never let host or drive state
become authoritative.

## The one architectural rule

**All domain logic lives in the core library crate.** The CLI, the HTTP daemon,
and the web UI are *thin clients*: each translates input into a single core call
and renders the result — nothing more. If you find yourself writing
reconcile/query/sync/import logic in a client, move it into the core. This is
what lets a fix land once and surface everywhere.

## Design conventions (stable)

- **Abstract external systems.** Depend on a trait for anything external
  (object store, metadata extraction, …), never on a concrete vendor/SDK. A new
  backend is a new implementation of an existing trait, not a change to callers.
- **One error surface.** Fallible core operations return the crate's shared
  `Result`/`Error`. Mark deliberate gaps with an explicit "not implemented"
  error so clients surface them honestly rather than faking success.
- **Wiring stays in clients.** Config discovery and backend selection live in
  the clients; keep client concerns out of the core.
- **Local stores are disposable.** Schemas apply idempotently; a store is always
  safe to delete and rebuild.
- **Tolerant wire types.** Serialized request/response types default missing
  fields so evolving clients don't break on partial input.

## Working agreements

- Keep changes behind the core's public entry point; clients shouldn't reach
  into internals.
- Before finishing: build, test, and lint must be clean. Use `make help` for the
  exact targets — all are expected to stay green.
- Record consequential decisions as a **new** ADR; don't rewrite old ones. Leave
  a short, honest trail of what's real vs. deferred.

## Finding what's unfinished

Don't trust a checklist in this file — it rots. Instead:

- `grep` for the "not implemented" marker to find live stubs and seams.
- The most recent ADR tracks current status and prioritized follow-ups.

## Dev workflow

- The Makefile is the task index (`make help`): install, build, run, test, lint.
- The UI hot-reloads on save; the Rust services do not — restart them after
  changing Rust code.
