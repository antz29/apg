#!/usr/bin/env bash
#
# scripts/gate.sh — the repo done-gate as ONE command.
#
# WHY THIS EXISTS
# The agent permission grants are deny-by-default and forbid chaining, so the
# gate (fmt -> check -> clippy -> build -> test) would otherwise be N separate
# calls with nothing enforcing that all of them ran. A cargo alias cannot wrap
# a sequence: an alias value is a cargo subcommand, and arbitrary-command
# aliases are an unimplemented proposal (rust-lang/cargo#6575). `make`/`just`
# would add a tool dependency this repo does not need.
#
# OWNERSHIP
# The gate is granted run-only as an exact bash pattern to the scaffolded
# agents — they may RUN it but not edit it. `build-implementer` owns
# `scripts/**` and is the one agent that may change a step, so the sequence
# cannot be rewritten beneath any other agent that is running it.
#
# USAGE
#   scripts/gate.sh          # actionlint, Formula ruby -c, shell -n, fmt --check,
#                            # check, clippy, build, test --lib, suite deps,
#                            # suite tsc --noEmit, bun test, node --test
#   scripts/gate.sh --e2e    # ... then the e2e tier (opt-in): the per-binary
#                            #     runner scripts/test-e2e.sh AND the
#                            #     opencode-suite boundary e2e (scratch /tmp
#                            #     repos only, never a real project)
#
# Every run, green or red, ends with a summary: one line per step of the run's
# mode, in order — passed, failed, skipped (by its own condition), or not run.
#
# Two steps reach file types no cargo/JS suite parses: `actionlint` validates
# every `.github/workflows/*.yml`, and `ruby -c` syntax-checks each
# `Formula/*.rb` (per file). `ruby` is a system interpreter, so that step hard-
# fails. `actionlint` is a dev-machine tool (e.g. /opt/homebrew/bin/actionlint),
# NOT a repo dependency, so an absent `actionlint` SKIPS the workflow step with
# a loud warning rather than blocking every agent's gate; install it to get the
# check. The step runs with `-shellcheck=` because actionlint's shellcheck
# integration depends on another ambient binary, so the same tree would pass or
# fail per machine; the workflow structure, expressions and YAML are what this
# gate validates (embedded-script style stays a review concern). The shell
# step parses (`-n`, never executes) `install.sh` and every `scripts/*.sh` with
# the shell its first-line shebang names — `sh` for `#!/bin/sh`, `bash` for a
# bash shebang — and fails on any other shebang, naming the file. The
# `opencode-suite/node_modules` bootstrap installs from the committed
# `opencode-suite/bun.lock` (`--frozen-lockfile`) only when the directory is
# absent or lacks the type-check toolchain (`node_modules/.bin/tsc` not
# executable, or `node_modules/@types/bun` absent) — the cold path, a fresh or
# pre-toolchain worktree — and skips the install when the toolchain is present,
# so gate-time network exposure is bounded to that first run. The type-check
# step then runs the suite-local `tsc --noEmit -p tsconfig.json` (typescript
# 5.9.3 from `opencode-suite/node_modules`, never bunx/npx or src/tslib's).
#
# `cargo test --lib` here is the repo's DEFAULT suite: the lib target's unit+int
# tiers only, seconds-fast. It links no test target under `tests/`, so the e2e
# crates are never built by the default step; `cargo clippy --all-targets` above
# is unchanged and still type-checks every target, and `--e2e` runs
# `scripts/test-e2e.sh` (or `cargo test-e2e` on its own). The runner builds
# every `*_e2e` binary once and runs each as its own process under a bounded
# pool: E2E_JOBS concurrent binaries (default min(8, CPU cores)) at
# E2E_THREADS libtest threads each (default 4, the documented `cargo test-e2e`
# alias value), heavy binaries (splice_e2e) capped lower; many concurrent
# `db.lbug` opens have been observed to flake with LadybugDB `Mmap ... failed`.
set -euo pipefail

cd "$(dirname "$0")/.."

BIN="$PWD/target/debug/apg"

# The run's full ordered step list, declared up front so the end-of-run summary
# can name every step — including the ones an earlier failure left not run.
STEPS=(
  "actionlint   (.github/workflows/*.yml)"
  "ruby -c   (Formula/*.rb)"
  "shell -n   (install.sh, scripts/*.sh; by shebang)"
  "cargo fmt --check   (cargo fmt fixes)"
  "cargo check --all-targets"
  "cargo clippy --all-targets -- -D warnings"
  "cargo build"
  "cargo test --lib   (default suite: unit+int)"
  "opencode-suite deps   (bootstrap from bun.lock when absent or lacking tsc/@types/bun)"
  "tsc --noEmit   (opencode-suite: suite-local typescript)"
  "bun test   (opencode-suite: bun)"
  "node --test   (src/tslib: node)"
)
if [ "${1:-}" = "--e2e" ]; then
  STEPS+=(
    "bash scripts/test-e2e.sh   (e2e tier: E2E_JOBS binaries x ${E2E_THREADS:-4} threads)"
    "bun test e2e   (opencode-suite boundary; candidate $BIN)"
  )
fi
OUTCOME=()
cur=-1

# Starting a step means the previous one passed (set -e exits on any failure).
step() {
  cur=$((cur + 1))
  printf '\n=== %s ===\n' "${STEPS[$cur]}"
}
skipped() { OUTCOME[cur]="skipped ($1)"; }

# EXIT trap: the step running at exit failed iff the exit status is non-zero;
# every step after it was not run. Re-exits with the original status.
summary() {
  local rc=$1 i outcome
  printf '\n=== gate summary ===\n'
  for i in "${!STEPS[@]}"; do
    if [ "$i" -lt "$cur" ]; then
      outcome=${OUTCOME[i]:-passed}
    elif [ "$i" -eq "$cur" ]; then
      if [ "$rc" -eq 0 ]; then outcome=${OUTCOME[i]:-passed}; else outcome="failed (exit $rc)"; fi
    else
      outcome="not run"
    fi
    printf '  %-34s %s\n' "$outcome" "${STEPS[$i]}"
  done
  exit "$rc"
}
trap 'summary "$?"' EXIT

step
if command -v actionlint >/dev/null 2>&1; then
  actionlint -shellcheck= .github/workflows/*.yml
else
  printf 'actionlint not installed — SKIPPING workflow validation (brew install actionlint)\n'
  skipped "actionlint not installed"
fi

step
for formula in Formula/*.rb; do
  ruby -c "$formula"
done

step
for f in install.sh scripts/*.sh; do
  IFS= read -r shebang <"$f" || shebang=
  case "$shebang" in
    '#!/bin/sh') sh -n "$f" ;;
    '#!/bin/bash' | '#!/usr/bin/env bash') bash -n "$f" ;;
    *)
      printf '%s: unsupported shebang %s — no syntax checker\n' "$f" "${shebang:-(none)}" >&2
      exit 1
      ;;
  esac
done

step
cargo fmt --check

step
cargo check --all-targets

step
cargo clippy --all-targets -- -D warnings

step
cargo build

step
cargo test --lib

step
if [ -x opencode-suite/node_modules/.bin/tsc ] && [ -d opencode-suite/node_modules/@types/bun ]; then
  printf 'opencode-suite/node_modules has the type-check toolchain — skipping install\n'
  skipped "toolchain present"
else
  (cd opencode-suite && bun install --frozen-lockfile)
fi

step
(cd opencode-suite && node_modules/.bin/tsc --noEmit -p tsconfig.json)

step
(cd opencode-suite && bun test)

step
(cd src/tslib && node --test)

if [ "${1:-}" = "--e2e" ]; then
  # The runner is committed 100644 (no chmod here), so run it through bash. It
  # owns the libtest `e2e:: --ignored` opt-in and the per-binary pool; the
  # E2E_THREADS/E2E_JOBS overrides pass through (unset E2E_THREADS defaults to
  # 4 threads, unset or empty E2E_JOBS to the runner's min(8, cores) binaries).
  step
  export E2E_THREADS="${E2E_THREADS:-4}" E2E_JOBS="${E2E_JOBS:-}"
  bash scripts/test-e2e.sh

  # The suite e2e is opt-in via APG_SUITE_E2E=1 (the boundary cases self-skip
  # otherwise), and every agent's shell grant is deny-by-default exact patterns
  # — an env-prefixed `bun test …` matches no allow rule, so this granted gate
  # command is the runnable entry point. The candidate is the debug build the
  # `cargo build` step above just produced; scenarios use scratch /tmp repos
  # only (global.constraint.no-real-project-test).
  step
  (cd opencode-suite && APG_SUITE_E2E=1 APG_BINARY="$BIN" bun test tests/boundary.e2e.test.ts)
fi

printf '\n=== gate GREEN ===\n'
