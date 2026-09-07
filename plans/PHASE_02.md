# PHASE_02 — Planned Implementation nodes (Future reconciliation)

References: **GraphModel-SPEC.md** (planned nodes, two-phase spine, `Builds`), **PlanCreation-SPEC.md**,
**PlanExecution-SPEC.md**, **PlanCompletion-SPEC.md**, **0.10.0-PLAN.md**.
Scope: replace the `Future` machinery with **planned Implementation nodes** — the one structural
change between the shipped baseline (PHASE_01) and the finalized spec family.

## Deliverable

`Future` is gone. Planned code is represented as `Module`/`File`/`Struct`/`Function` nodes
carrying `status: planned`, **authored by the plan-writer at plan time** at the FQN where the
code will land. The scanner emits unmarked (present) nodes only; a scan that finds real code at
a planned FQN **supersedes** the planned node and re-points its incident edges (`Anchors`,
`ImplementedBy`, `Builds`, `Details`, `Contains`). Planned nodes never survive on `main`. The
apply gate verifies every planned node in the branch was **realized**.

## Work items

1. **Schema / load / merge / ingest**:
   - `status` property on the four Implementation node kinds; the scanner never emits
     `planned`.
   - **Scanner-replace** reconciliation in load/merge/ingest: a scanned node at a planned FQN
     supersedes the planned one (status cleared) and re-points its incident edges.
   - **Remove the `Future` node kind** (and its tier-aligned `kind`) from schema/load/merge/
     ingest/export — a `Future` record is rejected, not silently migrated.
   - **`Builds`** rel-table: Task → planned Implementation node (was Task → Future).
   - **`Anchors`**: Requirement → proposed Solution node (pending) or real code (resolved);
     drop the Requirement → Future pair.
2. **CLI / tools**:
   - **Drop `apg spec add future` and `apg spec promote`** (promotion is the scanner-replace;
     no `Future` remains). Pending-anchor detection in the tools (currently Future-node
     membership) becomes planned-node membership.
   - **Plan-side planned-node authoring**: the plan tools create a planned Implementation node
     at its intended FQN and wire `Builds(Task → planned node)`; the spec tools never create
     planned nodes. (Exact command shape — `apg plan add planned <fqn> --kind …` vs
     `--builds <fqn>` materialising the node — confirm at kickoff.)
   - **Requirement-delivery record**: confirm at kickoff how `Implements` is materialized when
     the spine resolves (scanner-replace adds code→req on realization, vs a single apply-time
     pass). `Implements` stays the terminal spine link per GraphModel-SPEC.
   - **`apg plan apply` coherence gate**: every `planned` Implementation node in the branch is
     realized against the code graph — a `status: planned` node with no real code at its FQN
     blocks apply. This replaces the `db.has_node`-any-label Builds check. All `Feedback`
     resolved (kept).
3. **Data migration**: convert `Future` records in `apg/specs/*.jsonl` (cosanima family,
   `apg-0.9.3`, `invariant-mechanism`) to planned Implementation nodes or retire them; re-point
   incident edges; sweep the `future/` literal residue from `apg-0.9.3.jsonl` requirement/AC/VI/
   note bodies while touching those files.
4. **Tests**: planned-node round-trip; scanner-replace (edges re-pointed, status cleared);
   apply gate rejects an unrealized planned node / passes when all realized; plan-writer
   planned-node authoring; no-`Future` regression (a `Future` record is rejected).

## Deliverables / done gate

- `cargo test` green + clippy clean.
- `rg -n '\bFuture\b' src/ .opencode/ apg/specs/ README.md AGENTS.md` is empty (the `Future`
  node and its machinery are gone; "the branch is the future" prose is lowercase-only).
- A dogfood trace end to end: a planned `Struct` authored by the plan-writer → implemented →
  a branch scan replaces it → the apply gate passes with the requirement's why-to-code chain
  resolving via the spine.

## Out of scope (later phases)

- Closing the REVIEW.md open items (PHASE_03).
- Docs/agents/dogfood re-run under the finalized model (PHASE_04).
- Release (PHASE_05).