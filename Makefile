# Trove developer tasks.
# Run `make` or `make help` to see available targets.

CARGO ?= cargo
NPM   ?= npm
UI    := ui

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
	TROVE_SERVERD_ADDR=$(SERVERD_ADDR) $(CARGO) run -p trove-serverd

ui: ## Run the web UI dev server (Vite), proxying /api to the daemon
	$(NPM) --prefix $(UI) run dev

dev: ## Run the daemon and the UI together (Ctrl-C stops both)
	@echo "Starting trove-serverd ($(SERVERD_ADDR)) + UI (http://localhost:5273)"
	@$(MAKE) -j2 server ui

cli: ## Run the CLI; pass args with ARGS, e.g. `make cli ARGS="query --limit 20"`
	$(CARGO) run -p trove-cli -- $(ARGS)

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
