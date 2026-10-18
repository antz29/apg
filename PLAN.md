# schema-rework — phased implementation plan

Plain-markdown plan for implementing the target model in `SPEC.md`. This is
**code work, not an APG-authored change-set** (SPEC §1.1: the model under
change is the model that manages change-sets). Each phase keeps `cargo build`
and `cargo test` green; e2e tiers run against scratch `/tmp` repos.

Ordering rule: the **vocabulary** (types + edges) is fixed first, then the
**rules** that validate it, then the **projection** (DB schema), then the
**surfaces** (lint/save/CLI) that consume it.

---

## Phase 0 — Groundwork (done)

- Worktree `schema-rework` cut from `main`; `SPEC.md` + `EXPLORATION.md` in
  place.
- Deliverable: the target model and the change-surface map are the reference.

## Phase 1 — Vocabulary: node types + wire records + in-memory graph

Add the two new node types and the new/renamed edge spellings end-to-end at the
*data* layer, before any validation or projection depends on them.

- **`src/layers/catalog.rs`** — `Layer::node_types()`: add `actor` to Domain,
  `role` to Solution; mirror in `LAYERS_TREE`.
- **`src/schema.rs`** — add `Record::Actor`, `Record::Role`; add edge records
  `Requires`/`Reads`/`Writes`/`IsRole`/`Realises` (with the kebab `rename`
  spellings). **Additive only** — `RealisedBy`/`DependsOn` and the `Represents`
  shape are removed/re-shaped in Phase 2 (matrix) and Phase 3 (projection), not
  here.
- **`src/graph.rs`** — `NodeKind::{Actor, Role}`; `Graph` fields
  `requires`/`reads`/`writes`/`is_role`/`realises`. **Additive only** —
  `depends_on`/`realised_by` removal is Phase 2/3.
- Acceptance: `cargo build` (this phase intentionally leaves the match arms
  in `tree.rs`/`tables.rs`/`ingest/*` temporarily non-exhaustive — add them
  as stubs here or fold into Phase 3/4; keep it compiling).

### Phase 1 review — folded forward (no blocking findings)

- **Advisory (fold into Phase 2):** the five new edge arms in `src/ingest/mod.rs`
  drop their `properties` (`{ from, to, .. }`) unlike every sibling spec edge —
  capture into `graph.edge_properties` keyed by the kebab kind.
- **Advisory (fold into Phase 2):** the five new sets are missing from the
  ingest post-pass `filter_edges` matrix and the `src/cleanup.rs` retain loop —
  add both so the new edges get the same dangling-drop + endpoint hygiene as
  siblings.
- **Optional (Phase 2):** drop the redundant `#[serde(rename = "realises")]`
  (snake_case already yields it).
- **Optional (Phase 3):** extend `edge_spool_roundtrip` (tags 16–20); add the
  new sets to `testutil.rs` and `splice/seed.rs` edge lists.

## Phase 2 — Rules: edge matrix + whole-model cardinalities

Rewrite the validator to the new relationship catalogue and add the cardinality
rules that the SPEC's §4/§8 make mandatory. This is the semantic heart.

- **`src/layers/validate.rs`** —
  - Rewrite `EDGE_KINDS` + `MATRIX`: `requires` (Stakeholder/User→Requirement),
    `contains` (Requirement→Requirement, Group→Group/Service/Entity,
    Entity→Value, System→Container, Container→Component), `reads`/`writes`
    (Service→Entity, own leaf-Group only), `calls` (Actor→Service,
    Service→Service), `publishes`/`subscribes` (Service/Actor→event Entity),
    `drives` (Requirement→Service only), `realises` (Component→Service),
    `is-role` (Container/Component→Role), `uses` (Person→Component),
    `represents` (Actor→User, Person→Actor), `implemented-by` (Component→code).
    Drop `depends-on`, `realised-by`.
  - Add a **whole-model cardinality validator** (pure, over the assembled node
    set): single-owner Service/Entity/Value; leaf-Group ≥1 Service; Entity ≥1
    Value; Service driven by ≥1 Requirement; Component realises ≥1 Service;
    exactly-one Role per Container/Component; cross-Group read/write
    confinement; acyclic containment (already have `validate_trees_acyclic`).
  - Remove `PROP_ATTACHES_TO` handling.
- **`src/layers/tree.rs`** — remove the `realised-by`/`depends-on` `edge_record`
  arms (they bail now); stop emitting `Constraint.attaches_to`.
- **`src/ingest/mod.rs`** — capture the five new edges' `properties` into
  `graph.edge_properties`; add them to the post-pass `filter_edges` matrix.
- **`src/cleanup.rs`** — add the five new sets to the retain loop.
- **`src/schema.rs`** — drop the redundant `rename = "realises"`.
  (Removal of the `RealisedBy`/`DependsOn` *record variants* and their `Graph`
  fields stays in Phase 3, together with the projection, to avoid one huge
  multi-surface break.)
- Acceptance: unit/int tests in `src/layers/tests.rs` pin the matrix + every
  cardinality rule (happy + each violation); `cargo test`.

### Phase 2 review — folded forward (no blocking findings)

- **Advisory → Phase 7:** `tests/spec_lint_e2e.rs` fails for **two** reasons, not
  just the removed `attaches-to`: the delta-gate fixture runs
  `edge add implemented-by solution.system.claim …`, and `implemented-by` now
  requires a **Component** source. Phase 7's e2e rewrite must re-point that
  fixture to `solution.component.*` **and** drop the R2 assertion.
- **Optional → Phase 3 checklist:** the ingest `finalize_graph` filters still
  enforce the OLD shapes for `drives` (Req→Group/Entity/Value/Service), `uses`
  (Person→System), `represents` (User→Entity, Entity→Person),
  `spec_implemented_by` (System/Container/Component→code), `subscribes`
  (Service→Entity only) — reshape all five to the new matrix, not just the DDL.
- **Optional (trivial, Phase 3):** stale `depends-on` comment in
  `src/layers/tests.rs`; stale `#[allow(dead_code)]`/"Unused until …" on
  `validate_node`/`validate_edges` (now wired).

## Phase 3 — Projection: DB schema + load routing

Make the DB tables and the parquet/export/ingest routing agree with Phase 1–2.

- **`src/load/tables.rs`** — `label_of`/`kind_slug` add `Actor`/`Role` (done in
  Phase 1 for the compile closure); `node_labels()` adds `Actor`/`Role` **here,
  together with the DDL** (adding them before the node tables exist would make
  every durable mutation fail on a binder error); `create_schema()` adds the
  `Actor`/`Role` node tables and rewrites rel tables
  (new `Requires`/`Reads`/`Writes`/`IsRole`/`Realises`; `Contains` +Entity→Value
  −Group→Value; `Drives`→Requirement→Service; `Uses`→Person→Component;
  `Represents`→Actor→User, Person→Actor; drop `DependsOn`/`RealisedBy`;
  `SpecImplementedBy`→Component→code only; `Constraint` drops `attaches_to`).
  Update `copy_from()` parquet names/stmts to match.
- **`src/load/parquet.rs`**, **`src/load/export.rs`**, **`src/ingest/edges.rs`**,
  **`src/ingest/mod.rs`** — route the new records, drop the removed ones, keep
  `graph.rs` ↔ `tables.rs` in parity; **reshape the ingest `finalize_graph`
  filters for `drives`/`uses`/`represents`/`spec_implemented_by`/`subscribes`
  to the new matrix** (not just the five new sets); remove the `RealisedBy`/
  `DependsOn` record variants + `Graph` fields + `Constraint.attaches_to`
  column; add the new sets to `testutil.rs` + `splice/seed.rs` and extend
  `edge_spool_roundtrip` (tags 16–20); drop the stale `depends-on` comment and
  the stale `#[allow(dead_code)]` attrs on `validate_node`/`validate_edges`.
- Acceptance: `cargo test-e2e` scan round-trips a scratch repo whose durable
  tree uses the new types/edges; a Cypher query via `apg_query` shows `Actor`/
  `Role` labels and the new rel tables.

### Phase 3 review — folded forward (no blocking findings)

- **Advisory → Phase 7 (doc cleanup):** stale doc comments still name the
  removed shapes — `src/plan_cmd/verify.rs` ("`Drives` Domain `RealisedBy`
  Solution spine"), `tests/artifacts_e2e.rs` (`attaches-to` still listed as a
  projected `Constraint` typed column), `tests/project_cmd_e2e.rs` (two
  requirement bodies + a doc comment describing the old §3.3 matrix). Rewrite
  each to the new model.
- **Optional → Phase 7:** `tests/artifacts_e2e.rs::lossless_authored_nodes`
  still carries `("attaches-to", …)` as arbitrary metadata on a constraint —
  replace with a neutral key.

## Phase 4 — One shared validator + lint scope/context

Consolidate the mechanical rules into a single source both the write path and
the lint consume, and add the `--tier` surface (SPEC §9).

- **`src/spec_lint.rs`** — share the Phase-2 validator (not a reimplementation);
  add `--tier requirements|domain|solution` (single-tier local structure +
  upstream spine) and the merge-strength context (§8.2: exactly-one real
  `implemented-by` required). Drop the R2 `attaches-to` error.
- **`src/layers/validate.rs` / `mod.rs`** — expose the shared rule entry so
  `write` and `lint` call the *same* code.
- Acceptance: `src/spec_lint/tests.rs` covers tier-scoped vs all-tier scope;
  the write path and lint reject the identical cardinality violation with the
  identical message.

### Phase 4 review — folded forward (no blocking findings)

- **Advisory 1 (§9.3):** `lint()` still runs the delta coverage gates for
  all-tier runs; §9.3 excludes them from lint entirely (they are
  `plan verify`'s job). Remove the `change_set_spec_delta`/`coverage_check`
  block from `lint()` and move the e2e assertions to the plan-verify home.
- **Advisory 2 (§9.1):** the event-only `publishes`/`subscribes` target check
  (`validate_event_targets`) is not wired into lint — a write/lint divergence.
  Call it from `model_errors` and pin a byte-identical test.
- **Advisory 3 (§9.2):** scoped lint flags a dangling *downstream* `drives`
  target under `--tier requirements`; adopt the "drift" reading (endpoint
  existence is structural, not downstream completeness) and pin it with a test.
- **Optional 5:** `Context::MergeStrength` needs `#[allow(dead_code)]` (only
  constructed in tests). **Optional 4:** code-ref validity (`validate_code_refs`)
  is not wired into lint — note it for the merge-strength caller (Phase 5+).

### Phase 4 follow-up — reviewed clean (no findings)

- Resolved advisories 1–3 + optional 5. Out-of-plan but accepted: fixed a
  pre-existing destructive `tests/cache_e2e.rs` bug (`remove_dir_all(temp_dir())`
  deleting the system temp dir). Delta coverage confirmed still live behind
  `apg plan verify` (`plan_cmd::coverage_check`).

## Phase 5 — Save boundary: whole-model validation

> **Scope expanded (coordinator call, per implementer stop-and-report):** the
> save boundary correctly refuses partial models (§8.1), which breaks ~39 e2e
> fixtures that seed deliberately-partial stores. The fixture migration is now
> part of this phase (it is the necessary consequence of the boundary). Design
> decision: seed the shared whole-model-valid base `testutil::valid_model()` for
> tests exercising domain/requirements/solution semantics, and use the
> **unconstrained `solution.role`** as the "generic single node" anchor for
> tests exercising pure mutation/save mechanics (a Role has no cardinality
> minimums, so a lone Role is whole-model-valid). Phase 7 keeps only the final
> full-gate run + README/AGENTS doc updates.

- **`src/layers/write.rs`** + session save path — `apg session save` runs the
  whole-model validator over the buffered set and refuses an inconsistent model
  before the atomic write + commit (§8.1). Intermediate construction stays
  legal (validation is at save, not admission).
- Acceptance: e2e — build a model that is missing a Component's Role, `save`
  refuses; complete it, `save` succeeds. Unit — the save gate calls the shared
  validator, not a fork.

## Phase 6 — Surfaces: suite tools + agent templates

The binary already accepts `actor`/`role` and the new edge kinds via the Phase-1
catalog. What remains is the **distributed surface** (product source embedded
via `include_str!`):

- **`opencode-suite/tools/*.ts`** — update `apg_node.ts` (add `actor`/`role`,
  drop the `attaches-to` mention), `apg_edge.ts` (new kinds
  `requires`/`reads`/`writes`/`is-role`/`realises`; drop `realised-by`/
  `depends-on`), and any other tool whose description still names the old
  vocabulary (grep `realised-by`/`depends-on`/`attaches-to`/old type lists).
- **`opencode-suite/agents/*.md`** (the distributed-agent templates) — update
  the model descriptions (six layers, node types, edge kinds, spine, removed
  shapes) to the new model. Product-source template edits only; the repo's own
  `.opencode/agents/**` regeneration is a post-release follow-up.

## Phase 7 — Deferred to release (out of this change-set)

- README.md / AGENTS.md data-model + CLI docs, and `.opencode/agents/**`
  regeneration — post-release, consistent with the durable-spec migration
  (SPEC §1.1).
- Plan-model Gates/Satisfies/prereq removal (§10) — a separate change-set
  (the transient plan model is §1.3 out of scope here).
- Final gate: `scripts/gate.sh --e2e` is already green end-of-phase-5; re-run at
  the very end after any last edits.

## Explicitly out of scope (SPEC §1.1 / §10)
- Migrating the apg repo's own `apg/layers/**` — a separate post-release
  reconciliation.
- Any import/migration/compat feature.
- Code-graph (scanner) labels and FQN rendering are untouched.

## Known follow-ups (flagged, not in this change-set's core phases)

- **Merge-strength realness half is unpinned (advisory, follow-up test).** The
  merge-act `validate_code_refs(&refs, &scanned, &BTreeSet::new())` decision —
  a planned-only `implemented-by` target is drift at merge — has no dedicated
  e2e (the new e2e covers only the presence path). Worth a follow-up e2e that
  targets a `PlannedNode` FQN and asserts merge refuses with `spec drift`.
  (Cosmetic: the realness `bail!` reports one Drift, not all; and a planned-only
  target reads "gone from the scanned graph", which slightly overstates.)

- **Feedback reviewability of the new node types.** The `Reviews` rel table's
  target list does not yet include `Actor`/`Role`, so a reviewer cannot attach
  `Feedback` to a new `Actor`/`Role` node (binder error). `Details` must NOT
  gain `Actor`/`Role` (notes are implementation-only, SPEC §1.3/§7). Feedback/
  Notes are §1.3 out-of-scope for this change-set, but adding `Actor`/`Role`
  to the `Reviews` target list (only) is a small, correct follow-up so the new
  nodes are reviewable — extend `spec_rel_pairs`/`DURABLE_EDGE_TABLES`.
