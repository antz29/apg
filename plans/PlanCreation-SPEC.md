# PlanCreation-SPEC.md — graph-native plan creation flow

Status: working spec (authoring in progress; materialized into the graph as a spec project later).
Parent capability: **GraphModel-SPEC.md** (4-tier graph; a project = a branch change-set).
Invariants: **Invariants-SPEC.md** (graph-wide invariant mechanism, used by this flow).
Siblings: **SpecCreation-SPEC.md** (input), **PlanExecution-SPEC.md** (output).

## Scope

The end-to-end process by which an **approved spec graph** (the project's proposed reality,
tiers 1–3) becomes an **approved, review-clean plan graph** — the **delta from current reality
to proposed reality**: a phased plan for what to create/modify in **tier 4** (code) to make the
proposed reality actual. The plan-writer operates separately from the spec-writer (who authored
tiers 1–3): it reads the proposed reality against the current reality (the present code graph)
and plans the tier-4 work. Every spec requirement is satisfied by exactly one phase, and each
phase carries an implementer-workable task list. Plan authoring happens in the project's
worktree + branch (created at project start, GraphModel-SPEC.md); the plan JSONL is transient
and branch-local. This spec covers **plan authoring only** — execution and apply are other
specs in the family.

## Goal

An approved plan whose **structure is valid** (CLI-enforced), whose **breakdown is sound**
(structural review before per-phase work), and whose **phases are individually and holistically
review-clean** (reviewer-loop-enforced) — with invariant rules applying as in
Invariants-SPEC.md.

## The flow (navigator-orchestrated, two holistic gates)

*Runs in the project's worktree + branch; the plan JSONL is transient and branch-local (it
never merges — the plan is the roadmap, not the record).*

1. **Breakdown** (single plan-writer): analyze the approved spec → `apg plan init <project>`
   (Plan node + strategy) + every `PlanPhase` (title, deliverable) + `Satisfies` +
   `Gates`/prereq edges. The skeleton only — no tasks yet.
2. **Structural holistic review #1** (single plan-review): checks the breakdown itself — every
   requirement Satisfied by **exactly one** phase, no Gates cycles, no empty phases, phase
   ordering/dependencies coherent, phase set matches the spec. **Structural feedback routes
   back to the breakdown writer** → fix → re-review → until the breakdown is structurally green.
3. **Parallel per-phase writing** (N plan-writers): one per phase, authoring only that phase's
   `Task` nodes (title, kind/tier, `Builds` futures, `Anchors`). Phases write disjoint FQNs;
   writes serialize on the spec flock (process-wide, spanning load→modify→write — no lost edges).
4. **Parallel per-phase review** (N plan-review, cycled): one per phase. Attach Feedback → route
   to that phase's writer → fix **through the authoring path** (`apg plan add task` is
   upsert-by-FQN; a corrected re-add *is* the fix — no correction tools) → resolve/reject →
   until each phase is individually green (zero feedback).
5. **Final holistic review** (single plan-review, cycled): across the whole plan — cross-phase
   consistency the per-phase reviews cannot see (requirement coverage, Gates cycles, phase
   ordering, consistent kind/tier classification, Builds targets declared in the spec, anchors
   resolve). If it flags phase X → **phase X's writer** fixes → which **triggers a single
   per-phase review of phase X** to verify → then the final holistic review again → until the
   entire plan is green.
6. **Approval**: all feedback resolved → plan approved → navigator hands off to execution
   (next spec's scope).

## Routing rules

- **Structural issues** (breakdown-level: phase set, ordering, gates, requirement coverage) →
  the breakdown writer.
- **Phase-level issues** (from per-phase review or the final holistic review) → that phase's
  writer.
- A phase touched by the final holistic review **re-enters its per-phase review** before the
  next holistic pass.

## Role separation (strict)

- **plan-writer**: authors (breakdown stage or per-phase stage) and actions feedback
  (`--fix`/`--wont-fix`), never resolves. Fixes always go through the authoring path.
- **plan-review**: attaches/resolves/rejects feedback; scoped per-phase, structurally, or
  holistically. Never authors.
- **codebase-navigator**: orchestrates the stage sequence, spawns the parallel subagents,
  routes feedback by scope, and decides when resolution has terminated the loop.

## Correctness layers (unchanged)

- **CLI envelope validation** at write time: phase `--satisfies` targets are real
  requirements; task kind/tier rules; `--builds` targets are declared futures; anchors resolve
  (real code FQN or declared Future).
- **Structural + reviewer loops** as the semantic backstop (breakdown soundness, per-phase and
  cross-phase consistency).
- The flow works identically with zero invariants.

## Invariant usage in plan creation

- **Writer awareness**: the navigator injects the active invariant set into plan-writer
  prompts; writers also query `apg invariants` — known rules (e.g.
  `invariant/plan.task-kind-in-set`) hold at authoring.
- **Reviewer checking**: plan-review checks the plan/phase/task nodes against their
  `GuardedBy` invariants; a comment enforcing a known rule links `Checks` → Invariant.
- **Emergent addition**: feedback patterns → navigator proposes a new invariant → user
  confirms → `apg invariant add`.

## Out of scope (other specs in this family)

- Plan execution, per-phase completion (`apg plan complete`), and the plan-end human gate
  (PlanExecution-SPEC.md).
- Applying the diff / the present state (PlanCompletion-SPEC.md).
- Removing the obsolete `apg plan retag` correction tool (recorded as a separate cleanup —
  the authoring-path fix principle makes it unnecessary).

## Implementation surface (when approved)

- Orchestration is the navigator's job: the stage sequence, parallel subagent spawning,
  scoped routing, and termination are driven by agent prompts (plan-writer breakdown-vs-phase
  modes, plan-review scoped modes, navigator procedure).
- The invariant mechanism from Invariants-SPEC.md (`Invariant` node, `GuardedBy`/`Checks`,
  `apg invariant add`, `apg invariants`) provides writer awareness + reviewer checking.
- The existing `apg plan` CLI (init/add phase/add task/link) already supports every write this
  flow performs — no new plan tooling required.
- Cleanup (separate): remove `apg plan retag` (CLI + suite tool) and its test.
- Tests: invariant-aware review round-trips; scoped-review routing (structural vs phase);
  upsert-by-FQN as the fix path.