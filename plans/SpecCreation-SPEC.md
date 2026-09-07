# SpecCreation-SPEC.md — graph-native spec capture + writing flow

Status: working spec (authoring in progress; materialized into the graph as a spec project later).
Parent capability: **GraphModel-SPEC.md** (4-tier graph; a project = a branch change-set).
Invariants: **Invariants-SPEC.md** (graph-wide invariant mechanism, used by this flow).

## Scope

The end-to-end process by which an idea or prose becomes an **approved, review-clean spec
graph** — the project's proposed reality across **tiers 1–3** (Requirements, Domain, Solution).
The spec-writer works across tiers 1–3 — the spec declares the proposed Solution shape but
never concrete planned code; the plan-writer (PlanCreation-SPEC.md) separately plans the tier-4
delta and authors its `planned` Implementation nodes. Spec authoring happens **in the project's worktree + branch** (created at
project start per GraphModel-SPEC.md); the branch graph is built by scanning + ingesting the
branch's state, and tool write-throughs serialize into the branch's JSONLs (the commit to the
branch is agent-operated — the navigator/implementer commits at phase boundaries; the CLI never
auto-commits). This spec covers **capturing +
writing specs only** — plan creation, execution, and code review are out of scope (separate
specs in the family).

## Goal

A user's intent lands in the project's branch graph as a spec whose **structure is valid**
(CLI-enforced), whose **content matches intent** (user-approved), and whose **quality is
review-clean** (reviewer-loop-enforced) — with recurring rules becoming explicit, linkable
`Invariant` nodes over time (see Invariants-SPEC.md).

## 1. The capture + writing flow

0. **Branch context** — the project's worktree + branch off `main` already exists (created at
   project start, GraphModel-SPEC.md). Its graph is built by scanning + ingesting the branch's
   committed state; all spec authoring below writes into this branch and commits with it.
1. **User** → **codebase-navigator**: "spec for X" (or a prose SPEC.md).
2. Navigator delegates to **spec-writer**:
   - interview the user one question at a time (purpose, scope/non-goals, interfaces, error
     handling, constraints, acceptance criteria);
   - propose 2–3 approaches with trade-offs; the user picks;
   - present the full design (goal, requirements grouped by feature, domain concepts, phases
     with gates, decisions, non-goals, ACs, VIs, open questions) and wait for
     **user approval**;
   - author via `apg spec init <project> --title --goal` then `apg spec add` / `anchor` /
     `link` — producing the tiers 1–3 proposed reality (Requirements, Domain, Solution nodes);
   - self-lint (`apg spec unresolved`).
3. **Review loop** (navigator-orchestrated, cycled until done):
   - navigator → **spec-review**: reviews the spec graph against the branch's code graph and
     the in-scope invariants; attaches `Feedback` nodes (open), linking `Checks` → Invariant
     where a comment enforces a known rule;
   - navigator → **spec-writer**: actions each Feedback (`--fix`/`--wont-fix`) and edits the
     spec through the **normal authoring path**;
   - navigator → **spec-review** again: **resolves** actioned feedback if the fix is correct,
     **rejects** (reopens) if not;
   - repeat until every `Feedback` on the spec is **resolved**.
4. **Approval**: all feedback resolved → spec approved → navigator hands off to plan-writer
   (PlanCreation-SPEC.md).

**Role separation (strict):** spec-writer authors + actions, never resolves; spec-review
attaches/resolves/rejects, never authors; the navigator routes and decides when resolution has
terminated the loop. **The spec-writer never authors tier-4 nodes** — planned Implementation
nodes (the concrete code shape) are created by the plan-writer at plan time
(PlanCreation-SPEC.md); requirements anchor to real code or a proposed Solution node.

**Namespacing:** there is no `future/` prefix — the branch is the "future". Spec FQNs are
project-scoped (`<project>/spec.<id>`), stable, and present-ness is branch membership
(GraphModel-SPEC.md).

## 2. Correctness layers (three; no invariant dependency)

- **CLI envelope validation** at write time: valid node kinds, note-kind vs target-category
  rules, resolvable anchors (a real code FQN or a proposed Solution node — never invented),
  acyclic DependsOn/Gates, orphan/coverage lint.
- **User design approval** before authoring (the semantic gate: content matches intent).
- **Reviewer loop** as the semantic backstop (internal consistency, ambiguity, coverage,
  clarity). Most feedback here is *not* an invariant violation.

## 3. Invariant usage in spec capture

The invariant mechanism (Invariants-SPEC.md) is exercised on spec capture as follows:

- **Writer awareness**: the navigator injects the active invariant set into the spec-writer
  prompt; the writer also queries `apg invariants` — so known rules hold at authoring.
- **Reviewer checking**: spec-review checks the spec against its `GuardedBy` invariants
  (queryable via `MATCH (s)-[:GuardedBy]->(i:Invariant)`); where a comment enforces a known
  rule it links `Checks` → Invariant.
- **Emergent addition**: as feedback patterns recur, the navigator proposes a new invariant to
  the user; on confirmation it is materialized via `apg invariant add` and enters the set
  writers/reviewers see.
- **No precondition**: the flow works identically with zero invariants.

## 4. Out of scope (other specs in this family)

- Plan creation from an approved spec (PlanCreation-SPEC.md).
- Plan execution (PlanExecution-SPEC.md) and the plan-end human gate.
- Code implementation + implementation-phase review (PlanExecution-SPEC.md).
- Applying the diff / the present state (PlanCompletion-SPEC.md).
- The invariant mechanism generalizing across tiers (GuardedBy already supports it; each
  domain defines its own spec + invariants).

## 5. Implementation surface (when approved)

- `Invariant` node + `Checks`/`GuardedBy` edges in schema/load/merge (label, rel-table,
  round-trip).
- `apg invariant add` subcommand (src/invariant_cmd.rs + main.rs dispatch/help) + suite tool
  `apg_invariant_add.ts`; `apg invariants` read tool + suite tool `apg_invariants.ts`
  (SUITE_TOOLS embed + lib).
- The `apg spec` tools already run in the project worktree; add the branch-context setup
  (worktree + branch + scan + ingest) to the navigator procedure.
- Agent-prose updates: spec-writer, spec-review, codebase-navigator (invariant awareness +
  Checks/GuardedBy citation, branch context); AGENTS.md.
- Tests: schema parse, add/list/guard round-trip, Checks/GuardedBy edges, tool smoke.