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

1. **`apg plan done` → assertion-only**: drop promotion and code-graph verification (keep
   `apg plan undone` for reversals). Marking a task done is the implementer's assertion; the
   scanner replacing a planned node is the promotion.
2. **`apg plan complete` → milestone-only**: drop `Implements` materialization and plan
   retirement; keep the all-tasks-done + all-feedback-resolved gate. The plan survives until
   apply.
3. **Task notes**: implementer attaches notes to tasks (`Task` is already an allowable
   `Details` target) — wire the write path + grant + procedure.
4. **Branch lifecycle**: `git worktree add -b <project>` off `main` at project start; build the
   branch's LadybugDB by scanning + ingesting the branch's committed state (code + committed
   `apg/specs/*.jsonl` + `apg/notes/*.jsonl`); tool write-throughs serialize into the branch's
   JSONLs (the commit to the branch is agent-operated — implementer/navigator at phase
   boundaries); the plan JSONL stays transient and branch-local (gitignored). Each branch scan
   **replaces realized planned nodes** (a scanned node at a planned FQN supersedes the planned
   node; see PHASE_01).
5. **Apply act** (the single delivery moment):
   - **Coherence gate**: every `planned` Implementation node in the branch is realized (a scan
     found real code at its FQN); all `Feedback` resolved; human gate passed (navigator summary
     of work/gotchas/deviations).
   - **Merge**: agent-operated `git merge <project-branch>` into `main` (new agent grant;
     push/tag remain denied).
   - **Rebuild**: fresh scan + ingest on `main`; verify delivered descriptions' `Implements`/
     `Anchors` resolve against the rebuilt graph.
6. **Dependency model**: branch off `main` always; squash-merge an in-flight dependency into the
   branch; rebase onto `main` after the dependency lands (navigator procedure; cross-project
   FQN stability is exercised here).
7. **Planned-node authoring (plan-writer)**: the plan tools can author `planned` Implementation
   nodes (a planned `Struct`/`Function`/… at its intended FQN) and `Builds(Task → planned node)`
   edges; the spec tools never create planned nodes (requirements anchor to real code or a
   proposed Solution node).

## Deliverables / done gate

- `cargo test` green, including:
  - assertion-only `plan done` (no promotion side effects; `undone` still works);
  - milestone-only `plan complete` (gate enforced, no `Implements`, plan file survives);
  - apply gate rejects a branch with an unrealized planned node;
  - task-note round-trip;
  - plan-writer authors a planned node + `Builds` edge; a branch scan replaces it;
  - wont-fix approval-only lifecycle (a `--wont-fix` action is not terminal until the reviewer
    resolves it).

## Out of scope (later phases)

- The `future/` namespace migration (PHASE_04) — plan/spec FQNs still carry it here.
- Agent flow orchestration + dogfooding (PHASE_05).