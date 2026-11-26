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
#   scripts/gate.sh --e2e    # ... then the e2e tier's HERMETIC selection: the
#                            #     per-binary runner scripts/test-e2e.sh skipping
#                            #     every scenario that needs a per-language
#                            #     frontend or the pinned-scale rescan, AND the
#                            #     opencode-suite boundary e2e (scratch /tmp
#                            #     repos only, never a real project)
#   scripts/gate.sh --e2e-full
#                            # ... then the COMPLETE e2e tier: the same runner
#                            #     with NO --exclude (the real-frontend
#                            #     acceptance scans and the pinned-scale rescan
#                            #     included), AND the opencode-suite boundary e2e
#
# E2E SELECTION (why --e2e excludes what it excludes)
# Hermetic means the built `apg` binary alone: its bundled structural scanner
# (`structfrontend`), its bundled agent frontend (`agentfrontend`),
# `touch_db`/`scan_checkout` payload fixtures, git, node/plan/review/artifacts
# and the suite boundary e2e. A scenario is therefore excluded from `--e2e`
# when it needs one of the twelve per-language frontends (`gofrontend`,
# `javafrontend`, `cppfrontend`, `rustfrontend`, `tsfrontend`,
# `csharpfrontend`, `pyfrontend`, `cobolfrontend`, `sqlfrontend`,
# `psfrontend`, `terraformfrontend`, `bicepfrontend`) — via an explicit
# `--language <lang>` or an auto-detected fixture carrying that language's
# sources — or when it is the pinned-scale
# 50k-file one-file-rescan acceptance. HERMETIC_E2E_EXCLUDES below holds one
# entry per scenario, never a glob: an unclassified NEW test therefore runs
# in BOTH modes until it is classified there.
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

# The run's mode: no argument is the default gate, `--e2e` appends the hermetic
# e2e selection, `--e2e-full` the complete one. Any other mode is refused.
MODE="${1:-}"
case "$MODE" in
  '' | --e2e | --e2e-full) ;;
  *)
    printf 'gate: unknown mode: %s (expected --e2e or --e2e-full)\n' "$MODE" >&2
    exit 2
    ;;
esac

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
if [ -n "$MODE" ]; then
  if [ "$MODE" = "--e2e" ]; then selection="hermetic"; else selection="full"; fi
  STEPS+=(
    "bash scripts/test-e2e.sh   (e2e: $selection; E2E_JOBS binaries x ${E2E_THREADS:-4} threads)"
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

# The hermetic selection's exclusions (`--e2e` only; `--e2e-full` passes none).
# Each entry is `<binary>[:<libtest-name-substring>]` for scripts/test-e2e.sh.
# One entry per scenario — no prefix globs — so a newly added test joins BOTH
# modes until it is classified here (see the header's E2E SELECTION note).
HERMETIC_E2E_EXCLUDES=(
  # The per-language acceptance binaries: every scenario in each spawns
  # that language's frontend (cobol/csharp/ps/sql/terraform respectively).
  cobol_scan_e2e
  csharp_scan_e2e
  ps_scan_e2e
  sql_scan_e2e
  terraform_scan_e2e

  # main_e2e — Go fixtures (auto-detected, or an explicit `--language go`):
  # each scan spawns gofrontend.
  main_e2e:acceptance_scratch_repo_real_init_scan_burst_read_your_writes_and_session
  main_e2e:acceptance_scan_freshness_fast_path_noop_and_content_edit_fallback
  main_e2e:acceptance_scan_emits_per_phase_timing_report_with_fast_path_skip_marker
  main_e2e:forced_scan_bypasses_the_fast_path_and_the_warm_cache_on_an_unchanged_tree
  main_e2e:scan_is_checkout_independent_and_query_stays_stored
  main_e2e:scan_hygiene_excludes_git_target_and_gitignored_trees
  main_e2e:acceptance_targeted_rescan_equivalence_leaf_body_signature_rename
  main_e2e:acceptance_incremental_universe_includes_reused_facts_newer_than_local_export
  main_e2e:acceptance_cross_worktree_cache_sharing_exactness
  main_e2e:mid_sequence_dml_failure_falls_back_to_the_full_load
  main_e2e:scan_then_immediate_plan_verify_treats_the_branch_graph_as_fresh
  main_e2e:post_scan_non_session_bookkeeping_mutations_treat_the_graph_as_fresh
  main_e2e:zero_target_incremental_scan_keeps_the_agent_roster_without_duplicates
  # ... plus the scans that pair gofrontend with another frontend: the
  # structural claim (go+md, md+go) and the mixed Go+Python single graph.
  main_e2e:structural_scan_emits_file_node_per_tracked_file
  main_e2e:acceptance_markdown_scratch_modules_sections_and_code_types
  main_e2e:acceptance_python_mixed_language_single_graph

  # main_e2e — Java fixtures (spawned javafrontend).
  main_e2e:java_targeted_scan_is_edge_exact_against_a_full_scan
  main_e2e:java_first_targeted_scan_after_a_seeded_full_scan_compiles_no_unchanged_package
  main_e2e:java_default_package_identity_end_to_end

  # main_e2e — Rust fixtures (`--language rust`, or a directly spawned
  # rustfrontend) for the Cargo-project and planned-node-realization scans.
  main_e2e:rust_all_manifest_discovery_nested_workspace_and_exclusions
  main_e2e:rust_nested_fqn_clean_and_unshadowed
  main_e2e:rust_default_libbin_crate_roots_are_distinct
  main_e2e:rust_multitarget_crate_fqns_are_stable
  main_e2e:planned_node_of_every_kind_realizes_on_a_full_scan
  main_e2e:planned_node_in_a_separate_cargo_project_realizes_like_the_root_crate
  main_e2e:planned_node_realizes_when_its_file_is_reused

  # main_e2e — TypeScript fixtures (spawned tsfrontend).
  main_e2e:js_only_repo_accepts_all_four_js_extensions
  main_e2e:mixed_js_ts_repo_resolves_both_directions
  main_e2e:dynamic_js_yields_only_categorised_unresolved
  main_e2e:self_scan_of_ported_tslib_produces_symbols

  # main_e2e — Python fixtures (spawned pyfrontend).
  main_e2e:acceptance_python_scratch_modules_files_and_code_types
  main_e2e:acceptance_python_resolution_and_unresolved_classification
  main_e2e:acceptance_python_incremental_graph_equivalence
  main_e2e:acceptance_python_environment_model_and_external_resolution
  main_e2e:python_language_plumbing_detection_and_availability

  # main_e2e — the pinned-scale acceptance (>50k tracked files, a 60 s
  # one-file-rescan bound): hermetic in language terms, but deliberately the
  # heavy scenario the full selection alone runs.
  main_e2e:acceptance_one_file_rescan_stays_under_the_value_threshold

  # splice_e2e — Rust, rust+agent, cobol, sql and ps fixture scans.
  splice_e2e:planned_node_realizes_on_the_splice_for_every_kind
  splice_e2e:realization_keeps_every_incident_edge_on_the_real_node
  splice_e2e:still_planned_node_keeps_its_node_and_edges_across_a_scan
  splice_e2e:copy_uses_edge_survives_a_splice_rescan
  splice_e2e:copy_uses_edge_survives_a_warm_full_load_reassembly
  splice_e2e:sql_uses_edge_properties_survive_splice_and_warm_reassembly
  splice_e2e:delta_cross_cut_reference_publishes_a_resolved_calls_edge
  splice_e2e:delta_retains_authored_code_target_edges_without_a_full_load
  splice_e2e:deleted_agent_definition_rescan_matches_a_full_rebuild
  splice_e2e:ps_function_to_struct_label_change_splices_like_a_full_rebuild
  splice_e2e:ps_struct_to_module_label_change_splices_like_a_full_rebuild

  # project_cmd_e2e — the three real-CLI scans/merges over Go fixtures.
  project_cmd_e2e:warm_cache_project_start_spawns_no_frontend_and_equals_a_full_scan
  project_cmd_e2e:merge_spec_only_lands_authored_rows_in_main_db
  project_cmd_e2e:project_start_and_merge_from_the_main_cli_succeed

  # The remaining real-scan scenarios over Go / md+go / rust fixtures.
  cache_e2e:cross_worktree_cached_reuse_is_repo_relative
  ingest_e2e:splice_path_is_taken_and_equals_a_full_rebuild
  plan_cmd_e2e:apply_gate_realizes_a_planned_node_in_a_separate_crate
  artifacts_e2e:forced_scan_discards_the_derived_index_and_rebuilds_it_from_scratch
  artifacts_e2e:pre_scan_write_node_lands_for_the_cold_scan_while_write_project_refuses
  progress_e2e:piped_stderr_carries_no_progress
  progress_e2e:pty_scan_keeps_progress_out_of_the_frontend_log
  progress_e2e:no_progress_flag_is_accepted_and_silent
  progress_e2e:fresh_fast_path_is_progress_silent
  progress_e2e:pty_scan_streams_one_symbol_per_node_and_a_matching_summary
)

if [ -n "$MODE" ]; then
  # The runner is committed 100644 (no chmod here), so run it through bash. It
  # owns the libtest `e2e:: --ignored` opt-in and the per-binary pool; the
  # E2E_THREADS/E2E_JOBS overrides pass through (unset E2E_THREADS defaults to
  # 4 threads, unset or empty E2E_JOBS to the runner's min(8, cores) binaries).
  step
  export E2E_THREADS="${E2E_THREADS:-4}" E2E_JOBS="${E2E_JOBS:-}"
  E2E_ARGS=()
  if [ "$MODE" = "--e2e" ]; then
    for entry in "${HERMETIC_E2E_EXCLUDES[@]}"; do
      E2E_ARGS+=(--exclude "$entry")
    done
  fi
  # bash 3.2 + `set -u`: expand the flag list only when it is non-empty, so the
  # full selection invokes the runner with no arguments at all.
  bash scripts/test-e2e.sh ${E2E_ARGS[@]+"${E2E_ARGS[@]}"}

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
