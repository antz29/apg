# apg-plan-phases-scoping — scope the tool's requirement set to the project

> High-level change-set definition. The durable spec lives in `apg/layers/` (authored
> through `apg_node`/`apg_edge`); this file is the work brief, not the graph.

## Goal
`apg_plan_phases` must report only the requirements the **current project / branch** is
responsible for — not every global requirement in the layers store.

## Why
`opencode-suite/tools/apg_plan_phases.ts` loads **all** `Requirement` nodes
(lines 40–42) and reports any without a `Satisfies` edge as "unsatisfied" (line 82).
Requirements delivered by other projects (or shipped with no plan at all) therefore
read "unsatisfied" for every project forever — noise that masks real gaps. Observed in
apg-cleanup: 24–25 previously-delivered requirements read "unsatisfied".

## Model
The binary's `solution_nodes_added_on_branch` (`src/plan_cmd/verify.rs`) already answers
"what did this branch add?"; mirror that principle — a requirement is in scope for the
project when this project / branch is responsible for it.

## Scope
- Scope the requirement set in `opencode-suite/tools/apg_plan_phases.ts` to branch-added
  requirements (or requirements Satisfied by this project's phases), so the
  "unsatisfied" / "over-satisfied" findings become project-local.
- Keep the existing findings unchanged: no-tasks, done-but-under-review, Gates cycles.
- Add / extend the suite test coverage for the scoped behaviour.
- Anchor: `solution.container.opencode-suite`.
- The suite is **product source** embedded via `include_str!` and installed by
  `apg init`; the repo's own `.opencode/**` are regenerated out-of-band — never edited
  here.

## Non-goals
- No change to the `apg` binary or to other suite tools.

## Acceptance
- Suite tests (`bun test`; `src/tslib` `node --test` where relevant) green.
- The suite e2e (`full_dogfood_round_trip_suite_tool_ops_inside_the_worktree`,
  `suite_tools_query_error_guard_is_structural`) green.
- `scripts/gate.sh` green; `apg plan verify` green; merge.

## Open questions
- Exact definition of "in scope": requirements added on the branch vs requirements with
  a `Satisfies` in the project vs both. Decide in the spec (spec-first).
