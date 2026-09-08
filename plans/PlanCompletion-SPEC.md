# PlanCompletion-SPEC.md — done: applying the diff to the present

Status: working spec (authoring in progress; materialized into the graph as a spec project later).
Parent capability: **GraphModel-SPEC.md** (4-tier active-knowledge graph; a project = a
change-set = a git branch).
Siblings: **SpecCreation-SPEC.md**, **PlanCreation-SPEC.md**, **PlanExecution-SPEC.md**.

## Scope

The final stage of a project's lifecycle: taking a reviewed, human-approved change-set (the
project branch, containing the proposed reality in tiers 1–3 and its built tier-4 delta) and
**applying it to the present**. This is the "done" act. It covers the apply-the-diff ceremony
(merge + rebuild) and what the present graph is afterward. The release ceremony that follows
(tag, push, release records) is human-owned and not detailed here.

## What "done" is

A project was authored and built as a **proposed diff to the present graph** — a worktree +
branch off `main`. The spec (tiers 1–3) is the proposed reality; the plan drove the tier-4
delta; implementers built it; reviewers brought it to green. **"Done" is applying the diff:**
merging the branch into `main` and rebuilding `main`'s graph, so the proposed reality becomes
the current reality. After apply:

- The **present graph (main) is the new current reality** — the 4-tier active knowledge
  updated in one act: requirements, domain, and solution nodes now present; the new/updated
  code present; the "why" exists alongside the code.
- **The change-set is consumed.** The branch's proposal mechanics (spec node, phases, plan)
  are not present-state objects; what remains are the nodes describing now, attached to the
  code (via `Implements`/`Anchors`/`Details`), with the project name retained as an **origin
  label** on the annotation nodes for traceability.
- **The plan is transient and gone** (it never survives; git history is its only trace).
- **Planned nodes are gone** — the code that realizes them is present on `main`; nothing stays
  marked `planned`.
- **Nothing is archived.** The graph always represents current reality; delivered descriptions
  stay on `main` as accumulated understanding. Past reality is git history — checkout a commit
  and rebuild the graph to see it.

## The apply-the-diff sequence

1. **Change-set coherence gate** (runs before the merge):
   - every `planned` Implementation node in the branch is **realized** — a scan found real code
     at its FQN and replaced it (a planned node with no code blocks apply);
   - every phase and the whole-plan implementation review are green — all `Feedback` resolved;
   - the **human gate** has passed: the navigator summarised the work, gotchas, and deviations
     still present (reviewer-approved wont-fix items and task notes), the human raised no
     blocking issues, and approved.
2. **Merge (agent-operated)**: merge the project branch into `main`. The merge itself is part
   of the apply act; **push/tag remain human**.
3. **Rebuild `main`'s graph**: a fresh scan + ingest of `main`'s committed state (code +
   spec/note JSONLs) — the present graph now *is* the applied diff.
4. **Verify the applied present**: spot-check that the delivered descriptions' `Implements`/
   `Anchors` edges resolve against the rebuilt graph and the "why" is attached to its code.

## Dependency handling at apply

If the project depended on an in-flight project A (squash-merged into this branch), the
dependency resolves when both land: A merges to `main` first (its own apply), then this branch
is rebased onto `main` (per GraphModel-SPEC.md) before its own apply. The stable, project-scoped
FQNs keep cross-project references intact through the rebase.

## Roles

- **codebase-navigator**: runs the coherence gate, produces the human-gate summary, operates
  the merge + rebuild on human approval.
- **Human**: approves at the gate; owns push/tag and the release ceremony that follows apply.
- **Reviewers/implementers/spec-writer/plan-writer**: their work is done before apply (the
  execution flow, PlanExecution-SPEC.md); they are not involved in the apply act.

## Invariants

The invariant set (Invariants-SPEC.md) applies to the apply act like any
artifact. An invariant's body is **free prose**, so the coherence gate does not
mechanically evaluate it — instead the navigator verifies the `GuardedBy`
invariant set in scope (`apg invariants`) as part of the human gate: a
`GuardedBy` invariant the merge would violate is raised there and blocks apply
via the human's no-blocking-issues approval. This is consistent with
Invariants-SPEC's "Correctness never depends on them" (the CLI envelope + the
human gate carry the load; invariants make the in-scope rules visible, never
mechanically enforced). Emergent invariants discovered during execution were
already materialized before the gate.

## Out of scope

- The release ceremony (tag/push/release records/bottle sha256) — human-owned, later.
- Removing the obsolete `apg plan retag` correction tool (recorded separate cleanup — the
  assertion + review + apply model makes it unnecessary).
- Database/infrastructure representation (GraphModel-SPEC.md).

## Implementation surface (when approved)

- The apply act is primarily git + rebuild orchestration: `git worktree`/`git merge`/`git
  rebase` procedure in the navigator, plus the existing scan + ingest to rebuild `main`'s
  graph.
- `apg plan promote` (or its replacement) reduces to: coherence-gate check + merge + rebuild —
  the planned-node realization verification moves to the gate.
- Namespace migration: drop the `future/<project>/` FQN prefix across the existing specs and
  tooling, per GraphModel-SPEC.md (present-ness = branch membership).
- Drop `apg spec archive` (the graph always represents current reality).
- Agent-prose updates: codebase-navigator (branch lifecycle, apply procedure), the writers/
  reviewers (branch context, stable FQNs); AGENTS.md.
- Tests: coherence gate rejects a branch with unrealized planned nodes; apply rebuilds a graph whose
  delivered descriptions resolve; cross-project FQN stability through squash/rebase.