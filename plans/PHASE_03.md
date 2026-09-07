# PHASE_03 — Execution + apply model

References: **GraphModel-SPEC.md**, **PlanExecution-SPEC.md**, **PlanCompletion-SPEC.md**,
**0.10.0-PLAN.md**.
Scope: change the execution/apply lifecycle — a project is a branch change-set; `plan done` is
an implementer assertion, `plan complete` is a milestone, and the diff is applied by
merge + rebuild after a coherence gate.

## Deliverable

Execution happens entirely in a project branch (worktree + branch off `main`, created at
project start); the plan survives until apply; nothing is promoted by `plan done`/`plan
complete`; the apply act (coherence gate → agent-operated merge → rebuild of `main`'s graph) is
the single delivery moment; implementers can attach task notes.

## Work items

1. **`apg plan done` → assertion-only**: drop `Builds` promotion and code-graph verification
   (keep `apg plan undone` for reversals). Marking a task done is the implementer's assertion.
2. **`apg plan complete` → milestone-only**: drop `Implements` materialization and plan
   retirement; keep the all-tasks-done + all-feedback-resolved gate. The plan survives until
   apply.
3. **Task notes**: implementer attaches notes to tasks (`Task` is already an allowable
   `Details` target) — wire the write path + grant + procedure.
4. **Branch lifecycle**: `git worktree add -b <project>` off `main` at project start; build the
   branch's LadybugDB by scanning + ingesting the branch's committed state (code + committed
   `apg/specs/*.jsonl` + `apg/notes/*.jsonl`); tool write-throughs commit to the branch; the
   plan JSONL stays transient and branch-local (gitignored).
5. **Apply act** (the single delivery moment):
   - **Coherence gate**: every `Builds` target resolves in the branch's graph; all `Feedback`
     resolved; human gate passed (navigator summary of work/gotchas/deviations).
   - **Merge**: agent-operated `git merge <project-branch>` into `main` (new agent grant;
     push/tag remain denied).
   - **Rebuild**: fresh scan + ingest on `main`; verify delivered descriptions' `Implements`/
     `Anchors` resolve against the rebuilt graph.
6. **Dependency model**: branch off `main` always; squash-merge an in-flight dependency into the
   branch; rebase onto `main` after the dependency lands (navigator procedure; cross-project
   FQN stability is exercised here).

## Deliverables / done gate

- `cargo test` green, including:
  - assertion-only `plan done` (no promotion side effects; `undone` still works);
  - milestone-only `plan complete` (gate enforced, no `Implements`, plan file survives);
  - apply gate rejects a branch whose `Builds` target does not resolve;
  - task-note round-trip;
  - wont-fix approval-only lifecycle (a `--wont-fix` action is not terminal until the reviewer
    resolves it).

## Out of scope (later phases)

- The `future/` namespace migration (PHASE_04) — plan/spec FQNs still carry it here.
- Agent flow orchestration + dogfooding (PHASE_05).