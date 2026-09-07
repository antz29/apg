# PlanExecution-SPEC.md — graph-native plan execution flow

Status: working spec (authoring in progress; materialized into the graph as a spec project later).
Parent capability: **GraphModel-SPEC.md** (4-tier graph; a project = a branch change-set).
Invariants: **Invariants-SPEC.md** (graph-wide invariant mechanism, used by this flow).
Siblings: **SpecCreation-SPEC.md**, **PlanCreation-SPEC.md** (input), **PlanCompletion-SPEC.md** (output — apply).

## Scope

The end-to-end process by which an **approved plan** becomes **working, reviewed code in the
project branch**: the tier-4 delta is implemented and reviewed — phase reviews against branch
scans, a final divergence-discovering implementation review, and a human gate. The apply act
(merge the branch into `main` + rebuild the present graph) is **PlanCompletion-SPEC.md**'s
scope; this spec ends at the human gate + handoff. The project's worktree + branch already
exist (created at project start, GraphModel-SPEC.md) — execution happens in them. The release
ceremony that follows apply (tag/push/release records) is human-owned and out of scope here.

## Goal

An approved plan delivered as code where: implementation work is an **implementer assertion**
(no per-task scan), each phase is **review-clean against a branch scan** (the proposed
reality's code), the whole implementation is **reviewed against the spec with divergence
discovered and resolved** (by fixing code or reconciling the spec), and the human **approves**
the outcome — ready for the apply act (PlanCompletion-SPEC.md).

## The flow

1. **Branch context** — the project's worktree + branch off `main` already exist (created at
   project start, GraphModel-SPEC.md); `main` is untouched during execution. Implementers work
   in the branch and commit to it.
2. **Implementation (assertion-only)** — the implementer works a phase's tasks, marking each
   `done` via `apg plan done`. This is the implementer's **assertion only** — no scan, no
   promotion. The implementer may **attach notes to a task** for concerns or deviations that
   arose during implementation.
3. **Phase review (branch scan = proposed reality)** — when all tasks in a phase are done, the
   phase review starts with a **scan of the branch**; that graph shows the proposed reality's
   code taking shape — and the scan **replaces realized planned nodes** (a scanned node at a
   planned FQN supersedes the planned one and re-points its incident edges). The implementation-phase-reviewer reviews the phase against the spec, the
   invariants, etc., attaches `Feedback`, and routes back to the implementer to fix. **When all
   feedback is resolved** (the reviewer approves or rejects every disposition — see rules), the
   phase is marked complete and execution proceeds to the next phase. **Nothing is applied to
   the present** — the branch is not merged yet.
4. **Final implementation review (divergence discovery)** — when all phases are complete, the
   whole plan is reviewed: the spec is compared against the code in the branch. The reviewer
   also examines prior feedback — an approved wont-fix may be re-flagged and re-raised as
   divergence here. Feedback is created and implementers are issued to fix issues. **A
   resolution path for divergence is spec reconciliation**: issue a spec-writer to update the
   spec to tie it back to the implementation, through the spec-review cycle. **When all
   feedback is resolved**:
5. **Human gate** — execution pauses. The navigator summarises the work, any gotchas, and any
   **deviations still present** (reviewer-approved wont-fix items and task notes), inviting the
   human to raise issues (which route back into fixes). **Assuming the human is happy**:
6. **Apply (agent-operated; push/tag human)** — the merge of the branch into `main`, the fresh
   scan on `main`, and the coherence-gate verification (every planned node realized; all
   feedback resolved; human gate passed) are the **apply act**, covered by
   **PlanCompletion-SPEC.md**. The merge is agent-operated; push/tag remain human.
7. **Push/tag** — human-owned, never an agent.

## Rules

- **`apg plan done` is assertion-only** — decoupled from promotion and graph verification
  (nothing is promoted by `plan done`; **branch scans replace planned nodes**, which is the
  promotion mechanism). The "a task cannot be done until its code exists" property **moves
  into the apply gate** (PlanCompletion-SPEC.md), which verifies every planned node was
  realized against the merged graph before applying.
- **`apg plan complete` is a milestone only** — decoupled from `Implements` and plan
  retirement. The plan **survives until apply**, which materializes all delivery records in
  one act (this also resolves non-code deliverables cleanly: delivery is recorded consciously
  at apply time).
- **Wont-fix is never terminal unapproved** — a writer's `--wont-fix` disposition is a
  *proposal* the reviewer must approve (resolve) or reject. Universal, for all feedback nodes
  anywhere.
- **Divergence is discovered in the final implementation review**, not as a separate trigger.
  Its resolutions are: fix the code, or **reconcile the spec** (spec-writer revision through
  the spec-review cycle).
- **Scan cadence**: one scan per phase review (of the branch) + the final implementation
  review scan of the branch + one fresh scan on `main` at apply. No per-task scans.
- **Deviations surfaced to the user**: anywhere the final output deviates from the spec is
  highlighted at the human gate; the durable record is the reconciled spec.

## Invariant usage in execution

- **Writer awareness**: implementer is aware of the invariants in scope (prompt injection +
  `apg invariants`) — code/process/graph-integrity rules hold while writing.
- **Reviewer checking**: the implementation-phase-reviewer checks the phase and its code
  against `GuardedBy` invariants (on code + plan nodes); a comment enforcing a known rule
  links `Checks` → Invariant.
- **Emergent addition**: execution feedback patterns → navigator proposes new invariants
  (product invariants like "release records' version equals the tag" are the classic
  execution-discovery case) → user confirms → `apg invariant add`.

## Role separation (strict)

- **implementer**: works tasks, marks `done` (assertion), attaches task notes, actions
  feedback (`--fix`/`--wont-fix` proposal). Git: `add`/`commit` into the worktree branch only.
- **implementation-phase-reviewer**: reviews each phase and the final whole-plan
  implementation against the spec + invariants; attaches/resolves/rejects feedback; marks
  phases complete. No edit, no scan, no task mutation, no authoring.
- **spec-writer / spec-review**: used for spec reconciliation (an outcome of the final
  implementation review), via the normal spec-review cycle.
- **codebase-navigator**: orchestrates the stage sequence, routes feedback by scope, produces
  the human-gate summary, and operates the apply act (merge + rebuild) on human approval — see
  PlanCompletion-SPEC.md.
- **Human**: approves at the gate; owns push/tag (and the release ceremony, a later spec).

## Out of scope (other specs in this family)

- The apply act — merge + rebuild + coherence gate (PlanCompletion-SPEC.md).
- The release ceremony: tag, push, release records, bottle sha256 (human-owned).
- No spec archive anywhere (GraphModel-SPEC.md — the graph always represents current reality;
  git holds history).
- Removing the obsolete `apg plan retag` correction tool (recorded separate cleanup — the
  assertion + review + apply model makes it unnecessary).

## Implementation surface (when approved)

- `apg plan done`: drop promotion + graph verification (assertion only; keep `undone`).
- `apg plan complete`: drop `Implements` materialization and plan retirement; keep the
  all-tasks-done + all-feedback-resolved gate.
- The apply act (merge + rebuild + coherence gate) is PlanCompletion-SPEC.md's surface; the
  planned-node realization verification moves to its gate.
- Implementer: task-note capability + grant (`Task` is already an allowable `Details` target).
- A merge-capable agent grant (`git merge` of the project branch into `main`, exercised at
  apply; push/tag remain denied).
- The invariant mechanism from Invariants-SPEC.md provides awareness + checking.
- Agent-prose updates: implementer, implementation-phase-reviewer, spec-writer (reconciliation
  mode), codebase-navigator (branch context — the worktree exists from project start — gate
  summary, apply handoff); AGENTS.md.
- Tests: apply-gate rejects a branch with unrealized planned nodes; plan survives until apply;
  phase-complete milestone without Implements; wont-fix approval-only lifecycle; task-note
  round-trip.