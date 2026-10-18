# schema-rework — what needs to happen

Scope of product changes to implement the target model in `SPEC.md` (the
requirements/domain/solution schema rework). This is **plain code work**, not an
APG-authored change-set: the model that manages change-sets is the very thing
being changed, so we implement it directly in the product source, test it
against scratch repos, and leave the durable-spec migration to a post-release
reconciliation (SPEC §1.1 / §10).

## The delta, in one table

| Concern | Current (`main`) | Target (`SPEC.md`) |
|---|---|---|
| Domain node types | `group, entity, value, service, constraint` | **+ `actor`** |
| Solution node types | `system, container, component, person, constraint` | **+ `role`** |
| Requirement ownership | Stakeholder/User `contains` Requirement | **`requires`** (new kind) |
| Requirement decomposition | `depends-on` | **`contains`** (Requirement→Requirement) |
| Value ownership | Group `contains` Value | **Entity `contains` Value** |
| Identity refinement | User `represents` Entity, Entity `represents` Person | **Actor `represents` User, Person `represents` Actor** |
| Domain→Solution | Domain `realised-by` Solution | **Component `realises` Service** (new kind, reversed) |
| Person usage | Person `uses` System | **Person `uses` Component** |
| Service entity access | (none) | **`reads` / `writes`** (new kinds) |
| Architectural role | (none) | **`is-role`** Container/Component → Role (new kind) |
| `drives` target | Group/Entity/Value/Service | **Service only** |
| `implemented-by` source | System/Container/Component | **Component only** |
| Constraint scope | layer + optional `attaches-to` | **layer only** (no attachment) |
| Plan model | Gates/Satisfies/prereqs | **removed** (transient, out of tier 1–3 spec) |

## Where each change lands (product source)

### 1. Catalog — `src/layers/catalog.rs`
- `Layer::node_types()`: add `"actor"` to `Layer::Domain`, `"role"` to
  `Layer::Solution`.
- `LAYERS_TREE`: same additions.

### 2. Wire schema — `src/schema.rs`
- New `Record` variants: `Actor`, `Role` (node records).
- New edge records: `Requires` (`requires`), `Reads` (`reads`), `Writes`
  (`writes`), `IsRole` (`is-role`), `Realises` (`realises`).
- Repurpose/remove: `Represents` changes endpoint types (Actor→User,
  Person→Actor); `RealisedBy` is removed; `DependsOn` is removed;
  `SpecImplementedBy` narrows to Component source.

### 3. In-memory graph — `src/graph.rs`
- `NodeKind`: add `Actor`, `Role`.
- `Graph`: add edge sets (`requires`, `reads`, `writes`, `is_role`, `realises`);
  drop `depends_on`, `realised_by`; keep `spec_implemented_by` (narrowed).

### 4. Write-time validation — `src/layers/validate.rs`
- Rewrite `EDGE_KINDS` + `MATRIX` to the new relationship catalogue (§4).
- New whole-model cardinality rules (§4.2–§4.4, §8.1): leaf-Group needs ≥1
  Service; Service/Entity/Value single-owner; Entity needs ≥1 Value; every
  Service driven by ≥1 Requirement; cross-Group reads/writes refused; Role
  count on Container/Component is exactly 1; etc.
- Drop `PROP_ATTACHES_TO` / `attaches-to` (constraint scope is its layer).

### 5. Tree ingestion — `src/layers/tree.rs`
- `node_record`: add `actor` / `role` arms.
- `edge_record`: add `requires` / `reads` / `writes` / `is-role` / `realises`;
  remove `realised-by` / `depends-on`; re-map `represents`.

### 6. Code refs — `src/layers/code_refs.rs`
- Mostly unchanged; `implemented-by` still a code-FQN target, now only legal
  from Component (enforced by the matrix in `validate.rs`).

### 7. DB schema — `src/load/tables.rs` (the big one)
- `node_labels()` / `label_of()`: add `Actor`, `Role`.
- `create_schema()`: new `Actor`/`Role` node tables; rewrite rel tables —
  `Contains` (+ Entity→Value, − DomainGroup→Value), new `Requires`/`Reads`/
  `Writes`/`IsRole`/`Realises`, `Drives` narrowed to Requirement→Service,
  `Uses` narrowed to Person→Component, `Represents` re-pointed, drop
  `DependsOn`/`RealisedBy`, `SpecImplementedBy` narrowed to Component→code.
- `copy_from()` parquet file names/stmts follow.

### 8. Load/ingest plumbing — `src/load/parquet.rs`, `src/load/export.rs`,
   `src/ingest/edges.rs`, `src/ingest/mod.rs`
- Route new node/edge records; drop removed ones; parity with `graph.rs` +
  `tables.rs`.

### 9. Lint — `src/spec_lint.rs`
- §9.4: `--tier requirements|domain|solution` flag (scope vs context).
- §9.1: one shared validator set across lint/save/merge (today `validate.rs`
  is the write path and `spec_lint.rs` is the read path — they must share the
  new cardinality rules rather than diverge).
- §8.2: merge-strength context requires exactly one real `implemented-by`.
- Drop the R2 `attaches-to` check (constraints no longer attach).

### 10. Save boundary — `src/layers/write.rs` / session
- §8.1: `save` validates the **whole buffered model** (all minimum
  cardinalities), not just the per-mutation structural checks.

### 11. CLI + suite — `src/node_cmd.rs`, `src/plan_cmd.rs`, `opencode-suite/`
- `apg node add` accepts the new types; `apg edge` the new kinds.
- `apg_spec_lint` tier input (the tool already advertises it per SPEC §9.4).

### 12. Tests
- `src/layers/tests.rs`, `src/schema/tests.rs`, `src/spec_lint/tests.rs`,
  `tests/spec_lint_e2e.rs`, `tests/specs_e2e.rs` — new matrix/cardinality
  coverage on scratch repos, never the apg repo.

## Explicitly out of scope (per SPEC §1.1 / §10)
- Migrating the apg repo's own `apg/layers/**` durable spec — a separate
  post-release reconciliation.
- Any compatibility layer / import / migration feature.
- The plan model's transient Gates/Satisfies removal (that is the transient
  model, outside the tier-1–3 spec — but §10 lists it; flag for a follow-up).
- Code-graph labels (`Module`/`File`/`Struct`/`Function`/`UnresolvedTarget`)
  and scanner FQN rendering are untouched.
