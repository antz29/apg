# GraphModel-SPEC.md — the 4-tier active-knowledge graph

Status: working spec (the anchor for the spec-family; materialized into the graph as a spec
project later).
Parent capability: **Invariants-SPEC.md** (graph-wide invariant mechanism, applies per tier).
Descendants: **SpecCreation-SPEC.md**, **PlanCreation-SPEC.md**, **PlanExecution-SPEC.md**,
**PlanCompletion-SPEC.md**.

## The model in one paragraph

The present graph is **4-tiered active knowledge** — not just the codebase, but the collective
understanding about it. A **project is a proposed diff to the graph** (a change-set), and that
change-set is literally a **git branch**: a worktree + branch off `main`, created at project
start, in which the proposed reality (tiers 1–3) is authored, the tier-4 delta is planned and
built, reviewed, and finally **merged into `main`** — at which point the diff is applied and the
present graph (main) is the new current reality. There is no `future/` namespace (the branch is
the "future") and no archive (the graph always represents current reality; git holds history).

## The four tiers

| Tier | Knowledge | What it is | Role that authors it |
|---|---|---|---|
| 1 | **Requirements** | Why the system exists; needs / obligations it must satisfy | spec-writer |
| 2 | **Domain** | What business reality exists: concepts, processes, rules, events, contexts | spec-writer |
| 3 | **Solution** | How we have chosen to solve the problem: architecture and design | spec-writer |
| 4 | **Implementation** | The codebase (database + infrastructure out of scope for now) | scanned (present); plan-writer authors `planned` nodes |

The spec-writer works **across tiers 1–3** — the spec *is* the proposed reality. The
plan-writer operates separately: the plan is the **delta from current reality to proposed
reality** — what to create/modify in tier 4 to make the proposed reality actual. The plan-writer
declares the tier-4 additions as **planned Implementation nodes** (marked `planned`) and plans
the work that makes them real.

## Node taxonomy

### Tier 1 — Requirements (Why)
| Node | Status | Notes |
|---|---|---|
| `Stakeholder` | new | who holds the need (actor/persona) |
| `Requirement` | existing | the obligations |
| `NonGoal` | existing | explicit out-of-scope |
| `AcceptanceCriterion` | existing | objective pass/fail |
| `VerificationItem` | existing | how it is verified |

### Tier 2 — Domain (What) — DDD-aligned
| Node | Status | Notes |
|---|---|---|
| `Domain` / `BoundedContext` | new | the domain area (e.g. the Auth domain) |
| `Subdomain` | new | core / supporting / generic partitioning |
| `Entity` | new | identity-bearing concept (a `User`) |
| `ValueObject` | new | value-based concept |
| `Aggregate` | new | consistency boundary with a root |
| `DomainEvent` | new | occurred facts ("user logged in") |
| `DomainProcess` | new | workflows / state transitions |
| `DomainRule` | new | business rule — **the Invariant mechanism at the domain tier** |
| `Actor` | new | party / role in the domain |

**Granularity is the specification mechanism.** A detailed spec is expressed as many small,
precise, linked nodes — not a few bulky prose nodes. The fuller DDD set above is what keeps a
detailed specification structured rather than prose-heavy.

### Tier 3 — Solution (How) — C4-aligned
| Node | Status | Notes |
|---|---|---|
| `System` | new | the solution's system-context root |
| `Container` | new | deployable unit: app / service / db / queue |
| `Component` | new | logical building block within a container |
| `Decision` | existing | design decisions |

### Tier 4 — Implementation (Code)
| Node | Status | Notes |
|---|---|---|
| `Module` | existing | package / module / namespace |
| `File` | existing | |
| `Struct` | existing | class / struct / interface / type |
| `Function` | existing | |
| `UnresolvedTarget` | existing | |

**Planned nodes.** Any Implementation-tier node (`Module`/`File`/`Struct`/`Function`) can carry
`status: planned` — a proposed tier-4 addition authored by the **plan-writer**
(PlanCreation-SPEC.md) at the FQN where the code will land. The scanner emits unmarked (present)
nodes only; a scan that finds real code at a planned FQN **replaces** the planned node and
re-points its incident edges (`Anchors`, `ImplementedBy`, `Builds`, `Details`, `Contains`).
Planned nodes never survive on `main`.

Database (`Table`/`Column`) and infrastructure (`Infrastructure`) nodes are **out of scope for
now** — noted only for completeness.

## Edges

**Hierarchy** (`Contains`): `Domain ⊃ Subdomain ⊃ Aggregate ⊃ Entity/ValueObject`,
`Domain ⊃ DomainEvent/DomainProcess/DomainRule/Actor`, `System ⊃ Container ⊃ Component`,
`Module ⊃ File ⊃ Struct/Function` (existing).

**The spine** (end-to-end traceability, why-to-code):
```
Requirement --Drives/Requires--> Domain --Realises/Represents--> Solution --ImplementedBy--> Implementation
```
The spine resolves in two phases. **Before the code is built** (the branch's proposed reality)
there is no direct Solution→Implementation edge: **the plan is the bridge** — a phase
`Satisfies` the requirement that `Drives` the Solution, and a task `Builds` the planned
Implementation node. **After the code is built** (apply, `main`), the direct `ImplementedBy`
edge takes over and the plan is gone: a Solution node's `ImplementedBy` target is a planned node
(pending) until a scan replaces it with the real node (resolved). The direct `Implements`
(code → requirement) edge remains as the terminal link in the chain; the full model threads the
Domain and Solution tiers through the middle so any requirement traces down to the code that
implements it and any code traces up to the why.

**Other existing edges** carry over unchanged: `Anchors`, `Details`, `DependsOn`, `Gates`,
`Contains`, `Reviews`, `Satisfies`, plus `GuardedBy`/`Checks` (Invariants-SPEC.md).
**`Builds`** (Task → planned Implementation node) is the plan's side of the bridge; it resolves
when a scan replaces the planned node.

## Versioning — the change-set is a git branch

- **Present reality = `main`'s graph.** Built by scanning + ingesting `main`'s committed state
  (code + spec/note JSONLs) into the per-checkout LadybugDB.
- **A project = a change-set = a worktree + branch off `main`, created at project start** (not
  at implementation). All authoring, planning, implementation, and review happen against the
  branch's graph: a fresh per-worktree LadybugDB built by scanning + ingesting the branch's
  state. Tool write-throughs serialize into the branch's JSONLs; **the commit to the branch is
  agent-operated** — the implementer commits code, the navigator commits JSONL changes at phase
  boundaries and operates the apply merge (the CLI never auto-commits: its staleness gate
  refuses a mutation against a tree that moved on, so a committed branch is re-scanned before
  further authoring).
- **No `future/` namespace.** The branch *is* the "future". A node's present-ness is a branch
  property, not an FQN property: nodes only on a branch = proposed; nodes on `main` = present.
  FQNs are stable and project-scoped; the merge determines reality.
- **Durability**: code + `apg/specs/*.jsonl` + `apg/notes/*.jsonl` are committed and merge;
  `db.lbug`, `graph.jsonl`, and plan JSONLs are derived/transient and stay gitignored.
- **Apply-the-diff = `git merge` branch → `main` + rebuild `main`'s graph.** The coherence gate
  (every planned node realized, all feedback resolved, human gate) runs before the merge; the merge
  itself is agent-operated as part of the apply act; **push/tag remain human**.
- **No archive.** The graph always represents current reality — delivered spec JSONLs stay on
  `main` as accumulated understanding; nothing in the present is dead. **Git holds history**:
  check out a commit/branch and rebuild the graph to see any past reality. An abandoned project
  is a branch that never merges.

## Dependencies between projects

- **Always branch off `main`.**
- To depend on an in-flight project A: `git merge --squash A` into B's branch — B picks up A's
  code and spec JSONLs as one squash commit, so B's graph includes A's proposed nodes and B can
  spec/plan against them by stable FQN.
- When A merges to `main`, **rebase B onto `main`**: A's real history is now in `main`, B's
  squashed copy reconciles away, and B's diff against `main` is only B's own change-set. (If A
  evolved after B squashed it, the rebase picks up A's final state and may need conflict
  resolution.)

## Role ownership per tier

| Tier | Owned by | Artifact |
|---|---|---|
| 1–3 (proposed reality) | spec-writer | the spec graph |
| tier-4 delta (proposed → current) | plan-writer | the plan graph + `planned` Implementation nodes |
| tier-4 (build) | implementers | code in the worktree |
| all (review) | spec-review / plan-review / implementation-phase-reviewer | feedback |
| apply-the-diff | navigator (agent-operated), human gate | merge + rebuild |

## Invariants per tier

The Invariants-SPEC.md mechanism applies graph-wide: `DomainRule` is the Invariant mechanism at
the domain tier; process/product/graph-integrity invariants guard artifacts in any tier via
`GuardedBy`; reviewers cite them via `Checks`; new invariants emerge from feedback patterns
(navigator-proposed, user-confirmed).

## Out of scope

- Database and infrastructure nodes (`Table`/`Column`/`Infrastructure`) — future.
- The mechanics of the existing scanner output (Module/File/Struct/Function) are unchanged;
  the new Domain/Solution node types are authored via the spec tools, not scanned.