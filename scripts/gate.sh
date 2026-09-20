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
# Scaffolded agents are granted this file as an exact bash pattern — they may
# RUN it, but `scripts/**` is not in their edit grant, so the sequence cannot be
# rewritten beneath them. Changing a step is a maintainer change.
#
# USAGE
#   scripts/gate.sh          # fmt --check, check, clippy, build, test
#   scripts/gate.sh --e2e    # ... then the e2e tier (opt-in, serial)
#
# `cargo test` here is the repo's DEFAULT suite. Since the test-tier split it is
# the unit+int tiers only, seconds-fast; the e2e tests are `#[ignore]`d and are
# run only by `--e2e` (or `cargo test-e2e`). E2E_THREADS bounds their
# parallelism (default 1: many concurrent `db.lbug` opens have been observed to
# flake with LadybugDB `Mmap ... failed`).
set -euo pipefail

cd "$(dirname "$0")/.."

step() { printf '\n=== %s ===\n' "$1"; }

step "cargo fmt --check   (cargo fmt fixes)"
cargo fmt --check

step "cargo check --all-targets"
cargo check --all-targets

step "cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

step "cargo build"
cargo build

step "cargo test   (default suite: unit+int)"
cargo test

step "bun test   (opencode-suite: bun)"
(cd opencode-suite && bun test)

step "node --test   (src/tslib: node)"
(cd src/tslib && node --test)

if [ "${1:-}" = "--e2e" ]; then
  step "cargo test tests::e2e:: -- --ignored --test-threads=${E2E_THREADS:-1}"
  cargo test tests::e2e:: -- --ignored --test-threads="${E2E_THREADS:-1}"
fi

printf '\n=== gate GREEN ===\n'
