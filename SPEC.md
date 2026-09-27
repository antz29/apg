# apg-details-target — align the `details` write surface with the projection

> High-level change-set definition. The durable spec lives in `apg/layers/` (authored
> through `apg_node`/`apg_edge`); this file is the work brief, not the graph.

## Goal
Remove the `details` write/load asymmetry: today the write surface accepts a
`Note → Note` `details` edge, but the load projection silently **drops** it.

## Why
- **Write** accepts it: `src/layers/validate.rs` (the `details` rule, ~lines 425–430)
  accepts a note source → **any** target, including another `Note`; pinned by
  `src/layers/tests.rs:529` (`details_accepts_any_target_and_enforces_note_source`).
- **Load** excludes it: the `Details` REL TABLE (`src/load/tables.rs:1062`) omits `Note`
  as a target — comment at lines 818–819: "`Note` is deliberately NOT a `Details`
  target" — so the pair is dropped, not a binder error. Pinned by
  `src/load/tests.rs:26–55` and `tests/artifacts_e2e.rs:152`
  (`illegal_details_pair_is_projected_away_not_a_binder_error`).
- Result: an edge accepted at write time is **silently lost** at projection — data loss
  with no error. (Workaround in practice: avoid `details` edges whose target is a Note.)

## Decision (spec-first)
Pick the canonical side and reconcile the spec's `details` definition:
- **Recommended:** tighten the **write** surface to refuse `Note → Note` — matches the
  deliberate projection exclusion, and needs no DB / schema change; then reconcile the
  spec prose.
- **Alternative:** admit `Note` to the `Details` target list (schema + projection
  change), if the spec intends `details` targets to include notes.

## Scope
- `src/layers/validate.rs` (+ `src/layers/tests.rs`) and/or `src/load/tables.rs`
  (+ its tests / `tests/artifacts_e2e.rs`).
- Reconcile the durable `details` edge-kind spec (SPEC §3.3) so write and projection
  agree.

## Non-goals
- `Reviews` (`Feedback → Note`) stays exactly as-is — `Note` remains reviewable.

## Acceptance
- The asymmetry test flips to assert the chosen consistent behaviour.
- `layers` / `load` / `artifacts` e2e green; `scripts/gate.sh` green; `apg plan verify`
  green; merge.

## Open questions
- Which side is canonical (write-refuses vs projection-admits)? Decide in the spec, then
  implement.
