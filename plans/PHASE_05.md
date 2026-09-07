# PHASE_05 — Flow orchestration + docs + E2E

References: **SpecCreation-SPEC.md**, **PlanCreation-SPEC.md**, **PlanExecution-SPEC.md**,
**PlanCompletion-SPEC.md**, **GraphModel-SPEC.md**, **Invariants-SPEC.md**, **0.10.0-PLAN.md**.
Scope: turn the four flow specs into orchestrated agent behavior + current docs, and dogfood
the whole loop once.

## Deliverable

The full spec → plan → execute → apply flow works end to end, agent-orchestrated, on a project
branch: spec-writer authors tiers 1–3; plan-writer authors the planned Implementation nodes and
plans the tier-4 delta (breakdown →
structural review → parallel per-phase write/review → final holistic review); implementers build
in the branch; the final implementation review discovers divergence (fix or reconcile the
spec); the human gate approves; apply merges + rebuilds. One dogfooded project lands on `main`.

## Work items

1. **spec-writer**: tiers 1–3 authoring (Requirements, Domain, Solution nodes from PHASE_01);
   interview → propose → design approval → author → self-lint; invariant awareness (prompt
   injection + `apg invariants`); reconciliation mode (used by the final implementation
   review to tie the spec back to the implementation).
2. **spec-review**: reviews the spec against the branch graph + `GuardedBy` invariants; cites
   `Checks` where relevant; resolves/rejects; never authors.
3. **plan-writer**: the tier-4 delta — authors the `planned` Implementation nodes + the plan; breakdown stage → structural holistic review →
   parallel per-phase writing → parallel per-phase review → final holistic review (two holistic
   gates, PHASE_02/03 tooling).
4. **plan-review**: per-phase, structural, and holistic scopes; approval-only wont-fix.
5. **implementation-phase-reviewer**: phase review (branch scan) + the final implementation
   review — divergence discovery (may re-flag an approved wont-fix); resolutions: fix the code
   or issue a spec reconciliation (spec-writer through the spec-review cycle).
6. **codebase-navigator**: branch lifecycle procedure (worktree + branch at project start,
   per-branch DB build), feedback routing by scope, human-gate summary (work, gotchas,
   deviations still present), and the apply procedure (coherence gate → merge → rebuild, per
   PHASE_03).
7. **implementer**: assertion-only `plan done`, task notes, branch commits (`git add`/
   `commit` only; no push/tag), `apg_review_action`.
8. **Agent prose**: update all seven agents (navigator, spec-writer, spec-review, plan-writer,
   plan-review, implementation-phase-reviewer, implementer) to the branch model + tier roles +
   invariant awareness; update **AGENTS.md** and **README.md** (node taxonomy, spine, invariant
   tools, plan lifecycle, apply).
9. **E2E dogfood**: one project through the whole flow on this repo (or a fixture): spec
   (tiers 1–3) → plan → implement (branch) → final review + reconciliation → human gate →
   apply → verify on `main`.

## Deliverables / done gate

- `cargo test` green (no regressions from prose/tool changes).
- E2E: the dogfooded project's spec is applied to `main`; delivered descriptions resolve
  against `main`'s rebuilt graph; the plan is retired; no `future/` FQNs remain.
- Docs current: AGENTS.md/README describe the 4-tier graph, invariants, and the branch
  lifecycle.

## Out of scope (1.0.0 / later)

- The 1.0.0 Cosanima release (its own spec).
- Database/infrastructure nodes.
- Cross-harness orchestration and the MCP-server unification.