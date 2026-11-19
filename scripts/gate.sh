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
#   scripts/gate.sh          # actionlint, Formula ruby -c, fmt --check, check,
#                            # clippy, build, test, suite deps, bun test,
#                            # node --test
#   scripts/gate.sh --e2e    # ... then the e2e tier (opt-in): the cargo e2e
#                            #     tests AND the opencode-suite boundary e2e
#                            #     (scratch /tmp repos only, never a real project)
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
# gate validates (embedded-script style stays a review concern). The
# `opencode-suite/node_modules` bootstrap installs from the committed
# `opencode-suite/bun.lock` (`--frozen-lockfile`) only when the directory is
# absent — the cold path, a fresh worktree — and skips the install when it is
# present, so gate-time network exposure is bounded to that first run.
#
# `cargo test` here is the repo's DEFAULT suite. Since the test-tier split it is
# the unit+int tiers only, seconds-fast; the e2e tests are `#[ignore]`d and are
# run only by `--e2e` (or `cargo test-e2e`). E2E_THREADS bounds their
# parallelism (default 1: many concurrent `db.lbug` opens have been observed to
# flake with LadybugDB `Mmap ... failed`).
set -euo pipefail

cd "$(dirname "$0")/.."

BIN="$PWD/target/debug/apg"

# The run's full ordered step list, declared up front so the end-of-run summary
# can name every step — including the ones an earlier failure left not run.
STEPS=(
  "actionlint   (.github/workflows/*.yml)"
  "ruby -c   (Formula/*.rb)"
  "cargo fmt --check   (cargo fmt fixes)"
  "cargo check --all-targets"
  "cargo clippy --all-targets -- -D warnings"
  "cargo build"
  "cargo test   (default suite: unit+int)"
  "opencode-suite deps   (bootstrap from bun.lock when absent)"
  "bun test   (opencode-suite: bun)"
  "node --test   (src/tslib: node)"
)
if [ "${1:-}" = "--e2e" ]; then
  STEPS+=(
    "cargo test e2e:: -- --ignored --test-threads=${E2E_THREADS:-1}"
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
cargo fmt --check

step
cargo check --all-targets

step
cargo clippy --all-targets -- -D warnings

step
cargo build

step
cargo test

step
if [ -d opencode-suite/node_modules ]; then
  printf 'opencode-suite/node_modules present — skipping install\n'
  skipped "node_modules present"
else
  (cd opencode-suite && bun install --frozen-lockfile)
fi

step
(cd opencode-suite && bun test)

step
(cd src/tslib && node --test)

if [ "${1:-}" = "--e2e" ]; then
  step
  cargo test e2e:: -- --ignored --test-threads="${E2E_THREADS:-1}"

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
