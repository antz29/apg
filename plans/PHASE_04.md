# PHASE_04 — Docs + agents + dogfood under the finalized model

References: all six SPECs, **0.10.0-PLAN.md**.
Scope: re-align every doc, agent, and suite tool that still describes the pre-finalization model
(`Future`, `future/`, promote) to the planned-node model, and re-run the end-to-end dogfood under
it.

## Deliverable

README, AGENTS.md, the seven agents, and the suite-tool prose describe the **finalized model**:
planned Implementation nodes (plan-writer-authored, scanner-replaced), the plan as the bridge,
the two-phase spine (plan pre-build, direct `ImplementedBy` post-build), and the planned-node
**realization** apply gate. One dogfooded project runs the full loop under the new model and
lands on `main`.

## Work items

1. **Docs**: README + AGENTS.md — replace `Future`/`future/` references (pending-anchor
   phrasing, promote, `apg spec add future`) with the planned-node model; describe planned-node
   authoring, scanner-replace, and the realization gate.
2. **Agents** (all seven):
   - spec-writer: requirements anchor to real code or a proposed Solution node; **never**
     authors planned code.
   - plan-writer: authors the planned Implementation nodes + the plan; `Builds(Task → planned
     node)`.
   - plan-review + implementation-phase-reviewer: review planned nodes; a branch scan's
     scanner-replace is the realization signal.
   - codebase-navigator: branch lifecycle + apply procedure (coherence gate = every planned
     node realized).
   - spec-review + implementer: no planned-node authoring; assertion-only done + task notes.
3. **Suite tools**: `apg_spec_promote.ts` (removed with `apg spec promote`), `apg_spec_add.ts`
   (future arm), `apg_spec_anchors.ts`, `apg_plan_apply.ts`, `apg_spec_unresolved.ts`,
   `apg_spec_trace.ts` — re-worded to planned nodes (pending = planned node / proposed Solution
   anchor; promote gone).
4. **E2E dogfood**: one project through the whole flow under the finalized model: spec
   (tiers 1–3; requirements anchored to proposed Solution nodes) → plan-writer authors planned
   nodes + tasks (`Builds`) → implementers build → branch scans replace planned nodes → phase +
   final review → human gate → apply (realization gate green) → verify on `main` (no planned
   nodes; the spine resolves to real code).

## Deliverables / done gate

- `rg -i '\bfuture\b' README.md AGENTS.md .opencode/` matches only "the branch is the future"
  prose — no `Future` node, no `future/` namespace, no promote.
- E2E: the dogfooded project's planned nodes are realized on `main`; requirements delivered;
  the plan is transient and gone.
- `cargo test` green + clippy clean.

## Out of scope (later phases)

- Release (PHASE_05).
- The 1.0.0 Cosanima release (its own spec).