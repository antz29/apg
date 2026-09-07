# PHASE_02 — Invariant mechanism

References: **Invariants-SPEC.md**, **0.10.0-PLAN.md**.
Scope: implement the graph-wide invariant mechanism — the discipline layer that makes recurring
rules explicit, discoverable, and linkable across every tier.

## Deliverable

Invariants are first-class graph nodes: materializable via `apg invariant add`, guardable onto
artifacts (`GuardedBy`), citable from review feedback (`Checks`), listable via `apg invariants`
+ suite tools, and visible to writers/reviewers through prompt injection. The flow works
identically with zero invariants (they are emergent, never a precondition).

## Work items

1. **`Invariant` node** in schema/load/merge: `fqn` (`invariant/<name>` universal,
   `<project>/invariant/<name>` project-scoped), `title`, `body`, `category`
   (process | product | graph-integrity), `scope`, `status` (active | retired).
2. **`GuardedBy` edge** (artifact → Invariant) — any artifact (Spec, Plan, Requirement, Task,
   code node, or a domain node) is guarded by the invariants it must respect.
3. **`Checks` edge** (Feedback → Invariant) — optional; a review comment cites the rule it
   enforces.
4. **`apg invariant add [<project>] <name> --title … --body … --category … --scope …
   [--guard <fqn>]*`** subcommand + main.rs dispatch/help — materializes the node, optionally
   linking `GuardedBy`.
5. **`apg invariants`** read command — list invariants, filter by scope/project.
6. **Suite tools**: `apg_invariant_add.ts`, `apg_invariants.ts` (SUITE_TOOLS embed + lib
   `runCli`).
7. **`DomainRule` alignment**: `DomainRule` (PHASE_01 tier-2 node) is the invariant mechanism
   at the domain tier — enforce its representation as an `Invariant` with
   `category = product` (confirm wiring with PHASE_01's `DomainRule`).

## Deliverables / done gate

- `cargo test` green, including:
  - schema parse + JSONL round-trip for `Invariant`/`GuardedBy`/`Checks`;
  - `apg invariant add` → `apg invariants` round-trip with `--guard`;
  - `Checks` (Feedback → Invariant) round-trip;
  - tool smoke for the two suite tools.

## Out of scope (later phases)

- Agent awareness/injection wiring (PHASE_05) — the CLI + tools land here; the agents use them
  in PHASE_05.
- `apg invariant rm`/`link` lifecycle commands — status flip (retired) is enough for now.