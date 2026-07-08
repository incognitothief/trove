#!/usr/bin/env bash
#
# Boot the Trove dev stack: the local API daemon (trove-serverd) and the web UI
# (Vite). Pre-builds the daemon so the UI never races an un-bound port, waits
# for both to be ready, then prints a clean address banner and streams logs.
#
# Ctrl-C stops everything. Environment overrides:
#   TROVE_HOME        local cache/config root      (default ~/.trove)
#   TROVE_BUCKET_DIR  simulated bucket directory   (default $TROVE_HOME/bucket-sim)
#   SERVERD_ADDR      daemon bind address          (default 127.0.0.1:7377)
#   UI_PORT           Vite dev server port         (default 5273)
#   CARGO_FEATURES    Cargo features for serverd   (e.g. s3 for real buckets)

set -euo pipefail

# Resolve repo root (this script lives in <root>/scripts).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TROVE_HOME="${TROVE_HOME:-$HOME/.trove}"
TROVE_BUCKET_DIR="${TROVE_BUCKET_DIR:-$TROVE_HOME/bucket-sim}"
SERVERD_ADDR="${SERVERD_ADDR:-127.0.0.1:7377}"
UI_PORT="${UI_PORT:-5273}"
export TROVE_HOME TROVE_BUCKET_DIR

# Colors (disabled if stdout is not a TTY).
if [ -t 1 ]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'; CYAN=$'\033[36m'; GREEN=$'\033[32m'
  YELLOW=$'\033[33m'; RED=$'\033[31m'; RESET=$'\033[0m'
else
  BOLD=""; DIM=""; CYAN=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/trove-dev.XXXXXX")"
SERVERD_LOG="$LOG_DIR/serverd.log"
UI_LOG="$LOG_DIR/ui.log"

SERVERD_PID=""
UI_PID=""
TAIL_PID=""

log()  { printf '%s\n' "${DIM}[dev]${RESET} $*"; }
fail() { printf '%s\n' "${RED}[dev] $*${RESET}" >&2; }

cleanup() {
  trap - INT TERM EXIT
  echo
  log "shutting down…"
  [ -n "$TAIL_PID" ]    && kill "$TAIL_PID"    2>/dev/null || true
  [ -n "$UI_PID" ]      && kill "$UI_PID"      2>/dev/null || true
  [ -n "$SERVERD_PID" ] && kill "$SERVERD_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup INT TERM EXIT

# Poll a URL until it responds (or time out). $1=url $2=label
wait_for() {
  local url="$1" label="$2" tries=0 max=100
  while ! curl -sf -o /dev/null "$url" 2>/dev/null; do
    tries=$((tries + 1))
    if [ "$tries" -ge "$max" ]; then
      fail "$label did not become ready at $url"
      return 1
    fi
    sleep 0.2
  done
}

# Ensure a child process is still alive; otherwise dump its log and bail.
ensure_alive() {
  local pid="$1" name="$2" logf="$3"
  if ! kill -0 "$pid" 2>/dev/null; then
    fail "$name exited during startup:"
    sed 's/^/    /' "$logf" >&2 || true
    exit 1
  fi
}

log "logs: ${LOG_DIR}"

# Preflight: UI deps must be installed.
VITE_BIN="ui/node_modules/.bin/vite"
if [ ! -x "$VITE_BIN" ]; then
  fail "UI dependencies missing — run 'make deps' (or 'npm --prefix ui install') first."
  exit 1
fi

# 1) Build the daemon first so compile errors surface here and startup is instant.
#    Run the built binary directly (not via 'cargo run') so this script owns its
#    PID and Ctrl-C reliably stops it.
log "building trove-serverd…"
FEATURE_ARGS=()
if [ -n "${CARGO_FEATURES:-}" ]; then
  FEATURE_ARGS=(--features "$CARGO_FEATURES")
fi
if ! cargo build -p trove-serverd "${FEATURE_ARGS[@]}"; then
  fail "build failed"
  exit 1
fi
SERVERD_BIN="${CARGO_TARGET_DIR:-target}/debug/trove-serverd"

# 2) Start the daemon.
log "starting daemon on ${SERVERD_ADDR}…"
TROVE_SERVERD_ADDR="$SERVERD_ADDR" "$SERVERD_BIN" >"$SERVERD_LOG" 2>&1 &
SERVERD_PID=$!
wait_for "http://${SERVERD_ADDR}/health" "daemon" || { ensure_alive "$SERVERD_PID" "daemon" "$SERVERD_LOG"; exit 1; }

# 3) Start the UI (exec vite directly inside ui/ so UI_PID is the real process).
log "starting web UI on port ${UI_PORT}…"
( cd ui && exec node_modules/.bin/vite --port "$UI_PORT" --strictPort ) >"$UI_LOG" 2>&1 &
UI_PID=$!
wait_for "http://localhost:${UI_PORT}/" "web UI" || { ensure_alive "$UI_PID" "web UI" "$UI_LOG"; exit 1; }

# 4) Clean address banner.
line="────────────────────────────────────────────────────────"
cat <<EOF

${CYAN}${line}${RESET}
  ${BOLD}◆ Trove is up${RESET}

  ${BOLD}Web UI${RESET}   ${GREEN}http://localhost:${UI_PORT}${RESET}
  ${BOLD}API${RESET}      ${GREEN}http://${SERVERD_ADDR}${RESET}
  ${BOLD}Health${RESET}   ${GREEN}http://${SERVERD_ADDR}/health${RESET}

  ${DIM}TROVE_HOME${RESET}        ${TROVE_HOME}
  ${DIM}TROVE_BUCKET_DIR${RESET}  ${TROVE_BUCKET_DIR}
  ${DIM}logs${RESET}              ${LOG_DIR}

  ${YELLOW}UI hot-reloads on save. The Rust daemon does not —${RESET}
  ${YELLOW}restart this script after changing Rust code.${RESET}
  ${DIM}Press Ctrl-C to stop both.${RESET}
${CYAN}${line}${RESET}

EOF

# 5) Stream both logs until interrupted.
tail -n +1 -f "$SERVERD_LOG" "$UI_LOG" &
TAIL_PID=$!
wait "$TAIL_PID"
