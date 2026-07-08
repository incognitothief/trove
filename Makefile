# Trove developer tasks.
# Run `make` or `make help` to see available targets.

CARGO ?= cargo
NPM   ?= npm
UI    := ui
# Optional Cargo features for S3-backed bucket (e.g. `make dev CARGO_FEATURES=s3`).
CARGO_FEATURES ?=
CARGO_FEATURE_FLAGS := $(if $(CARGO_FEATURES),--features $(CARGO_FEATURES),)

# Local, disposable state (override to isolate a demo, e.g. `make dev TROVE_HOME=/tmp/t/home`).
export TROVE_HOME       ?= $(HOME)/.trove
export TROVE_BUCKET_DIR ?= $(HOME)/.trove/bucket-sim
SERVERD_ADDR            ?= 127.0.0.1:7377

.DEFAULT_GOAL := help
.PHONY: help deps install install-rust install-ui build build-ui \
        server ui dev cli test fmt lint typecheck check clean

help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

## --- Dependencies -----------------------------------------------------------

deps: install-rust install-ui ## Install all dependencies (Rust + UI)

install: deps ## Alias for `deps`

install-rust: ## Fetch Rust/Cargo dependencies for the workspace
	$(CARGO) fetch

install-ui: ## Install UI (TypeScript/React) dependencies
	$(NPM) --prefix $(UI) install

## --- Build ------------------------------------------------------------------

build: ## Build the Rust workspace (core + cli + daemon)
	$(CARGO) build --workspace

build-ui: ## Type-check and build the production UI bundle
	$(NPM) --prefix $(UI) run build

## --- Run services -----------------------------------------------------------

server: ## Run the local HTTP/JSON daemon (trove-serverd)
	TROVE_SERVERD_ADDR=$(SERVERD_ADDR) $(CARGO) run -p trove-serverd $(CARGO_FEATURE_FLAGS)

ui: ## Run the web UI dev server (Vite), proxying /api to the daemon
	$(NPM) --prefix $(UI) run dev

dev: ## Boot the full stack (pre-builds, waits for readiness, prints addresses)
	@SERVERD_ADDR=$(SERVERD_ADDR) CARGO_FEATURES='$(CARGO_FEATURES)' bash scripts/dev.sh

# Forward everything after `cli` to the CLI. Because make itself parses leading
# dashes as its own options, put a `--` before any CLI flags, e.g.
#   make cli import /path/to/music -- --plan
#   make cli query --limit 20
# The classic form still works too: make cli ARGS="import /path --plan"
CLI_ARGS := $(ARGS) $(filter-out cli,$(MAKECMDGOALS))

cli: ## Run the CLI, e.g. `make cli import /path -- --plan` (or ARGS="query --limit 20")
	@CARGO="$(CARGO)" bin/trove $(CLI_ARGS)

# When `cli` is invoked, turn the trailing words into no-op targets so make
# forwards them as arguments instead of failing with "no rule to make target".
ifneq (,$(filter cli,$(MAKECMDGOALS)))
$(eval $(filter-out cli,$(MAKECMDGOALS)):;@:)
endif

## --- Quality ----------------------------------------------------------------

test: ## Run the Rust test suite
	$(CARGO) test --workspace

fmt: ## Format Rust sources
	$(CARGO) fmt --all

lint: ## Run clippy across the workspace
	$(CARGO) clippy --workspace --all-targets

typecheck: ## Type-check the UI without emitting
	$(NPM) --prefix $(UI) run typecheck

check: build test lint ## Build, test, and lint (pre-commit sanity)

## --- Housekeeping -----------------------------------------------------------

clean: ## Remove Rust and UI build artifacts
	$(CARGO) clean
	rm -rf $(UI)/dist $(UI)/node_modules $(UI)/.vite
