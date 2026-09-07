# PHASE_04 — Namespace migration + lifecycle cleanup

References: **GraphModel-SPEC.md**, **PlanCompletion-SPEC.md**, **0.10.0-PLAN.md**.
Scope: remove the `future/` namespace (the branch is the "future"), remove the archive
machinery, and retire the `apg plan retag` band-aid. Touches everything — done late, once.

## Deliverable

No `future/<project>/` FQN prefix anywhere: spec/plan/review FQNs are project-scoped and stable
(`<project>/spec.<id>`, `<project>/plan.phase-01…`), present-ness is branch membership (nodes
only on a branch = proposed; nodes on `main` = present). `apg spec archive` is gone (the graph
always represents current reality; git holds history). `apg plan retag` is gone.

## Work items

1. **FQN convention change**: drop `future/<project>/` → project-scoped stable FQNs across
   schema docs, `specs.rs`/`spec_cmd.rs`/`plan_cmd.rs`/`review_cmd.rs`/`artifacts.rs`
   (paths, prefixes, `short_id`, requirement/dependency resolution), suite tools
   (`apg_plan_*`, `apg_spec_*`, `apg_review_*`), lib `apg.ts`, and query patterns.
2. **Migrate existing specs**: the 8 cosanima specs + `apg-0.9.3` (requirements, notes,
   feedback) to the new FQNs; re-point incident edges (`Implements`, `Anchors`, `DependsOn`,
   `Details`, `Reviews`, cross-spec `SpecDependsOn`) in `apg/specs/*.jsonl`.
3. **Drop `apg spec archive`**: remove the subcommand, `apg/archived/` handling, and its
   "refuses while feedback open" logic (no archive concept remains).
4. **Remove `apg plan retag`**: CLI subcommand, `retag_task` helper, suite tool
   `apg_plan_retag.ts`, SUITE_TOOLS entry, and its tests — obsolete under the
   assertion + review + apply model (fixes go through authoring, never label-patching).

## Deliverables / done gate

- `cargo test` green, including:
  - migration round-trip: an existing spec JSONL migrates to project-scoped FQNs with all
    incident edges re-pointed;
  - cross-project FQN stability (a pending project depending on a delivered one by
    `<project>/spec.R9`);
  - no `future/` literal remains in tooling, agent prompts, or tests (verify by `rg`).
- `apg spec archive` and `apg plan retag` are absent from `--help`.

## Out of scope (later phases)

- Agent flow orchestration + docs + E2E (PHASE_05).