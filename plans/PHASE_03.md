# PHASE_03 — Close the open spec divergences

References: **REVIEW.md** (the `[ ]` items), **PlanCreation-SPEC.md**, **PlanExecution-SPEC.md**,
**PlanCompletion-SPEC.md**, **Invariants-SPEC.md**, **0.10.0-PLAN.md**.
Scope: every unchecked REVIEW.md item is either implemented or explicitly reconciled — zero open
divergences between the code and the spec family.

## Deliverable

REVIEW.md is all `[x]` (fixed) or `[~]` (reconciled, with the spec prose updated to match); no
`[ ]` item remains.

## Work items

1. **Plan CLI / tooling**:
   - `apg_plan_add.ts` description drops the retired `human` task kind ({source, test, gate,
     docs}).
   - `apg plan add phase --prereq` runs the same Gates-cycle check `apg plan link` uses
     (self-gate + transitive cycle) before any write.
   - `apg plan add task` and `apg plan link` verify the target phase exists
     (`plan.phase-NN` must resolve) — CLI-envelope hole closed.
   - Exactly-one-phase Satisfies detection: `apg plan phases` (or the unresolved lint) reports
     a requirement Satisfied by more than one phase.
   - Durable phase milestone: `Record::PlanPhase` gains `status` (pending | done);
     `apg plan complete` writes it — a completed phase is distinguishable from tasks-done +
     feedback-resolved.
2. **Apply / coherence gate**:
   - Invariant evaluation: `apg plan apply` checks `GuardedBy` invariants on the branch's
     plan/spec/code nodes (an invariant the merge would violate blocks apply). Confirm scope
     (which node kinds the gate walks) at kickoff.
   - Plan JSONL consumption: decide implement-vs-reconcile — explicitly drop the plan JSONL
     after a successful apply, or record the gitignored/branch-local transience as the design
     (PlanCompletion-SPEC "the plan is transient and gone").
   - Human-gate leg: reconcile as navigator-prose-only (per PlanCompletion-SPEC the human gate
     is the navigator's job; the CLI gate is planned-node realization + feedback + invariants).
3. **Agent prose (plan creation)**:
   - plan-writer breakdown stage = **skeleton only, no tasks** (remove the tasks-in-breakdown
     and proposal-step prose).
   - codebase-navigator plan-creation orchestration: stage sequence, parallel per-phase
     spawning, routing-by-scope (structural → breakdown writer; phase-level → phase writer),
     and the termination decision.
   - Re-entry rule (a holistic-flagged phase re-enters its per-phase review before the next
     holistic pass) in the navigator/plan-review prose.
   - plan-writer + plan-review invariant awareness: `apg_invariants` grant + awareness prose
     (mirror spec-writer/spec-review; writers/reviewers never materialize).
4. **Data / docs residue** (items not already swept in PHASE_02):
   - Any remaining `future/` literal in spec-data prose (apg-0.9.3.jsonl) and stale
     archive/complete prose in `apg_spec_unresolved.ts`, `apg_review_resolve.ts`, and
     `cosanima-mcp/spec.R6`'s body tool-list.
5. **Missing tests**:
   - apply → merge → rebuild with delivered descriptions resolving (the apply path beyond the
     promote round-trip).
   - cross-project FQN stability through squash/rebase.
   - PHASE_04 migration round-trip (spec JSONL → project-scoped FQNs with incident edges
     re-pointed).
   - scoped-review routing test (structural vs phase).

## Deliverables / done gate

- `cargo test` green + clippy clean.
- REVIEW.md has no `[ ]` line: every item is `[x]` or `[~]` with the reconciliation recorded
  in the spec/agent prose.
- `rg -i 'future/' src/ .opencode/ apg/specs/` clean (no `future/` literal anywhere).

## Out of scope (later phases)

- Docs/agents/dogfood under the finalized model (PHASE_04).
- Release (PHASE_05).