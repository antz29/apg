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
   - Tier 4 unchanged (Module/File/Struct/Function/UnresolvedTarget — code only; DB/infra out
     of scope).
2. **Spine edges**: `Drives`/`Requires` (Requirement → Domain),
   `Realises`/`Represents` (Domain → Solution), `ImplementedBy` (Solution → Implementation).
   Schema + edges + merge; keep direct `Implements` (code → requirement) as the terminal link
   in the chain.
3. **`Future.kind` re-alignment** *(confirm at kickoff)*: re-map the proposed-code vocabulary
   (function/struct/service/rpc/endpoint/other) onto the tier kinds (`Container`/`Component`/
   `Struct`/…) so a diff's additions declare which tier node they will become.
4. **Serialization**: record variants, label registry, rel-tables, JSONL round-trip for every
   new node/edge kind; `labels(n)` returns the new labels.

## Deliverables / done gate

- `cargo test` green, including:
  - schema parse for every new node/edge kind;
  - JSONL → DB → JSONL round-trip for each;
  - a sample 4-tier spine (Requirement → Domain → Solution → Implementation) resolving via
    `apg_query`.
- `apg_find_symbol`/`apg_query` surface the new labels with no regressions on existing labels.

## Out of scope (later phases)

- The invariant mechanism (PHASE_02).
- The branch-as-change-set execution model (PHASE_03).
- The `future/` namespace removal (PHASE_04) — this phase adds to the old namespace.