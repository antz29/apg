# suite-split-PHASE_02 — Init hygiene (shadow check, stale-suite pruning, test paths)

References: **suite-split-PLAN.md**, `src/main.rs` (`duplicate_install_files`, `cmd_init`), `src/invariant_cmd.rs`.
Scope: three mechanical fixes made necessary (and one surfaced) by the move.

## Deliverable

`duplicate_install_files` no longer flags dependency noise; `apg init` prunes stale
apg-owned files from `~/.opencode/` so removed-suite tools/agents don't shadow forever; the
invariant suite smoke test imports the tools from their new home.

## Work items

1. **Skip `node_modules` (and lockfiles) in the shadow walk** (`src/main.rs:616`): the
   recursive walk currently descends into `.opencode/node_modules` and flags every dep file
   that also exists in `~/.opencode/node_modules` — the bulk of the "load of errors" when
   running `apg init` in the apg repo (and in any project that keeps its own tool
   dependencies). Skip `node_modules` during the walk; the project's own `package.json` /
   `package-lock.json` / `bun.lock` are not part of the suite and should not be flagged either.
2. **Prune stale apg-owned files from `~/.opencode/` on init** (`cmd_init`): after the
   install/update loops, delete `tools/apg_*.ts` not in the current `SUITE_TOOLS` (the `apg_`
   prefix is the apg namespace), the known six distributed agent filenames not in the current
   `AGENTS`, and `lib/apg.ts` if it ever leaves the suite. Anything else in `~/.opencode/` (a
   user's own tools/agents) is preserved. `apg init` already overwrites edited suite files
   (`write_if_changed`), so pruning the same owned set is consistent. Surface a short note in
   the init output when stale files were removed.
3. **Re-point the invariant smoke test** (`src/invariant_cmd.rs:848-850`): the test writes a
   harness that imports `apg_invariant_add.ts` / `apg_invariants.ts` from
   `manifest/.opencode/tools/`; update to `manifest/opencode-suite/tools/` (and confirm the
   import still resolves — `opencode-suite/node_modules` from PHASE_01).

## Deliverables / done gate

- Unit tests cover the node_modules skip (extend `duplicate_install_files_detects_overlap` at
  `src/main.rs:1262`) and the stale-suite prune (removed tool/agent gone, user-owned files
  preserved).
- `cargo test` green; clippy clean.
- `apg init .` in the repo prints no shadow warnings for node_modules (suite files are gone
  from `.opencode/` by PHASE_01, so nothing else should flag) and prunes any stale
  `~/.opencode` leftovers.

## Out of scope (later phases)

- Docs and `~/.opencode` sync + the full in-repo dogfood loop (PHASE_03).