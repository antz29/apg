# PHASE_01 — Graph model foundation

References: **GraphModel-SPEC.md**, **0.10.0-PLAN.md**.
Scope: extend the graph to represent all four tiers of active knowledge and the spine.

## Deliverable

The graph can represent the full 4-tier taxonomy — Requirements, Domain (DDD), Solution (C4),
Implementation (code) — with the spine edges linking them, and `apg query` works against the
new labels. Everything else in the plan builds on this substrate.

## Work items

1. **New node kinds** in schema.rs + labels/load/merge:
   - Tier 1: `Stakeholder`
   - Tier 2 (DDD): `Domain`/`BoundedContext`, `Subdomain`, `Entity`, `ValueObject`,
     `Aggregate`, `DomainEvent`, `DomainProcess`, `DomainRule`, `Actor`
   - Tier 3 (C4): `System`, `Container`, `Component`
   - Tier 4: Module/File/Struct/Function/UnresolvedTarget, gaining a `planned` status (see
     work item 3); DB/infra out of scope.
2. **Spine edges**: `Drives`/`Requires` (Requirement → Domain),
   `Realises`/`Represents` (Domain → Solution), `ImplementedBy` (Solution → Implementation).
   Schema + edges + merge; keep direct `Implements` (code → requirement) as the terminal link
   in the chain.
3. **`planned` status on Implementation nodes**: schema + load/merge support for a
   `status: planned` marker on `Module`/`File`/`Struct`/`Function` (the scanner emits unmarked,
   present nodes only). A scan that finds real code at a planned FQN **replaces** the planned
   node and re-points its incident edges (`Anchors`, `ImplementedBy`, `Builds`, `Details`,
   `Contains`). Planned nodes are authored by the plan-writer at plan time (PHASE_03), never by
   the spec tools.
4. **Serialization**: record variants, label registry, rel-tables, JSONL round-trip for every
   new node/edge kind; `labels(n)` returns the new labels.

## Deliverables / done gate

- `cargo test` green, including:
  - schema parse for every new node/edge kind;
  - JSONL → DB → JSONL round-trip for each;
  - a sample 4-tier spine (Requirement → Domain → Solution → Implementation) resolving via
    `apg_query`;
  - a scanned node at a planned FQN replaces the planned node (edges re-pointed; status
    unmarked).
- `apg_find_symbol`/`apg_query` surface the new labels with no regressions on existing labels.

## Out of scope (later phases)

- The invariant mechanism (PHASE_02).
- The branch-as-change-set execution model (PHASE_03).
- The `future/` namespace removal (PHASE_04).