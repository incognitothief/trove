#!/usr/bin/env bash
#
# Run one `trove import` cycle per immediate subdirectory of a parent path.
#
# Trove has no built-in globbing or batch import yet. This script is a thin
# wrapper: for each child directory it runs the one-shot import path
# (scan → upload → verify → commit).
#
# Usage:
#   CARGO_FEATURES=s3 scripts/import-batch.sh <parent-dir>    # real S3 bucket
#   scripts/import-batch.sh <parent-dir>                      # local simulator
#   scripts/import-batch.sh --after <folder> <parent-dir>     # resume batch
#   scripts/import-batch.sh <parent-dir> -- [--include-dotfiles] [--no-artwork] …
#
# Arguments after `--` are forwarded to every `bin/trove import` call.
# bin/trove needs CARGO_FEATURES=s3 at invocation time when config.toml points
# at a real bucket (see docs/runbook/00-prerequisites.md).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TROVE="$ROOT/bin/trove"

usage() {
  cat <<'EOF'
Usage: scripts/import-batch.sh [--from NAME] [--after NAME] <parent-dir> [-- trove-import-args…]

Run bin/trove import once per immediate subdirectory of <parent-dir>.
Folders are processed in C locale sort order (byte-wise, usually alphabetical).

Resume options (match folder basename under <parent-dir>):
  --from NAME   start at NAME (inclusive); re-import if it failed mid-job
  --after NAME  skip through NAME (exclusive); use when NAME finished cleanly

Examples:
  CARGO_FEATURES=s3 scripts/import-batch.sh ~/Music/DJ-Crates
  CARGO_FEATURES=s3 scripts/import-batch.sh --after 1600J /Volumes/T72/music/library
  scripts/import-batch.sh ~/Music/DJ-Crates -- --no-artwork

Exit code is the number of failed imports (capped at 125).
EOF
}

parent=""
start_from_name=""
start_after_name=""
trove_args=()

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --from)
      [ $# -ge 2 ] || { echo "error: --from requires a folder name" >&2; exit 1; }
      start_from_name="$2"
      shift 2
      ;;
    --after)
      [ $# -ge 2 ] || { echo "error: --after requires a folder name" >&2; exit 1; }
      start_after_name="$2"
      shift 2
      ;;
    --)
      shift
      trove_args=("$@")
      break
      ;;
    -*)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      if [ -n "$parent" ]; then
        echo "error: unexpected argument: $1" >&2
        usage >&2
        exit 1
      fi
      parent="$(cd -- "$1" && pwd)"
      shift
      ;;
  esac
done

if [ -z "$parent" ]; then
  usage >&2
  exit 1
fi

if [ -n "$start_from_name" ] && [ -n "$start_after_name" ]; then
  echo "error: use only one of --from or --after" >&2
  exit 1
fi

if [ ! -x "$TROVE" ]; then
  echo "error: trove wrapper not found at $TROVE (run 'make build' first)" >&2
  exit 1
fi

# Fail fast before looping hundreds of folders.
preflight_err="$("$TROVE" archive pull-index --offline 2>&1)" || {
  if [[ "$preflight_err" == *"no S3 support"* ]]; then
    cat >&2 <<'EOF'
error: config points at a real S3 bucket but trove was invoked without the s3 feature.

bin/trove runs `cargo run` and needs CARGO_FEATURES at invocation time (a prior
`make build CARGO_FEATURES=s3` is not enough). Re-run with:

  CARGO_FEATURES=s3 scripts/import-batch.sh <parent-dir>

EOF
    exit 1
  fi
}

dirs=()
while IFS= read -r dir; do
  dirs+=("$dir")
done < <(find "$parent" -mindepth 1 -maxdepth 1 -type d | LC_ALL=C sort)

if [ "${#dirs[@]}" -eq 0 ]; then
  echo "error: no subdirectories under $parent" >&2
  exit 1
fi

import_dirs=()
resume=false
past_after=false

if [ -z "$start_from_name" ] && [ -z "$start_after_name" ]; then
  resume=true
fi

for dir in "${dirs[@]}"; do
  base="$(basename "$dir")"

  if [ "$resume" = false ]; then
    if [ -n "$start_from_name" ]; then
      if [ "$base" = "$start_from_name" ]; then
        resume=true
      else
        continue
      fi
    elif [ -n "$start_after_name" ]; then
      if [ "$past_after" = false ]; then
        [ "$base" = "$start_after_name" ] && past_after=true
        continue
      fi
    fi
  fi

  import_dirs+=("$dir")
done

if [ -n "$start_from_name" ] && [ "$resume" = false ]; then
  echo "error: --from folder not found under $parent: $start_from_name" >&2
  exit 1
fi

if [ -n "$start_after_name" ] && [ "$past_after" = false ]; then
  echo "error: --after folder not found under $parent: $start_after_name" >&2
  exit 1
fi

total="${#import_dirs[@]}"
if [ "$total" -eq 0 ]; then
  echo "error: no folders left to import" >&2
  exit 1
fi

failed=0
failures=()

if [ -n "$start_from_name" ]; then
  echo "import-batch: resuming from $start_from_name ($total folder(s) remaining)"
elif [ -n "$start_after_name" ]; then
  echo "import-batch: resuming after $start_after_name ($total folder(s) remaining)"
else
  echo "import-batch: $total folder(s) under $parent"
fi

for i in "${!import_dirs[@]}"; do
  dir="${import_dirs[$i]}"
  n=$((i + 1))
  echo
  echo "[$n/$total] importing $dir"

  if ((${#trove_args[@]} > 0)); then
    import_cmd=("$TROVE" import "${trove_args[@]}" "$dir")
  else
    import_cmd=("$TROVE" import "$dir")
  fi

  if "${import_cmd[@]}"; then
    echo "[$n/$total] done: $dir"
  else
    failed=$((failed + 1))
    failures+=("$dir")
    echo "[$n/$total] FAILED: $dir" >&2
  fi
done

echo
if [ "$failed" -eq 0 ]; then
  echo "import-batch: all $total folder(s) imported"
  exit 0
fi

echo "import-batch: $failed of $total folder(s) failed:" >&2
for f in "${failures[@]}"; do
  echo "  - $f" >&2
done

exit "$((failed > 125 ? 125 : failed))"
