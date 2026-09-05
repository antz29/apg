# Plan — Graph-Native Specs, Plans, Review, and Agent Distribution

**SPEC:** `SPEC.md` (final, R1–R29, no open questions — not relitigated here).

This plan itself is prose — the graph tooling it plans (`apg spec`, `apg plan`,
`apg review`, the suite tools, the distributed agents) is what this plan builds.
It is structured exactly like the plan graph the SPEC describes: phases with
prerequisites (gates), deliverables that `Satisfies` the R-numbers, tasks with
tiers, verification items, and a done gate per phase.

## Strategy

**Layer-first, data-path-down (chosen variant).** Three candidate breakdowns:

- **Variant A — layer-first, data-path-down (CHOSEN):** build the data path
  first and the surface last. Phase 01 lands the schema (graph + record types),
  Phase 02 the serialization + scan ingest (the JSONL layouts and the rebuild
  path that everything else reuses), Phase 03 the CLI + lifecycle transitions
  (authoring, promote/done/complete/archive, review gates), Phase 04 the
  opencode suite tools, Phase 05 the agents + `apg init` distribution. Every
  phase leaves `cargo test` + `cargo build` green. The JSONL is the contract
  throughout — phases 03–05 all consume the Phase 02 data path, so the
  write-through/rebuild guarantee is exercised by every later phase.
- **Variant B — vertical slices per requirement family** (spec slice, plan
  slice, review slice, agents slice): each slice is user-visible sooner, but
  every slice re-touches schema + ingest + CLI + tools, blurring the ownership
  of each layer and re-testing the rebuild path repeatedly.
- **Variant C — agents-first:** the distributed agents are the most
  differentiating deliverable, but they consume tools that consume a CLI that
  consumes the data path; starting there inverts the dependency chain and
  forces stubs everywhere. Rejected.

**Dogfooding note:** this plan cannot be executed by the spec/plan agents it
plans — the tooling does not exist yet (chicken-and-egg). It is executed by the
user with direct commits (the repo's `STAGE_1`–`STAGE_4` precedent), and the
`tier` column documents the intended routing for when the repo defines its
writers via `agent-builder` (Phase 05). Once Phase 05 lands, representing this
very SPEC+PLAN in the graph is the dogfooding exercise that closes the loop.

## Test-tier routing (documented; effective once writers exist)

`tier` on each task below: `source` (implementer edits source/config only),
`unit` (unit tests only), `int` (integration fixture only), `gate` (build/lint/
scan gates, no new code), `human` (agent prose checks). Today a single hand
implements all tiers; the tags are the routing contract the repo's future
writers will follow.

## Repo-gate facts (the phases rely on these)

- `cargo test` — the unit gate (all new code ships unit tests with it).
- `cargo build` / `cargo build --release` — clean builds at every phase end.
- `build.rs` compiles the language frontends; `APG_BUILD_FRONTENDS=0` skips them
  when only the ingestor changes.
- The **integration fixture** (Phase 02) is a small Go project +
  hand-written `apg/specs/<project>.jsonl`, `apg/notes/<module>.jsonl`, and
  `apg/.trans/plans/<project>.jsonl` — scanned with `apg scan` (or the
  `apg_scan` tool) and asserted via `apg query`.
- The opencode suite tools (Phase 04) are TypeScript in `.opencode/tools/`
  (+ `.opencode/lib/apg.ts`), tested against the fixture db.

## Phases

Every phase carries an explicit **Prerequisites** line. No phase starts before
its prerequisites are committed green.

| Phase | Title | One-line deliverable |
|---|---|---|
| 01 | Schema: node kinds + rel tables (R1/R2/R3/R20/R21) | `graph.rs`/`schema.rs`/`load.rs` understand Spec/Requirement/Phase/Decision/Future/NonGoal/AC/VI/Note/Feedback/Plan/PlanPhase/Task + the nine new rel tables; `future/` FQN root with fail-loud collisions. |
| 02 | Serialization + scan ingest (R4/R5/R10/R11) | `apg/` committed layout (`specs/`, `notes/` per module) + `apg/.trans/`; write-through JSONL read/write; scan auto-discovers all three JSONL sets, re-ingests after code, resolves Anchors/Details/Reviews by FQN, pending→`Anchors→Future`, exports records to `graph.jsonl`. |
| 03 | CLI + lifecycle transitions (R6/R7/R8/R9/R17/R18/R19/R23/R25/R26/R27) | `apg spec` / `apg plan` / `apg review` subcommands; promote, done/undone, complete, archive transitions; derived state; review done-gates enforced. |
| 04 | Suite tools (R12/R16) | `apg_spec_*`, `apg_plan_*`, `apg_review*` opencode tools + `apg.ts` discovery update; render template. |
| 05 | Agents + `apg init` distribution (R13/R14/R15/R24/R26/R27/R28/R29) | Six distributed agents (incl. `agent-builder`), `codebase-navigator` delegation, `apg init` install/ownership + `.gitignore` scaffold. |

## Phase 01 — Schema: node kinds + rel tables

**Prerequisites:** none.

**Files touched**

- `src/graph.rs` — `NodeKind` enum + `Node` struct: new kinds (`Spec`,
  `Requirement`, `Phase`, `Decision`, `Future`, `NonGoal`,
  `AcceptanceCriterion`, `VerificationItem`, `Note`, `Feedback`, `Plan`,
  `PlanPhase`, `Task`) and the optional fields they carry (`title`, `goal`,
  `id`, `feature`, `kind`, `number`, `deliverable`, `strategy`, `tier`,
  `status`, `disposition`, `body`, `target`, `parent`).
- `src/schema.rs` — `Record` enum: node variants (`spec`, `requirement`,
  `phase`, `decision`, `future`, `non_goal`, `acceptance_criterion`,
  `verification_item`, `note`, `feedback`, `plan`, `plan_phase`, `task`) and
  edge variants (`contains`, `details`, `reviews`, `depends_on`, `gates`,
  `spec_depends`, `anchors`, `implements`, `satisfies`, `builds`).
- `src/load.rs` — `create_schema`: new node tables + rel tables (`Details`,
  `Reviews`, `DependsOn`, `Gates`, `SpecDependsOn`, `Anchors`, `Implements`,
  `Satisfies`, `Builds`) and the extended `Contains` pairs;
  `build_load_files`/`copy_from`: multi-pair load for the new rel tables.

**Work items**

- [ ] (`source`) New `NodeKind`s in `graph.rs`; `Node` gains the optional
      spec/plan fields (mirroring how `Module`/`UnresolvedTarget` are location-
      free; these kinds carry no `path`/`start`/`end`/`code_type`).
- [ ] (`source`) New `Record` variants in `schema.rs`; serde derives match the
      unified-JSONL shape from the SPEC's serialization format sections.
- [ ] (`source`) `create_schema` emits the new node/rel tables; multi-pair
      rel tables follow the `Contains`/`Uses` per-pair `COPY FROM` pattern.
- [ ] (`source`) `future/` FQN root accepted by the FQN renderer; residual
      collision panics loudly (R3), never silent overwrites.
- [ ] (`unit`) Table-creation smoke test; `Record` parse tests against the
      SPEC's fixture JSONL (spec + plan serialization sections); collision
      panic test.

**Verification**

- `cargo test` green; `cargo build` clean.
- `create_schema` produces the new tables; loading empty spec/plan records
  round-trips.

## Phase 02 — Serialization + scan ingest

**Prerequisites:** Phase 01.

**Files touched**

- `src/specs.rs` (new) — JSONL read/write for `apg/specs/*.jsonl`,
  `apg/notes/*.jsonl`, `apg/.trans/plans/*.jsonl`; module-based note routing
  (slugged module fqn filenames, `_root.jsonl` fallback); write-through
  (JSONL first, then re-ingest; crash-safe, idempotent).
- `src/ingest.rs` — the post-code spec/plan/note ingest step: read the
  discovered JSONL sets, resolve `Anchors`/`Details`/`Reviews` by FQN against
  the freshly built code graph, pending anchors → `Anchors→Future`, mark
  satisfied `Future`s.
- `src/load.rs` — `Export` enum + `write_graph_jsonl`: emit spec/plan/note/
  feedback records in `graph.jsonl`.
- `src/main.rs` — `apg scan` auto-discovery of the three JSONL sets.

**Work items**

- [ ] (`source`) `apg/` layout reader/writer: committed `specs/` + `notes/`,
      gitignored `.trans/` (plans, renders, logs, db, export). `apg init`-era
      scaffolding of `apg/.trans/` can land in Phase 05; the reader tolerates a
      missing dir.
- [ ] (`source`) Notes routing: note on a spec/`Future` node → owning project's
      `apg/specs/<project>.jsonl`; note on a code node → `apg/notes/<module>.jsonl`
      (module = the `Contains`-owning `Module`, fqn slugged; `_root.jsonl`
      fallback).
- [ ] (`source`) Write-through authoring: every mutation writes the JSONL
      first, then re-ingests into the live DB; re-running is idempotent.
- [ ] (`source`) `apg scan`: discover `apg/specs/*.jsonl`, `apg/notes/*.jsonl`,
      `apg/.trans/plans/*.jsonl`; ingest after code; resolve edges by FQN;
      unsatisfied anchors become pending (`Anchors→Future`); a `Future` whose
      `target` now exists is marked satisfied.
- [ ] (`source`) Export: `graph.jsonl` carries the spec/plan/note/feedback
      records (self-contained, canonical fqns).
- [ ] (`unit`) Round-trip: serialize → deserialize → identical graph;
      malformed record fails loudly (no partial ingest); module-routing cases.
- [ ] (`int`) Integration fixture: small Go project + the three fixture JSONL
      sets → `apg scan` → `apg query` returns spec/plan nodes and edges;
      `graph.jsonl` contains the records; a scan rebuild reproduces the graph.

**Verification**

- `cargo test` green; fixture scan + query green.
- Rebuild idempotence: two consecutive scans produce identical spec/plan/note
  graphs.

## Phase 03 — CLI + lifecycle transitions

**Prerequisites:** Phase 02.

**Files touched**

- `src/spec_cmd.rs`, `src/plan_cmd.rs`, `src/review_cmd.rs` (new) — subcommand
  logic; shared transition helpers.
- `src/main.rs` — `apg spec` / `apg plan` / `apg review` subcommand parsing
  and dispatch.

**Work items**

- [ ] (`source`) `apg spec`: `init`, `add` (requirement/phase/decision/future/
      non-goal/acceptance-criterion/verification/note), `anchor`, `link`, `rm`,
      `render`, `promote`, `archive`. `--anchor` accepts only a resolved code
      FQN or an existing `future/…` FQN — never auto-creates a `Future`.
- [ ] (`source`) `apg plan`: `init`, `add` (phase/task with `--tier`/
      `--builds`), `link`, `done`/`undone`, `complete`, `render` (checkable
      task list).
- [ ] (`source`) `apg review`: `add` (`open`), `action` (`--fix`/`--wont-fix`,
      `actioned`), `resolve` (`resolved`), `reject` (back to `open`,
      disposition `rejected`), `list`.
- [ ] (`source`) `promote` transition (R17): re-point each `Anchors(req→Future)`
      to the real code node, add `Implements(code→req)`, retire the `Future`
      (write-through).
- [ ] (`source`) `done` transition (R23): for each `Builds(Task→Future)` verify
      the `Future.target` exists in the code graph (error otherwise — a task is
      not done until its code exists), run `promote`, set `status = done`;
      `undone` flips back (no `Future` recreation).
- [ ] (`source`) `complete` (R23/R27): requires every phase task `done` AND no
      unresolved `Feedback` on the phase or its tasks; adds `Implements` per
      `Satisfies`; completing the final phase retires the plan (R22) and drops
      its JSONL.
- [ ] (`source`) `archive` (R19/R27): refuses while any `Feedback` on the
      spec's nodes is unresolved; moves the JSONL out of active discovery,
      retained as the historical record.
- [ ] (`source`) Derived state (R18): `planned`/`delivered` from `Implements`
      presence; spec `implemented` when all requirements delivered.
- [ ] (`unit`) Transition tests: promote (re-anchor + `Implements` + retire);
      `done` on an unbuilt `Builds` target errors; `complete` with open
      feedback errors; `archive` with unresolved feedback errors; reject/reopen
      cycle; round-trips survive a rebuild.

**Verification**

- `cargo test` green.
- CLI e2e against the Phase 02 fixture: author a spec, plan, and review item;
  run `done`/`complete`/`archive`; assert every refusal and transition.

## Phase 04 — Suite tools

**Prerequisites:** Phase 03.

**Files touched**

- `.opencode/tools/` — new `apg_spec_*`, `apg_plan_*`, `apg_review*` tools.
- `.opencode/lib/apg.ts` — root discovery updated to `apg/`, Cypher literal
  escaping for the new types, shared `apg spec|plan|review` subprocess plumbing.
- `src/main.rs` — `SUITE_TOOLS` registration.

**Work items**

- [ ] (`source`) Read/query: `apg_spec` (overview + counts),
      `apg_spec_requirements`, `apg_spec_phases`, `apg_spec_deps`,
      `apg_spec_anchors`, `apg_spec_trace`.
- [ ] (`source`) Lint: `apg_spec_unresolved` — dangling `depends_on`, orphan
      requirements, ACs with no covering requirement, unsatisfied `Future`s,
      spec drift (anchors to vanished nodes, `Future.target` mismatches),
      open/actioned `Feedback`.
- [ ] (`source`) Author: `apg_spec_init`, `apg_spec_add`, `apg_spec_anchor`,
      `apg_spec_link`, `apg_spec_rm`.
- [ ] (`source`) Lifecycle: `apg_spec_promote`, `apg_spec_archive`.
- [ ] (`source`) Review: `apg_review` (+ `add`, `action`, `resolve`, `reject`).
- [ ] (`source`) Plan: `apg_plan` (overview + phase table + task status),
      `apg_plan_phases`, `apg_plan_tasks` (with `Builds`/`Satisfies`/`Anchors`
      + `status`), `apg_plan_complete`, `apg_plan_render`; authoring
      `apg_plan_init`/`add`/`link`; checkout `apg_plan_done`/`undone`.
- [ ] (`source`) Render template (R16): full platform-style markdown from graph
      state (Goal, Background, Scope, Requirements grouped by feature, Design,
      Error Handling, Verification, Acceptance Criteria); `apg_spec_render`.
- [ ] (`source`) `apg.ts`: discovery resolves `apg/` walking up from cwd;
      escaping handles the new kinds' string props.
- [ ] (`int`) Tool smoke tests against the fixture db (all tools run without
      error and return the expected rows).

**Verification**

- Every new tool runs against the fixture db and returns correct rows; the
  render matches the platform template.
- The read suite's existing tools still work on repos without specs.

## Phase 05 — Agents + `apg init` distribution

**Prerequisites:** Phase 04.

**Files touched**

- `.opencode/agents/spec-writer.md`, `spec-review.md`, `plan-writer.md`,
  `plan-review.md`, `agent-builder.md` (new).
- `.opencode/agents/codebase-navigator.md` — `task: allow`, spec/plan
  delegation sections, provided-spec reading.
- `src/main.rs` — `SPEC_WRITER_AGENT`, `SPEC_REVIEW_AGENT`,
  `PLAN_WRITER_AGENT`, `PLAN_REVIEW_AGENT`, `AGENT_BUILDER_AGENT` consts
  (`include_str!`); `cmd_init` install/update; `is_apg_source_dir`;
  `remove_legacy_project_install`; `.gitignore` scaffold; `SUITE_TOOLS`.
- `AGENTS.md` — docs for the new CLI, layouts, and agents.

**Work items**

- [ ] (`source`) `spec-writer.md`: hidden subagent, `edit: "*": deny`, read
      suite + `apg_spec_*` tools + `question` + read-only bash, no `apg_scan`.
- [ ] (`source`) `plan-writer.md`: the same, with `apg_plan_*` tools; may
      translate an existing prose plan into the plan graph.
- [ ] (`source`) `spec-review.md` / `plan-review.md`: review tools
      (`apg review add`/`resolve`/`reject`/`list`) + read suite + plan read
      tools (`apg_plan_phases`/`apg_plan_tasks` for plan-review); no authoring
      tools.
- [ ] (`source`) `agent-builder.md`: `mode: primary`, the sole `edit` grant
      scoped to `.opencode/agents/**`; detect stack graph-first, interview,
      scaffold the repo's code-writer suite (implementer, test-implementers,
      reviewer, optional coordinator) with tailored permissions and the
      navigator's non-negotiable rules.
- [ ] (`source`) `codebase-navigator.md`: `task: allow`; delegates spec/plan
      creation to the writers; reads a provided spec and proposes the graph
      structure that represents it.
- [ ] (`source`) `src/main.rs`: the five new agent consts + install/update in
      `cmd_init`; `is_apg_source_dir` list becomes the six-agent set;
      `remove_legacy_project_install` removes the new files from legacy
      project installs; `apg init` scaffolds `apg/.trans/` and ensures the
      repo `.gitignore` carries `apg/.trans/` (other lines untouched).
- [ ] (`gate`) `apg init` idempotent; six agents installed into
      `~/.opencode/agents/`; ownership checks correct on the apg repo's own
      `.opencode/`.
- [ ] (`human`) Agent prose checks: navigator reads the platform-template
      `SPEC.md` and proposes a spec graph structure; `spec-writer` confirms
      against the graph and materializes it; `plan-writer` authors a plan from
      the spec graph; a review cycle attaches/actions/resolves/rejects with
      the writer unable to resolve and the reviewer unable to action;
      `agent-builder` scaffolds a sample repo's writer suite and re-running
      updates it idempotently.

**Verification**

- `cargo test` + `cargo build` green; `apg init` idempotent (re-run: no
  spurious diffs).
- All six agents present with the documented permission blocks; the apg repo's
  own `.opencode/` remains pure (`is_apg_source_dir` true).

## How to Execute

1. Execute phases in order, 01 → 05; each phase commits while the repo is
   `cargo test`- and `cargo build`-green before the next starts. A phase's
   **Prerequisites** must be committed before the phase begins.
2. Until the repo defines its writers (`agent-builder`, Phase 05), one hand
   implements every tier; the `tier` tags on each task record the routing
   contract for later.
3. Phase 02's integration fixture is the load-bearing verification — every
   later phase re-verifies against it.
4. Phase 05's `human` items are the only non-agent steps (agent prose checks
   require an interactive session).
5. After all phases, run the full gate (`cargo test`, `cargo build --release`,
   suite-tool smoke on the fixture) and a final SPEC-fulfilment review against
   the Acceptance Criteria.
6. **Dogfood:** once Phase 05 lands, represent this very `SPEC.md` + `PLAN.md`
   in the graph (spec-writer → spec graph, plan-writer → plan graph) as the
   end-to-end proof of the system.