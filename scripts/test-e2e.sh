#!/usr/bin/env bash
#
# scripts/test-e2e.sh — the per-binary e2e tier runner.
#
# WHY THIS EXISTS
# One `cargo test e2e:: -- --ignored` links every integration test into one
# libtest pool: unrelated crates share one thread count, one failure stops the
# rest, and per-crate output is interleaved. This runner builds the e2e test
# binaries ONCE, then runs each discovered `*_e2e` integration binary as its
# own process under a bounded process pool, with a per-binary log and a
# per-binary pass/fail summary.
#
# DISCOVERY (never hard-coded)
# `cargo test --no-run --message-format=json` emits one `compiler-artifact`
# record per target; every record whose target kind is `test` and whose name
# ends in `_e2e` is run. A new `tests/foo_e2e.rs` joins the run automatically,
# and zero discovered binaries is a loud failure. Every scenario in the binary
# runs by default (`e2e:: --ignored`); nothing is filtered unless asked for.
#
# USAGE
#   scripts/test-e2e.sh [-h|--help] [--exclude <binary>[:<test-substring>]]...
#
# ENVIRONMENT
#   E2E_JOBS      concurrently running binaries (default: min(8, CPU cores))
#   E2E_THREADS   libtest threads per binary (default: 4)
#                 heavy binaries run at min(2, E2E_THREADS)
#
# --exclude is repeatable: a bare <binary> drops that whole binary, while
# <binary>:<test-substring> runs it with each matching test skipped (libtest
# `--skip <test-substring>`). Nothing is excluded by default, so the full
# selection passes no libtest flags and a newly added scenario runs in both
# modes. Logs land in $CARGO_TARGET_DIR/e2e-logs/ (default `target/e2e-logs/`);
# the process exits non-zero if any binary fails.
#
# macOS default bash 3.2 has no `wait -n`, so the pool waits on specific pids.
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
LOG_DIR="$TARGET_DIR/e2e-logs"

# Heavy binaries open many databases and are capped to at most 2 libtest
# threads regardless of E2E_THREADS: lbug reserves ~8 TiB of virtual address
# space per Database, and splice_e2e has been observed to exhaust the process
# address space at libtest's default parallelism. Extend this space-separated
# list as new heavy crates appear.
E2E_HEAVY_BINARIES="splice_e2e"

usage() {
  cat <<'EOF'
Usage: scripts/test-e2e.sh [-h|--help] [--exclude <binary>[:<test-substring>]]...

Builds every e2e test binary once, discovers each `*_e2e` integration test
binary from the cargo JSON output, and runs them as a bounded process pool.
Each binary runs all its scenarios (libtest `e2e:: --ignored`).

Environment:
  E2E_JOBS      concurrent binaries (default: min(8, CPU cores))
  E2E_THREADS   libtest threads per binary (default: 4);
                heavy binaries are capped to min(2, E2E_THREADS)

Options:
  --exclude <binary>              drop that whole binary
  --exclude <binary>:<substring>  run that binary with the matching tests
                                  skipped (repeatable)
  -h, --help                      show this help

Logs: $CARGO_TARGET_DIR/e2e-logs/<binary>.log (default target/e2e-logs/),
with the exit status beside it. The runner exits non-zero if any binary fails.
EOF
}

is_positive_int() {
  case "$1" in
    '' | *[!0-9]* | 0) return 1 ;;
  esac
  return 0
}

EXCLUDES=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --exclude)
      if [ "$#" -lt 2 ]; then
        printf 'test-e2e: --exclude needs a value\n' >&2
        usage >&2
        exit 2
      fi
      EXCLUDES+=("$2")
      shift 2
      ;;
    --exclude=*)
      EXCLUDES+=("${1#--exclude=}")
      shift
      ;;
    *)
      printf 'test-e2e: unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

i=0
while [ "$i" -lt "${#EXCLUDES[@]}" ]; do
  entry=${EXCLUDES[$i]}
  case "$entry" in
    '' | :*)
      printf 'test-e2e: --exclude needs a <binary>[:<test-substring>] value, got: %s\n' "$entry" >&2
      exit 2
      ;;
  esac
  i=$((i + 1))
done

# CPU cores: sysctl (macOS) first, nproc as the Linux/fallback probe.
cores=""
if command -v sysctl >/dev/null 2>&1; then
  cores=$(sysctl -n hw.ncpu 2>/dev/null || true)
fi
if [ -z "$cores" ] && command -v nproc >/dev/null 2>&1; then
  cores=$(nproc 2>/dev/null || true)
fi
case "$cores" in
  '' | *[!0-9]* | 0) cores=4 ;;
esac

default_jobs=$cores
if [ "$default_jobs" -gt 8 ]; then default_jobs=8; fi

JOBS="${E2E_JOBS:-$default_jobs}"
THREADS="${E2E_THREADS:-4}"
if ! is_positive_int "$JOBS"; then
  printf 'test-e2e: E2E_JOBS must be a positive integer, got: %s\n' "$JOBS" >&2
  exit 2
fi
if ! is_positive_int "$THREADS"; then
  printf 'test-e2e: E2E_THREADS must be a positive integer, got: %s\n' "$THREADS" >&2
  exit 2
fi

mkdir -p "$LOG_DIR"
rm -f "$LOG_DIR"/*.log "$LOG_DIR"/*.status

BUILD_JSON="$LOG_DIR/build.json"
printf '=== e2e runner: building test binaries ===\n'
if ! cargo test --no-run --message-format=json >"$BUILD_JSON"; then
  printf 'test-e2e: cargo test --no-run failed; not running the e2e tier\n' >&2
  exit 1
fi

# Discover the `*_e2e` test binaries from the JSON build records. Each record
# is one line, so plain parameter expansion is enough (no jq dependency).
BINS=()
EXES=()
has_bin() {
  local needle=$1 j=0
  while [ "$j" -lt "${#BINS[@]}" ]; do
    if [ "${BINS[$j]}" = "$needle" ]; then return 0; fi
    j=$((j + 1))
  done
  return 1
}
while IFS= read -r line; do
  case "$line" in
    *'"reason":"compiler-artifact"'*'"target":{"kind":["test"],'*) ;;
    *) continue ;;
  esac
  name=${line#*'"name":"'}
  name=${name%%'"'*}
  case "$name" in
    *_e2e) ;;
    *) continue ;;
  esac
  case "$line" in
    *'"executable":"'*) ;;
    *) continue ;;
  esac
  exe=${line#*'"executable":"'}
  exe=${exe%%'"'*}
  [ -n "$exe" ] || continue
  if ! has_bin "$name"; then
    BINS+=("$name")
    EXES+=("$exe")
  fi
done <"$BUILD_JSON"

if [ "${#BINS[@]}" -eq 0 ]; then
  printf 'test-e2e: no *_e2e integration test binaries were discovered\n' >&2
  printf 'test-e2e: expected compiler-artifact records with target.kind=["test"] and a `_e2e` name\n' >&2
  exit 1
fi

# Warn (not fail) about --exclude entries that name no discovered binary, so a
# typo is visible without making a phase-06 exclusion list brittle.
i=0
while [ "$i" -lt "${#EXCLUDES[@]}" ]; do
  entry=${EXCLUDES[$i]}
  bin=${entry%%:*}
  if ! has_bin "$bin"; then
    printf 'test-e2e: warning: --exclude names no discovered binary: %s\n' "$bin" >&2
  fi
  i=$((i + 1))
done

is_heavy() {
  case " $E2E_HEAVY_BINARIES " in
    *" $1 "*) return 0 ;;
  esac
  return 1
}

run_one() {
  local name=$1 exe=$2 threads=$3 skips=$4
  local log="$LOG_DIR/$name.log" status="$LOG_DIR/$name.status"
  printf '=== %s (%s thread(s)) ===\n' "$name" "$threads" >"$log"
  set -- "$exe" e2e:: --ignored --test-threads="$threads"
  if [ -n "$skips" ]; then
    while IFS= read -r skip; do
      [ -n "$skip" ] && set -- "$@" --skip "$skip"
    done <<EOF
$skips
EOF
  fi
  local rc=0
  "$@" >>"$log" 2>&1 || rc=$?
  printf '\n[test-e2e] exit %s\n' "$rc" >>"$log"
  printf '%s\n' "$rc" >"$status"
}

printf '=== e2e runner: %s binaries, jobs=%s, threads=%s (heavy capped)\n' \
  "${#BINS[@]}" "$JOBS" "$THREADS"

pids=()
active=0
i=0
while [ "$i" -lt "${#BINS[@]}" ]; do
  name=${BINS[$i]}
  exe=${EXES[$i]}
  i=$((i + 1))

  whole=0
  skips=""
  j=0
  while [ "$j" -lt "${#EXCLUDES[@]}" ]; do
    entry=${EXCLUDES[$j]}
    case "$entry" in
      "$name") whole=1 ;;
      "$name":*) skips="${skips}${entry#"$name":}"$'\n' ;;
    esac
    j=$((j + 1))
  done
  if [ "$whole" -eq 1 ]; then
    printf '  skip  %-20s (--exclude)\n' "$name"
    continue
  fi

  threads=$THREADS
  if is_heavy "$name" && [ "$threads" -gt 2 ]; then
    threads=2
  fi

  run_one "$name" "$exe" "$threads" "$skips" &
  pids+=("$!")
  active=$((active + 1))
  if [ "$active" -ge "$JOBS" ]; then
    wait "${pids[0]}" || true
    pids=("${pids[@]:1}")
    active=$((active - 1))
  fi
done

while [ "$active" -gt 0 ]; do
  wait "${pids[0]}" || true
  pids=("${pids[@]:1}")
  active=$((active - 1))
done

failed=0
printf '\n=== e2e summary ===\n'
i=0
while [ "$i" -lt "${#BINS[@]}" ]; do
  name=${BINS[$i]}
  i=$((i + 1))
  status="$LOG_DIR/$name.status"
  log="$LOG_DIR/$name.log"
  if [ ! -f "$status" ]; then
    # No status file: the binary was skipped by --exclude.
    printf '  %-8s %-20s (--exclude, no log)\n' skipped "$name"
    continue
  fi
  read -r rc <"$status" || rc=1
  if [ "$rc" -eq 0 ]; then
    printf '  %-8s %-20s log: %s\n' passed "$name" "$log"
  else
    failed=1
    printf '  %-8s %-20s log: %s (exit %s)\n' FAILED "$name" "$log" "$rc"
  fi
done

if [ "$failed" -ne 0 ]; then
  printf '\n=== e2e RED ===\n'
  exit 1
fi
printf '\n=== e2e GREEN ===\n'
