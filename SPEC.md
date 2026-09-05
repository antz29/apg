# Graph-Native Specs for apg

## Goal

Make apg able to represent a *spec* as graph data, so a spec is authored
directly in the LadybugDB graph — not written as a monolithic prose file —
then rendered back to human-readable markdown for review, and implemented and
tracked through the same graph that holds the code it describes.

Concretely:

1. **Specs live in the graph.** A spec is a set of `Spec`/`Requirement`/
   `Phase`/`Decision`/`NonGoal`/`AcceptanceCriterion`/`VerificationItem` nodes
   plus `Contains`/`Details`/`DependsOn`/`Gates`/`SpecDependsOn`/`Anchors`/
   `Implements` edges, stored in the same `apg/.trans/db.lbug` as the code
   graph.
   **Future code** — the functions, services, and RPCs the spec says will be
   built but that don't exist yet — is captured as first-class `Future` nodes
   under the same `future/` root, so the spec, its plan, and the eventual
   implementation all reference the same placeholder nodes.
   The cross-reference structure that today lives inline in prose ("consumes
   the antecedent's R4", "gated on Phase 3", "items 3–6", "decisions A/B/C",
   anchors to `workitem_controller.go:476-496") becomes queryable edges.
2. **Durable serialization is `apg/specs/<project>.jsonl`** — a write-through,
   **committed** (the intent data lives in the repo, versioned with the code),
   rebuildable record (unified-JSONL style, fqns only). The graph DB is a
   rebuildable projection; the JSONL survives a fresh `apg scan`, which
   re-ingests it alongside code. Everything transient — the db, the graph
   export, plans, renders, logs — lives in gitignored `apg/.trans/`.
3. **`apg spec render` derives a markdown spec** from the graph (never the
   source) in the template style of the existing platform specs
   (`Goal`, `Background`, `Scope`, `Non-Goals`, `Requirements`, `Design`,
   `Error Handling`, `Verification`, `Acceptance Criteria`).
4. **A `spec-writer` agent is distributed with apg** (`apg init` installs it
   into `~/.opencode/`). It has **no file write access** and authors specs
   through the `apg spec` tooling. The existing `codebase-navigator` agent
   delegates spec creation to it.
5. **Plans are the bridge from future to present.** The phased execution plan
   — the platform's `PLAN.md` + `PHASE_*.md` artifacts — is authored as
   `Plan`/`PlanPhase`/`Task` graph data under `future/<project>/plan`
   (serialized to `apg/.trans/plans/<project>.jsonl`) by a distributed
   **`plan-writer`** agent. A phase `Satisfies` spec requirements and its tasks `Build` the spec's
   `Future` nodes; `apg plan done <task>` marks each task complete — and that
   act promotes the task's `Builds` futures into present code (`Implements`
   edges) — until every phase's tasks are done, every requirement is
   implemented, and the spec is archivable.
6. **Review is a closed feedback cycle.** A **`spec-review`** agent attaches
   `Feedback` nodes to spec nodes and a **`plan-review`** agent to plan nodes;
   the writers action (or wont-fix) them, and a reviewer resolves or rejects
   the action. An artifact is **done only when every attached `Feedback` is
   resolved** — enforced by the archive/complete gates, never asserted.
7. **`agent-builder` scaffolds the repo's code-writer suite.** Because code
   writers are repo-specific, apg ships a guide agent that detects the
   codebase's technologies graph-first, interviews the user about its build
   gates, test tiers, and git conventions, and generates the repo's
   `implementer`/test-implementer/`reviewer`/`coordinator` agents into
   `.opencode/agents/`.

This attacks the core pain of current specs: they grow huge quickly and are
inherently hard to navigate and review, because a graph-shaped document is
serialized into prose and then re-read linearly.

## Background

### Current state

Specs are prose markdown files under `plans/<project>/SPEC.md` (the platform
repo) or equivalents. They follow a consistent template but grow to 1000+
lines (e.g. `workitem-timer/SPEC.md`, `model-gateway/SPEC.md`). The review
burden is linear reading of prose; structural facts — requirement ids
(`A1–A7`, `G1–G7`, `Q1–Q3`, `R1–R10`), dependencies ("consumes the
antecedent's R4"), phase ordering ("Phase 2 gated on Phase 3"), code anchors
(`workitem_controller.go:476-496`), cross-spec antecedents
(`plans/sdk-session-standardisation/`) — are all embedded inline in prose.

The platform `spec-writer` agent (`~/platform/.opencode/agents/spec-writer.md`)
writes exactly one file, `plans/<project>/SPEC.md`, with a fixed template and
`edit` permission scoped to `plans/**/SPEC.md`. It explores the codebase
graph-first via the apg suite.

### apg pipeline (what this extends)

The pipeline is scanner → ingestor → graph → LadybugDB:

- Scanners (Go/Java/C++/Rust/TS/C#) emit one JSON object per line — the
  unified JSONL schema (`src/schema.rs`, `Record` enum) — facts only, opaque
  ids, no FQNs, no graph assembly.
- `src/ingest.rs` resolves identity (canonical FQNs), builds the graph
  (`src/graph.rs`: `NodeKind`, `Graph`, `Node`), and classifies `code_type`
  (`src/classify.rs`).
- `src/load.rs` bulk-loads LadybugDB (`create_schema` :330, `build_load_files`
  :132, `copy_from` :358) and writes `graph.jsonl` (`write_graph_jsonl` :480,
  `Export` enum :421).
- The DB schema today: five node tables (`Module`, `Struct`, `Function`,
  `File`, `UnresolvedTarget`) and five rel tables (`Contains`, `Calls`,
  `Uses`, `UnresolvedCall`, `UnresolvedUse`). Multi-language scans merge via a
  `lang_switch` control record and per-language `--id-prefix`.

The graph DB is **rebuilt** on every `apg scan`; the durable artifacts are the
scan sources (code), the committed `apg/` intent data (`config.json`,
`apg/specs/*.jsonl`, `apg/notes/*.jsonl`), and exports. Everything transient
lives in gitignored `apg/.trans/`.

### What this spec adds

Specs are treated as just another graph-fed source, merged into the same DB so
spec↔code queries work in one Cypher space. The existing `apg` architecture
(a new "frontend" that feeds the same `Graph`, extended node/rel schema, new
suite tools, agent distribution via `apg init`) makes this an extension rather
than a new system.

## Scope

- **Schema:** new node kinds (`Spec`, `Requirement`, `Phase`, `Decision`,
  `Future`, `NonGoal`, `AcceptanceCriterion`, `VerificationItem`, `Note`,
  `Feedback`, `Plan`, `PlanPhase`, `Task`) and new/extended rel tables
  (`Contains` pairs, `Details`, `Reviews`, `DependsOn`, `Gates`,
  `SpecDependsOn`, `Anchors`, `Implements`, `Satisfies`, `Builds`) in
  `src/graph.rs` + `src/load.rs`.
- **Serialization:** a committed `apg/` directory at the repo root holds the
  **durable** intent data — `apg/config.json` (classification config),
  `apg/specs/<project>.jsonl` (specs: write-through, rebuildable, unified-JSONL
  style, canonical fqns, no opaque ids), and `apg/notes/<module>.jsonl` (notes
  on code nodes, split one file per owning module). Everything **transient** —
  the rebuilt DB (`db.lbug`), the `graph.jsonl` export, `plans/<project>.jsonl`
  (the bridge under construction), rendered markdown, and logs — lives in
  gitignored `apg/.trans/`. `Feedback` nodes serialize in the artifact JSONL of
  their review (spec feedback → `apg/specs/`; plan/task/code feedback →
  `apg/.trans/plans/`).
- **CLI:** `apg spec` subcommands — `init`, `add` (requirement/phase/decision/
  future/non-goal/acceptance-criterion/verification/note), `anchor`, `link`,
  `rm`, `render`, `promote`, `archive`. `apg plan` subcommands — `init`,
  `add` (phase/task), `link`, `done`/`undone`, `complete`, `render`.
  `apg review` subcommands — `add`, `action`, `resolve`, `reject`, `list`.
- **Scan integration:** `apg scan` auto-discovers `apg/specs/*.jsonl`,
  `apg/notes/*.jsonl`, and `apg/.trans/plans/*.jsonl`, re-ingests
  specs/plans/notes after code, re-attaches anchors by FQN, and exports
  spec/plan/note/feedback records in `graph.jsonl`.
- **Suite tools:** `apg_spec`, `apg_spec_requirements`, `apg_spec_phases`,
  `apg_spec_deps`, `apg_spec_anchors`, `apg_spec_unresolved` (lint),
  `apg_spec_trace`, `apg_spec_init`, `apg_spec_add`, `apg_spec_anchor`,
  `apg_spec_link`, `apg_spec_rm`, `apg_spec_render`, the `apg_plan_*` and
  `apg_review_*` tools — installed by `apg init`.
- **Agents:** distributed `spec-writer`, `spec-review`, `plan-writer`,
  `plan-review`, `agent-builder` (subagents/primary, **no file write access**
  except `agent-builder`'s scoped `.opencode/agents/**` write) + the existing
  `codebase-navigator`. Writers update artifact nodes and action feedback;
  reviewers attach/resolve feedback — a closed cycle neither side can complete
  alone (tool permissions enforce it). **All are language-agnostic** — no repo
  paths, no language toolchains, no build gates. Code writers
  (implementer, test-implementers) are **not** distributed (repo-defined,
  scaffolded by `agent-builder`); they consume apg's tools.
  `codebase-navigator` gains `task` permission, delegates spec/plan creation to
  the writers, and reads a provided spec (prose `SPEC.md` or requirements
  description) to propose the spec graph structure that represents it. `apg init`
  installs all six and keeps its ownership checks correct.

## Non-Goals

- **No parsing of existing markdown files into the graph.** Neither `SPEC.md`
  nor `PLAN.md`/`PHASE_*.md` are parsed — the graph is authored directly; there
  is no markdown source of truth to parse. Existing prose plans are translated
  by an agent (the plan-writer) into `Plan`/`PlanPhase`/`Task` graph data, just
  as existing prose specs are translated by the spec-writer.
- **No code generation from spec/plan graphs.** "The graph is implemented" means the
  graph is the navigation and tracking structure implementation works through
  (`Implements` edges, `apg plan complete`), not that code is synthesized from
  requirements.
- **No persistent spec store.** The spec graph is not durable on its own; the
  JSONL is the durable form and the DB is rebuilt from it after every scan.
- **No direct DB mutation outside `apg spec`.** No generic Cypher `CREATE`;
  authoring is only through the `apg spec` CLI/tools.
- **No change to code scanning or fidelity.** Code nodes/edges are untouched;
  the code graph's semantics are preserved (Contains is extended, not
  redefined).
- **Notes do not replace structured data.** Narrative prose goes in `Note`
  nodes; anything queryable (ids, titles, features, phases, deps, anchors)
  is structured.
- **No distributed code-writer agents.** apg does not ship `implementer`/
  test-implementer agents — a code writer's lockdown (build gates, git
  conventions, repo layout) is inherently repo- and language-specific. A repo
  defines its own writers (the platform's are the reference pattern), guided by
  the distributed **`agent-builder`** agent (R29); apg distributes only the
  language-agnostic spec/plan/review agents, the `agent-builder`, and the tools
  (`apg_plan_done`, `apg_review_action`, …) the repo's writers consume.

## Requirements

### Schema & data model

**R1 — Spec node kinds.** Add `NodeKind` variants and node tables. The
`future/` root namespaces everything not-yet-implemented; a spec's FQNs live
under `future/<project>/spec` (later, the plan graph lands at
`future/<project>/plan`, and **future code nodes** — placeholders for
to-be-built code — under `future/<project>/`):
- `Spec {fqn, title, goal}` — fqn is `future/<project>/spec` (e.g.
  `future/workitem-timer/spec`); the `apg/specs/<project>.jsonl` path is the
  serialization home, not the identity.
- `Requirement {fqn, id, title, body, feature}` — fqn is
  `future/<project>/spec.<id>`; `body` is a short, queryable one-liner;
  `feature` groups requirements (e.g. `feature-a`, `feature-b`) for render.
- `Phase {fqn, number, title}` — fqn is `future/<project>/spec.phase-<n>`.
- `Decision {fqn, id, summary}` — fqn is `future/<project>/spec.decision-<id>`.
- `Future {fqn, kind, target}` — a placeholder for **future code**: a function,
  struct, service, RPC, or endpoint the spec/plan says will be built but does
  not exist in the code graph yet. fqn is `future/<project>/<name>`; `kind` is
  a fixed v1 set `function`/`struct`/`service`/`rpc`/`endpoint`/`other`
  (declared by the author, never guessed); `target` is the intended real FQN
  once implemented (enables satisfaction and drift detection). This is the
  first-class generalization of the old "pending anchor" idea — specs, plans,
  and implementation all reference the same `Future` nodes.
- `NonGoal {fqn, body}`, `AcceptanceCriterion {fqn, body}`,
  `VerificationItem {fqn, body}`.
- `Note {fqn, body, kind}` — the universal prose container (Background
  narrative, design rationale, open questions, reviewer comments). `kind` is a
  fixed v1 section tag: `background` (Background narrative), `design` (Design
  rationale), `error-handling`, `open-question`, `decision` (decision
  rationale), `comment` (reviewer/discussion comments), `misc` (catch-all);
  extensible later. Render maps the section kinds to sections and the rest to a
  general Notes/Comments section.
  **Notes route by what they `Details`:** a note on a spec/`Future` node is
  project-owned (fqn `future/<project>/note-<n>`) and serializes in the owning
  project's `apg/specs/<project>.jsonl` (committed with the spec); a note on a
  **code** node is repo-level (fqn `annotations/<n>`) and serializes in
  `apg/notes/<module>.jsonl` — the committed annotation ledger, **split one
  file per owning module** (the module that `Contains` the annotated code node;
  module fqns slugged to filenames) so the ledger is merge-friendly and each
  module's annotations version with the code they annotate. A note whose target
  is not yet in a module (e.g. repo-level) lands in `apg/notes/_root.jsonl`.
- `Feedback {fqn, body, status, disposition}` — a **review item** in the
  writer↔reviewer cycle. fqn is `future/<project>/feedback-<n>`;
  `status` ∈ `open`/`actioned`/`resolved`; `disposition` ∈
  `fixed`/`wont-fix`/`rejected` (set when actioned/rejected). Lifecycle:
  reviewer attaches (`open`) → writer actions or wont-fixes (`actioned`) →
  reviewer accepts (`resolved`, terminal) or rejects (returns to `open`).
  An artifact is **done only when every `Feedback` that `Reviews` it is
  `resolved`**. Feedback routes to the artifact JSONL of its review: target is
  a spec/`Future` node → `apg/specs/<project>.jsonl`; target is a plan/task
  or code node → `apg/.trans/plans/<project>.jsonl`.

**R2 — Rel tables.** Extend `Contains` with spec pairs and add new rel tables:
- `Contains` + `(Spec→Requirement, Spec→Phase, Phase→Requirement, Spec→Decision,
  Spec→NonGoal, Spec→AcceptanceCriterion, Spec→VerificationItem)` — the
  existing multi-pair table absorbs these; code-graph queries are unaffected
  (they match on code labels).
- `Details(Note→Module, Note→Function, Note→Struct, Note→File, Note→Spec,
  Note→Requirement, Note→Phase, Note→Decision, Note→NonGoal, Note→AcceptanceCriterion,
  Note→VerificationItem)` — **all** kinds by default (code + spec), so Notes
  are the universal annotation container: `(n:Note)-[:Details]->(x)` reads
  "note details node x".
- `Reviews(Feedback→Module, Feedback→Function, Feedback→Struct, Feedback→File,
  Feedback→Spec, Feedback→Requirement, Feedback→Phase, Feedback→Decision,
  Feedback→NonGoal, Feedback→AcceptanceCriterion, Feedback→VerificationItem,
  Feedback→Future, Feedback→Plan, Feedback→PlanPhase, Feedback→Task)` — a
  reviewer's feedback targets the artifact node it reviews. All kinds, like
  `Details`.
- `DependsOn(Requirement→Requirement)` — "consumes R4".
- `Gates(Phase→Phase)` — phase ordering / gating ("Phase 3 gated on Phase 2").
- `SpecDependsOn(Spec→Spec)` — cross-spec antecedents / prior art.
- `Anchors(Requirement→Function, Requirement→Struct, Requirement→File,
  Requirement→Future)` — resolved anchors point at real code nodes; **pending**
  anchors (references to not-yet-built code) target a `Future` node (e.g.
  `future/workitem-timer/gateway`). A pending anchor whose `Future.target` later
  exists in the code graph is **satisfied** (auto-detected on rebuild); the
  anchor stays stable across rebuilds either way.
- `Implements(Function→Requirement, Struct→Requirement, File→Requirement)` —
  added during implementation; serializes so it survives rebuilds.

**R3 — FQN uniqueness / fail-loud.** Spec and Future fqns follow the
`future/<project>...` convention. The ingestor's existing collision behavior
applies: any residual FQN collision panics loudly rather than silently
overwriting.

### Serialization

**R4 — Durable serialization.** A committed `apg/` dir at the repo root, with
gitignored `apg/.trans/` for everything rebuildable:
- `apg/specs/<project>.jsonl` — one spec's durable, write-through serialization
  (**committed**). Records are type-tagged in the unified-JSONL style (`spec`,
  `requirement`, `phase`, `decision`, `future`, `non_goal`,
  `acceptance_criterion`, `verification_item`, `note`, `feedback`, `contains`,
  `details`, `reviews`, `depends_on`, `gates`, `spec_depends`, `anchors`,
  `implements`), canonical fqns only, no opaque ids. This is the source a fresh
  `apg scan` re-ingests from. Notes and spec-review `Feedback` on spec/`Future`
  nodes serialize here.
- `apg/notes/<module>.jsonl` — the **committed** annotation ledger, split one
  file per owning module: `note` + `details` records whose targets are code
  nodes (module = the module that `Contains` the annotated node, fqn slugged;
  fallback `_root.jsonl`). `apg scan` reads all of them if present.
  (Code-review `Feedback` belongs to a plan phase and serializes in the plan
  JSONL, not here.)
- `apg/.trans/` — gitignored: `db.lbug`, `graph.jsonl`, `apg-frontend.log`,
  `plans/<project>.jsonl`, and rendered markdown. None of it is committed;
  all of it is re-derivable from source + the committed `apg/` data.

**R5 — Write-through authoring.** Every `apg spec` mutation is write-ahead:
load the project's JSONL → apply the mutation → write the JSONL back → re-ingest
the spec against the current code graph so the live DB reflects it immediately.
A crash or scan mid-session loses nothing. Authoring is idempotent:
`add` upserts by id, `rm` removes a node and its incident edges.

### CLI

**R6 — `apg spec init <project> --title … --goal …`.** Creates the `Spec` node
and `apg/specs/<project>.jsonl`.

**R7 — `apg spec add`.** Adds nodes: `requirement <id> [--title …] [--body …]
[--feature …] [--depends-on <id>]* [--anchor <fqn>]*`,
`future <name> --kind function|struct|service|rpc|endpoint|other [--target <fqn>]`
(a placeholder for future code, also the pending-anchor target),
`phase <n> --title … [--gate <phase-n>]*`, `decision <id> --summary …`,
`non-goal|acceptance-criterion|verification --body …`, `note --body …
[--kind …] [--on <fqn>]*` (attaches `Details` edges; a `--on` target that is a
code FQN routes the note to the committed `apg/notes/<module>.jsonl`, a
spec/`Future` FQN to the project's `apg/specs/<project>.jsonl`). Options may be
repeated;
`--anchor` accepts only a **resolved** code FQN or an **existing** `future/…`
FQN — an unresolvable FQN is an error (a `Future` is never auto-created; the
author declares future code explicitly with `add future`, then anchors to it).

**R8 — `apg spec anchor <project> <req-id> <fqn>` / `apg spec link <project>
<req-id> --depends-on <id>` / `apg spec rm <project> <id>`.** Add/remove edges
and nodes explicitly. `anchor` accepts a resolved code FQN or an existing
`future/…` FQN; an unresolvable FQN is an error (no auto-created `Future`).

**R9 — `apg spec render <project> [--out <path>]`.** Derive a human-readable
markdown spec from the graph. Default output is `apg/.trans/specs/<project>.md`
(gitignored); `--out -` writes to stdout. The render is a projection — editing
it is never a supported path; the graph is the source.

### Scan integration

**R10 — Re-ingest on `apg scan`.** `apg scan` auto-discovers
`apg/specs/*.jsonl`, `apg/notes/*.jsonl`, and `apg/.trans/plans/*.jsonl` (if
present) after code ingestion, ingests the spec/plan/note/**feedback** records
into the same DB, and resolves `Anchors`/`Details`/`Reviews` by FQN against the
freshly built code graph.
Anchors whose FQN is not in the code graph become **pending** anchors to a
`Future` node (or stay on their existing `Future` node). A `Future` whose
`target` FQN now exists in the code graph is marked **satisfied** (reported by
lint and `apg_spec_trace`); pending anchors whose target exists are surfaced so
the author can re-anchor to the real code node. Spec anchors never fail a scan
— future code is expected.

**R11 — Export.** `graph.jsonl` includes spec node and edge records so the
self-contained export artifact covers specs and code together.

### Suite tools

**R12 — New opencode tools.** Installed by `apg init` into `~/.opencode/tools/`
alongside the existing suite (registered in `SUITE_TOOLS` in `src/main.rs`):
- Read/query: `apg_spec` (overview + counts), `apg_spec_requirements`,
  `apg_spec_phases`, `apg_spec_deps`, `apg_spec_anchors`, `apg_spec_trace`
  (requirement → deps → anchors → implementing code).
- Lint: `apg_spec_unresolved` — dangling refs (`depends_on` → nonexistent id),
  orphan requirements (in no phase), acceptance criteria with no covering
  requirement, unsatisfied `Future` nodes (planned code not yet built), spec
  drift (anchors to code nodes that no longer exist after a refactor, or
  `Future.target` mismatches).
- Author: `apg_spec_init`, `apg_spec_add`, `apg_spec_anchor`, `apg_spec_link`,
  `apg_spec_rm`.
- Lifecycle: `apg_spec_promote` (Future → present transition), `apg_spec_archive`
  (retire a fully-implemented spec from active discovery).
- Review: `apg_review` (list feedback for a target), `apg_review_add`,
  `apg_review_action`, `apg_review_resolve`, `apg_review_reject` — the closed
  writer↔reviewer cycle.
- Plan: `apg_plan` (overview + phase table + task status), `apg_plan_phases`,
  `apg_plan_tasks` (with `Builds`/`Satisfies`/`Anchors` and `status`),
  `apg_plan_complete` (complete a phase), `apg_plan_render` (PLAN.md-style
  markdown); authoring `apg_plan_init`, `apg_plan_add` (phase/task),
  `apg_plan_link` (satisfies/builds/prereq); checkout `apg_plan_done`,
  `apg_plan_undone` (mark a task complete / revert).
- Derive: `apg_spec_render`.
Shared plumbing (root discovery, `apg spec` subprocess, Cypher literal
escaping) extends `~/.opencode/lib/apg.ts`.

### Agents & distribution

**R13 — Distributed `spec-writer` agent.** New `.opencode/agents/spec-writer.md`,
adapted from the platform `~/platform/.opencode/agents/spec-writer.md`, with:
- `mode: subagent`, `hidden: true`.
- **No file write access**: `edit: "*": deny` (the platform version's
  `"plans/**/SPEC.md": allow` is removed entirely). The agent authors purely
  through `apg_spec_*` tools; the JSONL is produced by the tooling.
- Permissions: `read: allow`, `external_directory: ask`, the full read suite +
  the `apg_spec_*` tools + `question` + read-only bash. **No `apg_scan`** —
  a stale/missing graph is reported, never silently rescanned.
- Workflow: `apg spec init <project>` → clarifying questions (one at a time,
  multiple choice preferred) → 2–3 proposed approaches with a recommendation →
  present the design and get approval → author the spec (requirements, phases,
  decisions, non-goals, acceptance criteria, verifications, notes) with
  `Anchors` to real code nodes (or `Future` nodes for not-yet-built code) and
  `DependsOn`/`Gates` edges → self-review via the lint tools → report the spec
  fqn and the next step. When handed a **proposed graph structure** or a source
  spec from the `codebase-navigator`, the spec-writer confirms every proposed
  requirement/anchor/dependency against the code graph (anchors must resolve or
  be declared `Future`), refines with the user, then materializes it via the
  tools.
- Self-review gate (mirrors the platform agent): remove placeholders, resolve
  contradictions, make ambiguous requirements explicit, confirm acceptance
  criteria are objective pass/fail statements, confirm every requirement is in
  a phase and every `depends_on` target exists.

**R14 — `codebase-navigator` delegation + spec/plan reading.** `.opencode/agents/codebase-navigator.md`
gains:
- `task: allow` in its permission block (currently absent — required to spawn
  subagents).
- A "spec authoring" section: when the user asks to turn an idea or feature
  request into a spec, delegate to the `spec-writer` subagent via the task
  tool; never author a spec inline.
- A "plan authoring" section: when the user asks to turn an existing spec into
  a phased implementation plan, delegate to the `plan-writer` subagent; never
  author a plan inline.
- **Reading a provided spec:** when the user supplies an existing spec — a
  prose `SPEC.md` in the platform template style, or any requirements
  description — the navigator reads it (via `read`/`apg_query`) and proposes a
  **spec graph structure** to represent it: the decomposition into
  `Requirement` ids grouped by `feature`, `Phase` ordering with `Gates`,
  `Decision`s, `NonGoal`s, `AcceptanceCriterion`s, `VerificationItem`s, `Future`
  nodes for code that doesn't exist yet, `Note`s (with `kind`) for the prose
  narrative, `DependsOn`/`Anchors` edges, and `SpecDependsOn` for cross-spec
  references. This is **agent prose** — the navigator reasons about the source
  spec and presents a proposed graph structure, then delegates authoring of
  that structure to the `spec-writer` subagent (which confirms the proposal
  against the code graph, refines it with the user, and materializes it via the
  `apg_spec_*` tools). The navigator never authors the graph itself.

**R15 — `apg init` installation and ownership.** `src/main.rs`:
- New `SPEC_WRITER_AGENT`, `SPEC_REVIEW_AGENT`, `PLAN_WRITER_AGENT`,
  `PLAN_REVIEW_AGENT`, `AGENT_BUILDER_AGENT` consts (`include_str!` on the
  `.opencode/agents/*.md` files); install/update them in `cmd_init` alongside
  `codebase-navigator.md`.
- `is_apg_source_dir` (:414) agents check becomes `["codebase-navigator.md",
  "spec-writer.md", "spec-review.md", "plan-writer.md", "plan-review.md",
  "agent-builder.md"]` — otherwise the apg repo's own `.opencode/`
  mis-detects as non-pure.
- `remove_legacy_project_install` (:435) also removes the five new agent files
  from legacy project-local installs.
- Scaffolds the layout: creates `apg/` (committed) with `.trans/` inside, and
  ensures the repo `.gitignore` carries `apg/.trans/` (added if missing, other
  lines untouched) so the durable `apg/` data is committed and only transient
  state is ignored.
- Printed install/update counts reflect all six agents.

### Render template

**R16 — Render section mapping.** `apg spec render` produces the platform
template style from graph state:
- `# <Spec.title>` · `## Goal` (Spec.goal)
- `## Background` — Notes with `kind = background`; graph-verified facts rendered
  as bullets with anchor `path:line` resolved from `Anchors` edges;
  `Antecedent: …` lines from `SpecDependsOn`.
- `## Scope` / `## Requirements` — requirements grouped by `feature`
  (`### Feature A`), each `**A1 — title.** body`, with explicit
  `Consumes: R1, R4` (from `DependsOn`) and `Anchors: <fqn>` lines.
- `## Non-Goals` — `NonGoal` nodes · `## Design` / `## Error Handling` — Notes
  by kind · `## Verification` — `VerificationItem` nodes · `## Acceptance
  Criteria` — `AcceptanceCriterion` nodes · `## Open Questions` — Notes with
  `kind = open-question` · `## Notes / Comments` — Notes with
  `kind = comment`/`misc`/`decision`.
- `Phase ordering (fixed)` — `Phase` nodes + `Gates` edges · Decisions from
  `Decision` nodes.

### Future → present transition (lifecycle)

**R17 — `apg spec promote <project> <future-name>` / `--all`.** The explicit
transition of planned code into the present. For each
`Anchors(req→Future f)` where `f.target` resolves in the current code graph:
re-point the anchor to the real code node (`Anchors(req→f.target)`), add
`Implements(f.target→req)` (the code delivers the requirement), and retire
`f` (remove the `future` record + incident edges from the project JSONL,
write-through). `--all` promotes every satisfiable `Future` in the project.
If `f.target` does not resolve → **error** (stale graph or target mismatch;
never guess, never auto-promote). Lint (`apg_spec_unresolved`) reports
satisfiable futures — "planned code now exists, run `promote`" — so the act is
always deliberate.

**R18 — Derived state, no renaming.** Requirements/Phases keep their
`future/<project>/spec.<id>` identity for the spec's lifetime — a plan record
does not become code, it becomes *realized*. Per-requirement state is derived:
`planned` (anchors to a `Future`, no `Implements`) vs `delivered` (an
`Implements` edge present). Spec-level state is derived from its requirements:
`planned` → `implemented` (all requirements delivered). FQNs are identity and
never change to express state; the `future/` root is the plan's home, not a
liveness flag.

**R19 — `apg spec archive <project>`.** The whole-spec "move into the past":
stops the project's JSONL from being discovered by `apg scan` (moves it out of
`apg/specs/`), while the `Implements` edges already in the code graph keep the
delivered work traceable. No FQN changes; the archived JSONL is retained as the
historical plan record.

### Plan graph (the bridge between present and future)

The phased execution plan is the artifact that carries the spec (`future`) into
the code (`present`). The platform's plan files establish the shape this models:
`PLAN.md` holds the strategy + a phase table whose deliverable columns name the
spec's requirements and prerequisites; `PHASE_N.md` holds per-phase goals,
deliverables (routed by test tier), files touched, executable verification
gates, acceptance criteria, and handoff. In the graph these become `Plan`/
`PlanPhase`/`Task` nodes under `future/<project>/plan` with edges that are the
bridge.

**R20 — Plan node kinds.**
- `Plan {fqn, title, strategy}` — fqn is `future/<project>/plan`; `strategy`
  (or a `Note`) carries the strategy text (variants considered, test-tier
  routing, repo-gate facts, execution method).
- `PlanPhase {fqn, number, title, deliverable}` — one row of the phase table;
  fqn is `future/<project>/plan.phase-<n>` (zero-padded, matching the PHASE
  files); `deliverable` is the one-line deliverable.
- `Task {fqn, title, tier, status}` — a phase deliverable; fqn is
  `future/<project>/plan.phase-<n>.task-<k>`; `tier` ∈
  `source`/`unit`/`int`/`e2e`/`gate`/`human` **routes the owning writer, which
  is repo-defined** (the platform's example: a `source` task is written and
  marked done by the implementer, `unit` by the unit-test-implementer, `int` by
  the int-test-implementer, `e2e` by the e2e-test-implementer, `human` by a
  human); `status` ∈ `pending`/`done`, the per-task checklist state — **the
  plan presents individual tasks per phase, marked off as the plan is
  implemented, each by its tier's (repo-defined) writer**.

**R21 — Plan rel tables.**
- `Contains` + `(Plan→PlanPhase, PlanPhase→Task, PlanPhase→AcceptanceCriterion,
  PlanPhase→VerificationItem)` — each phase carries its own ACs and verification
  gates (reusing the spec node kinds).
- `Gates` + `(PlanPhase→PlanPhase)` — prerequisites ("02→01; 03→01+02").
- `Satisfies(PlanPhase→Requirement)` — the phase delivers a spec requirement
  (the "R1/R5/R6" column). **The bridge edge from plan to future.**
- `Builds(Task→Future)` — the task creates a piece of planned code. **The
  bridge edge from plan to future code.**
- `Anchors` + `(Task→Function, Task→Struct, Task→File)` — files touched.
- `Details` + `(Note→Plan, Note→PlanPhase, Note→Task)` — strategy, seam
  contracts, implementation notes, handoff prose.

**R22 — `apg/.trans/plans/<project>.jsonl`.** The plan's write-through
serialization (gitignored, unlike the committed spec): `plan`, `plan_phase`,
`task`, `acceptance_criterion`, `verification_item`, `note`, `feedback`,
`contains`, `gates`, `satisfies`, `builds`, `anchors`, `details`, `reviews`
records.
Plan-review `Feedback` and implementation (code-review) `Feedback` — whose
targets are plan nodes, tasks, or code nodes — serialize here. Re-ingested by
`apg scan` alongside `apg/specs/*.jsonl`. **The plan is transient by design**:
it is the roadmap for building the future→present bridge, not the record of
it. Once every phase is complete (R23), the plan is retired and its JSONL
dropped — the durable trace of the bridge is the spec's `Implements` edges and
the retired `Future`s, which live in the spec JSONL. A plan is never committed
and never archived (unlike a spec, which persists for provenance).

**R23 — `apg plan` CLI.** `init <project> --title … [--strategy …]`;
`add <project> phase <n> --title … [--deliverable …] [--prereq <n>]*
[--satisfies <req-id>]*`; `add <project> task <phase> <k> --title … [--tier …]
[--builds <future-name>]`; `link`; `render <project>` (PLAN.md-style markdown,
tasks rendered as a checkable list); `done <project> <task-fqn>` — **mark a
task complete, and that act moves its portion of future work into the
present**: for each `Builds(Task→Future)` on the task, verify the
`Future.target` exists in the code graph (error if not — the task is not done
until its code exists), run the `promote` transition (re-anchor
`Anchors(req→Future)` to the target code node, add `Implements(code→req)`,
retire the `Future`), then set `status = done`. Tasks with no `Builds` edge
(seams, gates, tests) are a plain status flip; `undone <project> <task-fqn>`
flips `status` back to `pending` (it does not recreate retired `Future`s — the
code is already in the present; undoing is a checklist correction, not a time
machine); `complete <project> <phase-n>` — the phase-level act, requires every
phase task `done` (error otherwise), then for every
`Satisfies(PlanPhase→Requirement)` adds the `Implements` edge from the phase's
built code (the promoted futures' targets / task anchors) to the requirement,
idempotently; completing the final phase retires the plan (R22) and drops its
JSONL.
Phase by phase, task by task, the plan's `Future` nodes dissolve into present
code — the plan is executed out of the graph, one checked-off task at a time.
**Completing the final phase retires the plan itself**: the plan's job — build
the `Implements` bridge from future intent to present code — is done, and the
plan JSONL is dropped (the durable record is the spec's `Implements` edges,
the retired `Future`s, and the code). A plan that was never fully executed
simply stays in `apg/.trans/plans/<project>.jsonl` (gitignored working state)
until it is.

**R24 — Distributed `plan-writer` agent.** New `.opencode/agents/plan-writer.md`,
the counterpart of `spec-writer`, adapted from the platform
`~/platform/.opencode/agents/plan-writer.md`:
- `mode: subagent`, `hidden: true`; **no file write access**
  (`edit: "*": deny` — the platform version's `plans/**/PLAN.md` +
  `plans/**/PHASE_*.md` writes are removed entirely). It authors the plan
through the `apg_plan_*` tools; the `apg/.trans/plans/<project>.jsonl` is
produced by the tooling.
- Permissions: `read: allow`, `external_directory: ask`, the full read suite +
  the `apg_spec_*` read tools (to read the spec graph) + the `apg_plan_*`
  tools + `question` + read-only bash. **No `apg_scan`.**
- Workflow: select a project (from the spec graph — list `Spec` nodes /
  `apg/specs/*.jsonl`; if none exists, stop and ask for a spec via
  `spec-writer`) → read the spec graph (requirements by feature, phases,
  decisions, `Future` nodes) → **propose at least two phase breakdowns**
  (architectural layer / service / risk / dependency chain / vertical slice),
  each with phase names + one-line deliverables + the dependency ordering it
  creates, and let the user choose → author `Plan`/`PlanPhase`/`Task` nodes:
  `apg plan init` + `apg plan add phase … --prereq … --satisfies <req-id>`
  + `apg plan add task … --tier … --builds <future-name>`, with `Anchors` for
  files touched and `Note`s for strategy/seam contracts/handoff → self-review
  via `apg_plan_phases`/`apg_plan_tasks` (every requirement is `Satisfies`-ed
  by at least one phase; phases dependency-ordered with no `Gates` cycle;
  deliverables concrete — "create `RootStore` + `LoadOrMintRoot`", not
  "implement the CA") → report the plan fqn + next step. Each `Task` is
  authored `pending`; the task list per phase is the implementation checklist
  that implementers mark off with `apg plan done` as code lands.
- It may also **translate an existing prose plan** (platform `PLAN.md` +
  `PHASE_*.md`) into the plan graph, confirming each phase's requirements,
  prerequisites, and files against the code graph first (mirroring the
  spec-writer's translation role).

### Review cycle (feedback)

**R25 — `Feedback` node + lifecycle.** A review item attached to an artifact
node via `Reviews`. `status`: `open` → `actioned` → `resolved` (terminal);
a rejected action returns `actioned` → `open`. `disposition` records the
outcome: `fixed` / `wont-fix` / `rejected`. The cycle is **closed and
separation-of-duties**: a reviewer attaches (`open`); a writer actions or
wont-fixes (`actioned`); the reviewer then marks `resolved` (accepted) or
`rejected` (reopens). The writer cannot resolve; the reviewer cannot action —
enforced by tool permissions, never by convention.

**R26 — `apg review` CLI.**
- `apg review add <target-fqn> --body … [--kind …]` — reviewer attaches a
  `Feedback` (`open`) to any artifact node (spec, plan, task, or code).
- `apg review action <feedback-fqn> --fix|--wont-fix [--note …]` — writer
  actions it (`actioned`, disposition set).
- `apg review resolve <feedback-fqn>` — reviewer accepts (`resolved`).
- `apg review reject <feedback-fqn>` — reviewer rejects the action (back to
  `open`, disposition `rejected`).
- `apg review list [<target-fqn>]` — list feedback with status.

**R27 — Done gates (derived, enforced).** "Done" is never asserted; it is a
query over `Feedback`:
- A spec is **review-complete** when no `Feedback` with `status <> 'resolved'`
  `Reviews` any of its nodes. `apg spec archive` refuses otherwise.
- A plan phase is **complete** when every task is `done` **and** no unresolved
  `Feedback` reviews the phase or its tasks. `apg plan complete` enforces both.
- A `Task` at `status = done` with unresolved feedback is *under review*, not
  complete; lint reports it.
- `apg_spec_unresolved` / `apg_plan_phases` lint surfaces every open/actioned
  feedback on an artifact.

**R28 — Review agents (distributed + repo-defined).** The closed cycle is the
same for spec, plan, and code; **only the spec/plan agents are distributed by
apg**, and they are language-agnostic (they hold no repo paths, no language
toolchains, no build gates — just the read suite + apg tools + `question` +
generic read-only bash):
- **`spec-review`** — may attach (`apg review add`), `resolve`, `reject`, and
  `list` feedback on spec nodes; the read suite. It **may not** modify spec
  nodes (no `apg_spec_init/add/anchor/link/rm`).
- **`plan-review`** — the same, for plan/task/code nodes (implementation
  review), plus `apg_plan_phases`/`apg_plan_tasks` to see the checklist.
- **`spec-writer`** / **`plan-writer`** — the writers additionally hold
  `apg review action` (they respond to feedback); they **may not** attach or
  resolve feedback.
- **Code writers are repo-defined, not distributed.** The platform's
  implementer + `unit`/`int`/`e2e`-test-implementer agents (`Task.tier` routing)
  and its coordinator are Go-specific examples of the pattern — a repo defines
  its own writers with its own permissions (build gates, git conventions,
  layout). apg distributes the *tools* they consume (`apg_plan_done`,
  `apg_review_action`, …) but never the agents, because a code writer's
  lockdown is inherently repo-specific (a C# repo has no `make verify`).
- Code review (per phase) uses the same `Feedback`/`Reviews` mechanism: a code
  reviewer attaches feedback to the phase's tasks or their anchored code nodes;
  the tier-owning writer actions it via `apg review action`.
- The cycle for spec, plan, and code is identical: **writer writes → reviewer
  attaches feedback → writer actions (or wont-fixes) → reviewer resolves or
  rejects → artifact is done when all feedback is resolved.**

**R29 — Distributed `agent-builder` (scaffolds a repo's code-writer suite).**
Because code writers are repo-defined (their lockdown is language- and
repo-specific), apg distributes a **guide agent that helps a user create them**:
- `mode: primary` (user-invoked when setting up a codebase); the only file it
  writes is `.opencode/agents/*.md` in the target repo (`edit` scoped to
  `.opencode/agents/**`; read/glob/grep over everything; `question`; read-only
  bash; no build gates, no git mutation).
- **Detect the codebase graph-first** — languages present, build/lint/test
  files (`Makefile`, `go.mod`, `Cargo.toml`, `package.json`, `pom.xml`,
  `CMakeLists.txt`, `*.csproj`), test-tier conventions (test file extensions:
  `*_test.go`, `*.test.ts`, `*Test.java`, …), module layout via `apg_modules`,
  `AGENTS.md`/contributing docs.
- **Interview the user** (`question`, one at a time, multiple choice
  preferred) to fill the gaps: the canonical quality gate command; the unit /
  integration / e2e gate commands and how the tiers are split; git conventions
  (worktree branches, commit rules); which agents to generate and how strict
  the lockdown should be (deny-by-default paths).
- **Generate** the requested repo agents into `.opencode/agents/`, adapted
  from the platform's reference pattern: `implementer` (edits source only,
  `**/*_test.*` denied, the repo's build/test/lint gates allowed, git
  worktree/commit, repo-green contract); `unit`/`int`/`e2e`-test-implementer
  (their tier's `*_test.*` files only, tier gates, the test contract);
  `reviewer` (read-only + read-only gates + test-contract enforcement);
  optionally `coordinator` (dispatches the generated writers + the distributed
  `spec-writer`/`plan-writer`/`spec-review`/`plan-review`, drives phased-plan
  execution, merges worktrees). Each generated agent embeds the
  codebase-navigator non-negotiable graph-first rules.
- Re-running regenerates/updates the suite as the repo's conventions change.

## Design

### Schema (load.rs DDL additions)

New node tables:

```sql
CREATE NODE TABLE Spec(fqn STRING PRIMARY KEY, title STRING, goal STRING);
CREATE NODE TABLE Requirement(fqn STRING PRIMARY KEY, id STRING, title STRING, body STRING, feature STRING);
CREATE NODE TABLE Phase(fqn STRING PRIMARY KEY, number INT64, title STRING);
CREATE NODE TABLE Decision(fqn STRING PRIMARY KEY, id STRING, summary STRING);
CREATE NODE TABLE Future(fqn STRING PRIMARY KEY, kind STRING, target STRING);
CREATE NODE TABLE NonGoal(fqn STRING PRIMARY KEY, body STRING);
CREATE NODE TABLE AcceptanceCriterion(fqn STRING PRIMARY KEY, body STRING);
CREATE NODE TABLE VerificationItem(fqn STRING PRIMARY KEY, body STRING);
CREATE NODE TABLE Note(fqn STRING PRIMARY KEY, body STRING, kind STRING);
CREATE NODE TABLE Feedback(fqn STRING PRIMARY KEY, body STRING, status STRING, disposition STRING);
CREATE NODE TABLE Plan(fqn STRING PRIMARY KEY, title STRING, strategy STRING);
CREATE NODE TABLE PlanPhase(fqn STRING PRIMARY KEY, number INT64, title STRING, deliverable STRING);
CREATE NODE TABLE Task(fqn STRING PRIMARY KEY, title STRING, tier STRING, status STRING);
```

Rel tables (multi-pair `Contains` extended; new tables):

```sql
-- Contains gains: FROM Spec TO Requirement/Phase/Decision/NonGoal/AcceptanceCriterion/VerificationItem,
--                FROM Phase TO Requirement,
--                FROM Plan TO PlanPhase, FROM PlanPhase TO Task,
--                FROM PlanPhase TO AcceptanceCriterion, FROM PlanPhase TO VerificationItem
CREATE REL TABLE Details(FROM Note TO Module, FROM Note TO Function, FROM Note TO Struct, FROM Note TO File,
                         FROM Note TO Spec, FROM Note TO Requirement, FROM Note TO Phase, FROM Note TO Decision,
                         FROM Note TO NonGoal, FROM Note TO AcceptanceCriterion, FROM Note TO VerificationItem,
                         FROM Note TO Plan, FROM Note TO PlanPhase, FROM Note TO Task);
CREATE REL TABLE Reviews(FROM Feedback TO Module, FROM Feedback TO Function, FROM Feedback TO Struct,
                         FROM Feedback TO File, FROM Feedback TO Spec, FROM Feedback TO Requirement,
                         FROM Feedback TO Phase, FROM Feedback TO Decision, FROM Feedback TO NonGoal,
                         FROM Feedback TO AcceptanceCriterion, FROM Feedback TO VerificationItem,
                         FROM Feedback TO Future, FROM Feedback TO Plan, FROM Feedback TO PlanPhase,
                         FROM Feedback TO Task);
CREATE REL TABLE DependsOn(FROM Requirement TO Requirement);
CREATE REL TABLE Gates(FROM Phase TO Phase, FROM PlanPhase TO PlanPhase);
CREATE REL TABLE SpecDependsOn(FROM Spec TO Spec);
CREATE REL TABLE Anchors(FROM Requirement TO Function, FROM Requirement TO Struct, FROM Requirement TO File,
                         FROM Requirement TO Future,
                         FROM Task TO Function, FROM Task TO Struct, FROM Task TO File);
CREATE REL TABLE Implements(FROM Function TO Requirement, FROM Struct TO Requirement, FROM File TO Requirement);
CREATE REL TABLE Satisfies(FROM PlanPhase TO Requirement);
CREATE REL TABLE Builds(FROM Task TO Future);
```

`build_load_files`/`copy_from`/`write_graph_jsonl` gain matching columns,
COPY statements, and `Export` variants. Spec nodes have no `path`/`start`/`end`
or `code_type` (mirroring `Module`/`UnresolvedTarget`).

### Serialization format (`apg/specs/<project>.jsonl`)

```jsonl
{"type":"spec","fqn":"future/workitem-timer/spec","title":"Workitem Timer, Reconstructible Sessions, and the Node REST Gateway","goal":"..."}
{"type":"requirement","fqn":"future/workitem-timer/spec.A2","id":"A2","parent":"future/workitem-timer/spec","title":"OperatorService.Heartbeat RPC","body":"...","feature":"feature-a"}
{"type":"phase","fqn":"future/workitem-timer/spec.phase-1","number":1,"title":"Reworked Feature B operator side","parent":"future/workitem-timer/spec"}
{"type":"decision","fqn":"future/workitem-timer/spec.decision-A","id":"A","summary":"...","parent":"future/workitem-timer/spec"}
{"type":"future","fqn":"future/workitem-timer/gateway","kind":"service","target":"github.com/foundry/flow/platform/gateway","parent":"future/workitem-timer"}
{"type":"note","fqn":"future/workitem-timer/note-1","body":"...","kind":"background","parent":"future/workitem-timer"}
{"type":"contains","from":"future/workitem-timer/spec","to":"future/workitem-timer/spec.A2"}
{"type":"details","from":"future/workitem-timer/note-1","to":"future/workitem-timer/spec"}
{"type":"depends_on","from":"future/workitem-timer/spec.A2","to":"future/workitem-timer/spec.A1"}
{"type":"gates","from":"future/workitem-timer/spec.phase-3","to":"future/workitem-timer/spec.phase-2"}
{"type":"anchors","from":"future/workitem-timer/spec.A3","to":"github.com/foundry/flow/...reconcileRunning"}
{"type":"anchors","from":"future/workitem-timer/spec.G3","to":"future/workitem-timer/gateway"}
{"type":"spec_depends","from":"future/workitem-timer/spec","to":"future/sdk-session-standardisation/spec"}
{"type":"implements","from":"github.com/foundry/flow/...reconcileRunning","to":"future/workitem-timer/spec.A3"}
```

### Plan serialization (`apg/.trans/plans/<project>.jsonl`)

The plan is the bridge: `Satisfies(PlanPhase→Requirement)` links execution to
the spec, `Builds(Task→Future)` links tasks to the planned code they create.
`Gates(PlanPhase→PlanPhase)` carries the prerequisite chain.

```jsonl
{"type":"plan","fqn":"future/workitem-timer/plan","title":"Workitem Timer — Implementation Plan","strategy":"TDD core-first, dependency-layered. Test-tier routing: implementer edits source only; unit/int/e2e test implementers edit *_test.go only. Every phase leaves make verify green."}
{"type":"plan_phase","fqn":"future/workitem-timer/plan.phase-01","number":1,"title":"Reworked Feature B operator side","deliverable":"the two-class resume gate, waiting shapes, Cancelled terminal state (R1-R3, R7-R10)","parent":"future/workitem-timer/plan"}
{"type":"plan_phase","fqn":"future/workitem-timer/plan.phase-02","number":2,"title":"Node REST gateway","deliverable":"gateway platform service + hard endpoint enforcement (G1-G7)","parent":"future/workitem-timer/plan"}
{"type":"task","fqn":"future/workitem-timer/plan.phase-01.task-1","title":"two-class resume gate in ResumeWorkitem","tier":"source","status":"pending","parent":"future/workitem-timer/plan.phase-01"}
{"type":"task","fqn":"future/workitem-timer/plan.phase-02.task-1","title":"gateway reverse-proxy platform service","tier":"source","status":"pending","builds":"future/workitem-timer/gateway","parent":"future/workitem-timer/plan.phase-02"}
{"type":"contains","from":"future/workitem-timer/plan","to":"future/workitem-timer/plan.phase-01"}
{"type":"contains","from":"future/workitem-timer/plan.phase-01","to":"future/workitem-timer/plan.phase-01.task-1"}
{"type":"gates","from":"future/workitem-timer/plan.phase-02","to":"future/workitem-timer/plan.phase-01"}
{"type":"satisfies","from":"future/workitem-timer/plan.phase-01","to":"future/workitem-timer/spec.R1"}
{"type":"satisfies","from":"future/workitem-timer/plan.phase-02","to":"future/workitem-timer/spec.G3"}
{"type":"builds","from":"future/workitem-timer/plan.phase-02.task-1","to":"future/workitem-timer/gateway"}
{"type":"anchors","from":"future/workitem-timer/plan.phase-01.task-1","to":"github.com/foundry/flow/platform/operator/internal/rpc.OperatorServer"}
{"type":"details","from":"future/workitem-timer/note-2","to":"future/workitem-timer/plan.phase-01"}
```

### Plan execution (per-task checkout)

The plan is a **checklist executed task by task**. Each `Task` starts
`pending`; implementing it is `apg plan done <project> <task-fqn>`, and that
act **moves the task's portion of future work into the present**: every
`Builds(Task→Future)` is verified against the code graph (the `Future.target`
must exist — a task is not done until its code exists) and promoted (re-anchor
`Anchors(req→Future)` to the target code node, add
`Implements(code→requirement)`, retire the `Future` from the project JSONL);
then `status = done`. A task with no `Builds` edge (seams, Makefile gates,
tests) is a plain status flip. `undone` flips a task back to `pending` — it
does not recreate retired `Future`s (the code is already in the present);
undoing is a checklist correction, not a time machine.

`apg plan complete <project> <phase-n>` is the phase-level act: it **requires
every phase task `done`** (errors otherwise), then for each
`Satisfies(PlanPhase→Requirement)` idempotently adds the `Implements` edge from
the phase's built code (promoted futures' targets / task anchors) to the
requirement. Executing all tasks and completing all phases leaves no
unsatisfied `Future` under the project and every requirement `delivered` — the
plan has been executed *out of the graph* task by task, and the spec is
`implemented` (ready for `apg spec archive`).

The plan is the exact bridge the platform's `PLAN.md`/`PHASE_N.md` artifacts
already are: a phase's `Satisfies` = the deliverable column's R-numbers; its
`Gates` = the "Prerequisites" lines; its `Task`-`Builds`-`Future` = "this
phase creates this planned code"; its `Anchors` = files touched; its
`AcceptanceCriterion`/`VerificationItem` = the phase's ACs and executable
gates; its `Task.status` checklist = the implementation progress. Prose
(strategy, seam contracts, handoff) is carried by `Note`s.

### Future code nodes

A `Future` node is the placeholder for code a spec/plan says will exist but
does not yet. It is the first-class form of a pending anchor and the node that
the `future/<project>/plan` graph (plan tasks) references as a build target via
`Builds`. `kind` says what it will be (`function`/`struct`/`service`/`rpc`/
`endpoint`/`other`, declared by the author, never guessed); `target` is the
intended real FQN when implemented. The `future/` root keeps every planned
artifact in one namespace: `future/<project>/spec`, `future/<project>/plan`,
and the future code nodes themselves.

### Notes on any node (ownership + persistence)

A `Note` routes to a durable home by what its `Details` edge targets:

- **Spec/future notes** — the note is project-owned (fqn `future/<project>/note-<n>`)
  and serializes in the owning project's `apg/specs/<project>.jsonl`, because
  the note is the *source* of the `Details` edge, so the whole edge lives in the
  same project JSONL:

```jsonl
{"type":"note","fqn":"future/workitem-timer/note-9","body":"background on the gateway decision","kind":"background","parent":"future/workitem-timer"}
{"type":"details","from":"future/workitem-timer/note-9","to":"future/workitem-timer/spec"}
```

- **Code notes** — the note is repo-level (fqn `annotations/<n>`) and serializes
  in the **committed** `apg/notes/<module>.jsonl` — one file per owning module
  (the module that `Contains` the annotated code node, slugged; fallback
  `_root.jsonl`) — because it annotates current code and must be versioned with
  it:

```jsonl
{"type":"note","fqn":"annotations/9","body":"reviewer note on reconcileRunning","kind":"design"}
{"type":"details","from":"annotations/9","to":"github.com/foundry/flow/...reconcileRunning"}
```

`apg scan` reads `apg/specs/*.jsonl`, `apg/notes/*.jsonl`, and
`apg/.trans/plans/*.jsonl` (if present).
On rebuild, `Details` edges re-attach by FQN if the target exists. A code note
whose target a refactor renamed becomes drift (the target no longer resolves) —
reported by lint, never fatal. Notes on `Future` nodes are spec notes (their
target is `future/...`).

### Review cycle (feedback lifecycle)

`Feedback` is a review item in the writer↔reviewer cycle. fqn is
`future/<project>/feedback-<n>`; it routes to the artifact JSONL of its
`Reviews` target (spec/`Future` target → spec JSONL; plan/task/code target →
plan JSONL).

```jsonl
{"type":"feedback","fqn":"future/workitem-timer/feedback-1","body":"G3 must hard-enforce undeclared-path 404 before forwarding, not after","status":"open","parent":"future/workitem-timer"}
{"type":"reviews","from":"future/workitem-timer/feedback-1","to":"future/workitem-timer/spec.G3"}
```

The state machine is closed and separation-of-duties:

```
reviewer:  apg review add <target>          → status = open        (attached)
writer:    apg review action <f> --fix|--wont-fix → status = actioned (disposition = fixed|wont-fix)
reviewer:  apg review resolve <f>           → status = resolved    (accepted, terminal)
reviewer:  apg review reject <f>            → status = open        (reopened, disposition = rejected)
```

The **done gate** is derived, never asserted: an artifact is done only when
every `Feedback` reviewing it (or, for phases, its tasks) is `resolved`.
`apg spec archive` and `apg plan complete` refuse while any is `open`/
`actioned`; lint surfaces the rest as *under review*.

### Pending-anchor lifecycle

`apg spec add <project> future <name> --kind function|struct|service|rpc|endpoint|other --target <fqn>`
declares future code explicitly, creating
`Future {fqn: "future/<project>/<name>", kind, target}`. `apg spec anchor
<project> <req> future/<project>/<name>` then attaches
`Anchors(Requirement→Future)`. `--anchor`/`anchor` with any FQN that is neither
a resolved code node nor an existing `future/…` FQN is an **error** — Future
nodes are never auto-created (no guessing; a typo cannot masquerade as planned
code). On each rebuild, a `Future` whose `target` FQN now exists in the code
graph is **satisfied**; lint reports it and the author (or implementer) can
re-anchor to the real code node. Pending anchors are queried as
`(r:Requirement)-[:Anchors]->(f:Future)`.

### Future → present transition (`promote`)

The DB has two layers: the `future/` plan graph (Spec/Requirement/Phase/
Decision/Notes + `Future` nodes) and the code graph (Module/Struct/Function/
File, always current). "Moving into current" is a **re-link, never a rename**:

- A `Future` is born planned (`Anchors(req→Future)`); when its `target` code
  exists in the code graph it is **satisfiable**; `apg spec promote` performs
  the transition: re-point each `Anchors(req→Future)` to the real code node,
  add `Implements(code→req)` for each re-anchored requirement, and retire the
  `Future` (removed from the project JSONL, write-through).
- Requirements/Phases keep `future/<project>/spec.<id>` forever. State is
  derived: `planned` (anchors to a `Future` / no `Implements`) vs `delivered`
  (an `Implements` edge). A spec is `implemented` when every requirement is
  delivered.
- `apg spec archive <project>` retires the whole spec: its JSONL leaves active
  `apg/specs/` discovery (retained as the historical plan record) while
  `Implements` edges keep the delivered work queryable.

After full implementation: no unsatisfied `Future` under `future/<project>`,
every requirement has an `Implements` edge, and the spec graph is a fully-laced
overlay on the code graph — pending anchors became present code edges. That is
"the spec moved from planned future → current."

### Implementation tracking

`Implements` edges (added by `apg spec promote` for pending-anchored
requirements, or by the implementer for resolved work) make the spec graph the
tracking surface:

```cypher
MATCH (p:Phase)-[:Contains]->(r:Requirement)
WHERE NOT (r)<-[:Implements]-(:Function|:Struct)
RETURN p.number, r.fqn
```

"what's left in Phase N" is a one-line query; the `Implements` edges serialize
into the project's JSONL so status survives rebuilds. A requirement with an
`Implements` edge is `delivered`; a `Future` with no satisfying code is still
`planned`; a project with no unsatisfied `Future` and every requirement
delivered is `implemented`.

### Authoring CLI behavior

- `add` is an **upsert** by id: re-running the same `add` replaces the node and
  its outgoing edges (idempotent authoring, safe to re-run mid-session).
- `rm` removes the node and all incident edges.
- Every mutation writes the JSONL **before** the in-memory graph is considered
  updated (write-ahead), then re-ingests the spec into the live DB.
- Anchors are validated against the current code graph at authoring time;
  a stale/missing graph is reported (lint/counts) rather than guessed.

### Agent permission sketches

`spec-writer.md`:

```yaml
mode: subagent
hidden: true
permission:
  read: allow
  external_directory: ask
  edit:
    "*": deny
  apg_query: allow
  apg_find_symbol: allow
  apg_modules: allow
  apg_module_files: allow
  apg_module_structs: allow
  apg_file_units: allow
  apg_file_path: allow
  apg_methods: allow
  apg_struct: allow
  apg_callers: allow
  apg_callees: allow
  apg_uses: allow
  apg_unresolved: allow
  apg_hunk: allow
  apg_spec: allow
  apg_spec_requirements: allow
  apg_spec_phases: allow
  apg_spec_deps: allow
  apg_spec_anchors: allow
  apg_spec_unresolved: allow
  apg_spec_trace: allow
  apg_spec_init: allow
  apg_spec_add: allow
  apg_spec_anchor: allow
  apg_spec_link: allow
  apg_spec_rm: allow
  apg_spec_render: allow
  apg_review_action: allow
  question: allow
  bash:
    "*": deny
    "ls *": allow
    "find *": allow
    "rg *": allow
    "grep *": allow
    "pwd": allow
    "cd *": allow
```

`spec-writer.md` holds the spec authoring tools plus `apg_review_action` (it
responds to feedback) but **not** `apg_review_add`/`apg_review_resolve`/
`apg_review_reject` — it cannot attach or close review items.

`spec-review.md` — the mirror image: the read suite + `apg_review`,
`apg_review_add`, `apg_review_resolve`, `apg_review_reject`, `apg_review_list`.
It does **not** hold any `apg_spec_init/add/anchor/link/rm/promote/archive`
tool — it can attach and close feedback but cannot modify a spec node. Its
cycle with `spec-writer` is closed: neither side can complete it alone.

`plan-writer.md` mirrors `spec-writer.md` but allows the `apg_spec_*` **read**
tools plus the `apg_plan_*` authoring tools (`apg_plan_init`, `apg_plan_add`,
`apg_plan_link`) and the plan read/lifecycle tools (`apg_plan`,
`apg_plan_phases`, `apg_plan_tasks`, `apg_plan_render`) plus
`apg_review_action`; `edit` denied entirely. It does not hold
`apg_spec_add`/`apg_spec_init`/`apg_spec_promote`/`apg_spec_archive` — the
plan-writer authors plans, not specs.

`plan-review.md` mirrors `spec-review.md` for plan/task/code nodes (implementation
review), adding `apg_plan_phases`/`apg_plan_tasks` to see the checklist and
`apg_review`/`apg_review_add`/`apg_review_resolve`/`apg_review_reject` for
feedback; it holds no `apg_plan_init/add/link` authoring tools.

`agent-builder.md` is the exception that proves the rule: `mode: primary`,
`read`/`glob`/`grep` over everything, `question`, `todowrite`, read-only bash
and git-read commands, **no build gates, no git mutation** — and exactly one
`edit` grant: `.opencode/agents/**` (the repo agent files it scaffolds). It
holds no `apg_spec_*`/`apg_plan_*`/`apg_review_*` authoring tools (it does not
author specs or plans; it guides the user to create their code-writer agents).

`codebase-navigator.md` adds `task: allow` and spec/plan delegation sections.

## Error Handling

- **Spec FQN collision** — a residual collision between two distinct spec
  entities panics loudly at ingest (existing ingestor behavior), never a
  silent overwrite.
- **Dangling references** — `depends_on`/`gates`/`spec_depends`/`anchors`
  targets that don't exist at ingest are reported by `apg_spec_unresolved`
  (lint), not silently dropped. `depends_on` to an undeclared requirement id
  is a write-time error from `apg spec link`.
- **Spec drift** — an `Anchors` edge whose code node disappears after a
  refactor (FQN renamed/removed) is detected by lint and reported as drift.
  A `Future` whose `target` no longer matches its now-built code (the code FQN
  changed) is likewise flagged.
- **Pending anchors** — references to not-yet-built code are expected, never
  errors. They serialize as `Anchors→Future`; a `Future` whose target code now
  exists is marked satisfied (never an error). A `Future` is **never
  auto-created**: `--anchor`/`anchor` with an FQN that is neither a resolved
  code node nor an existing `future/…` FQN fails loudly, so a typo cannot
  masquerade as planned code.
- **Malformed/missing spec/plan JSONL** — a malformed record fails loudly at
  ingest (no partial spec/plan); a missing project JSONL is simply absent (no
  spec/plan). A malformed committed `apg/notes/*.jsonl` likewise fails loudly.
- **Code-note drift** — a `Details` edge whose code-node target disappears after
  a refactor is reported by lint; the note stays in `apg/notes/*.jsonl` and
  re-attaches if the FQN returns.
- **Write-ahead safety** — a crash between mutation and re-ingest loses nothing
  (the JSONL is written first); re-running authoring is idempotent.
- **`promote` failures** — `apg spec promote` with an unresolvable
  `Future.target` errors (stale graph or target mismatch); it never guesses,
  never auto-promotes, and never renames FQNs. `apg plan done` inherits this:
  a task whose `Builds` `Future.target` does not resolve errors, so a task
  cannot be marked done until its code actually exists in the present.
  `apg plan complete` requires every phase task `done` (a phase with pending
  tasks cannot be completed).
- **Plan phase ordering** — `apg plan add phase --prereq` references a phase
  number that doesn't exist is a write-time error; a `Gates` cycle between
  phases is detected by lint (`apg_plan_phases`) and rejected, not silently
  accepted.
- **Review-cycle integrity** — the cycle is enforced by tool permissions (a
  writer cannot `resolve`/`reject`, a reviewer cannot `action`), and by the
  state machine (`resolve`/`reject` on an `open` item errors; `action` on a
  `resolved` item errors). `apg spec archive` / `apg plan complete` refuse
  while any `Feedback` on the artifact is `open`/`actioned` — an artifact is
  never *done* with unresolved feedback. A `Feedback` whose `Reviews` target
  disappears (node deleted) is drift, reported by lint.
- **Stale/missing code graph** — authoring needs the code graph to resolve
  anchors; if the gate counts are zero or a query errors, `apg spec` reports
  the stale graph and does not guess FQNs (the spec-writer falls back to
  read/glob/grep and notes it, matching the platform agent's rule).
- **Reserved words** — `end` is quoted as `` n.`end` `` in queries, as today.

## Verification

1. `cargo test` passes. New unit tests cover: spec/plan JSONL round-trip
   (serialize → deserialize → identical graph), render producing every required
   section, `is_apg_source_dir` with all six agents present.
2. Integration fixture: create a small Go project + a `apg/specs/<project>.jsonl`
   with requirements, phases, deps, resolved + pending anchors, and notes, plus
   a `apg/.trans/plans/<project>.jsonl` with phases, tasks,
   `Satisfies`/`Builds`/`Gates` edges, and a committed `apg/notes/<module>.jsonl`
   with a note on a code node; run `apg scan`; assert spec/plan nodes and edges
   and code notes query correctly and `graph.jsonl` contains the records.
3. Anchor lifecycle: pending anchor becomes an `Anchors→Future` edge; after
   adding the referenced code and re-scanning, the `Future` is reported
   satisfied; `apg spec promote` re-points the anchor to the real code node,
   adds the `Implements` edge, and retires the `Future` (the project JSONL no
   longer contains it). `apg plan done <task>` performs the same transition
   for every `Builds(Task→Future)` on the task; `apg plan complete` requires
   all phase tasks `done` and adds `Implements` for each
   `Satisfies(PlanPhase→Requirement)`.
4. Render: `apg spec render <project>` produces a valid SPEC-style markdown
   (Goal, Background, Scope, Non-Goals, Requirements, Design, Error Handling,
   Verification, Acceptance Criteria, Open Questions) with feature-grouped
   requirements and explicit `Consumes`/`Anchors` lines.
5. `apg init` installs all six agents into `~/.opencode/`; `spec-writer.md`
   and `plan-writer.md` have no `edit` permission and no review-add/resolve/
   reject tools; `spec-review.md`/`plan-review.md` hold no authoring tools;
   `agent-builder.md` edits only `.opencode/agents/**`; `codebase-navigator.md`
   has `task: allow` and spec/plan reading/delegation sections; re-running
   `apg init` is idempotent.
6. Existing code-graph suite tools still pass on a repo with and without specs
   (Contains extended, not redefined).
7. `cargo build` and the frontend build (`build.rs`) stay green.
8. Agent prose check: given a small prose spec (platform-template `SPEC.md`),
   the `codebase-navigator` reads it and proposes a spec graph structure
   (requirements by feature, phases with gates, future nodes, notes, deps,
   anchors); the `spec-writer` confirms the proposal against the graph and
   materializes it into the spec JSONL; the `plan-writer` then authors a plan
   graph (`Plan`/`PlanPhase`/`Task` with `Satisfies`/`Builds`/`Gates`) from
   the spec graph and materializes it into `apg/.trans/plans/<project>.jsonl`.
9. Task checklist round-trip: tasks serialize with `status`, flip
   `pending → done` via `apg plan done` (surviving a rebuild from the JSONL),
   `done → pending` via `apg plan undone`, and a `done` task's `Builds` future
   is absent from a fresh scan (promoted and retired).
10. Review-cycle check: `spec-review` attaches feedback (`open`); the writer
    actions it (`actioned`); `resolve` closes it (`resolved`); `reject` reopens
    it. `apg spec archive`/`apg plan complete` refuse while any feedback is
    open/actioned; an artifact with only `resolved` feedback is done. The
    writer cannot resolve and the reviewer cannot action (permission-enforced).
11. Agent-builder check: given a small repo (Go or other) with known build
    gates, the `agent-builder` detects the stack graph-first, interviews the
    user, and writes `implementer.md` + the test-implementers + `reviewer.md`
    (+ optional `coordinator.md`) into `.opencode/agents/`, each with a
    permission block scoped to the detected layout and gates, test-files
    denied for the implementer, and the codebase-navigator rules embedded;
    re-running updates them idempotently.

## Acceptance Criteria

1. `apg spec init/add/anchor/link/rm` round-trip: JSONL written, live DB
   updated, and a subsequent `apg scan` rebuild reproduces an identical spec
   graph (no loss).
2. Anchors resolve to real code nodes; pending anchors are recorded as
   `Anchors→Future` and reported satisfied once the target code lands;
   `apg spec promote` performs the transition (re-anchor + `Implements` +
   retire `Future`) and `apg spec archive` retires a fully-implemented spec
   from active discovery.
3. `Future` nodes (kind + target) round-trip through the JSONL and query like
   any node; the `future/` root hosts `spec`, `plan`, and future code FQNs
   without collision.
4. `apg spec render` derives a full template-style markdown spec from the
   graph; the render is never editable back into the graph.
5. `apg scan` auto-discovers `apg/specs/*.jsonl`, `apg/.trans/plans/*.jsonl`,
   and `apg/notes/*.jsonl`; `graph.jsonl` includes spec, plan, and note records.
6. `apg_spec_unresolved` reports dangling refs, orphan requirements, ACs
   without a covering requirement, pending anchors, spec drift, and **open/
   actioned feedback** on an artifact; `apg_plan_phases` reports unsatisfied
   requirements (no `Satisfies`), `Gates` cycles, phases with no tasks, and
   tasks *under review* (done with unresolved feedback).
7. The distributed `spec-writer`/`plan-writer` authors and `spec-review`/
   `plan-review` reviewers form the closed cycle with no file write access:
   writers hold no review-add/resolve/reject tools, reviewers hold no
   authoring tools; `codebase-navigator` delegates spec/plan creation to the
   writers (task permission present).
8. `codebase-navigator` reads a provided spec (prose or requirements
   description) and proposes a faithful spec graph structure; the spec-writer
   confirms it against the code graph and materializes it — no graph authoring
   happens in the navigator itself.
9. `apg plan done <task>` moves that task's portion of future work into the
   present: its `Builds` futures are promoted (`Implements` added, `Future`s
   retired) and `status` flips to `done`; `apg plan complete <phase>` requires
   all phase tasks done **and all phase/task feedback resolved** and marks the
   phase's `Satisfies` requirements delivered; executing all tasks and
   completing all phases leaves every requirement delivered and the spec
   archivable. A task whose `Builds` future has no code cannot be marked done
   (errors).
10. Existing code-graph behavior is unchanged on repos without specs.
11. Notes route correctly: a note on a spec/`Future` node persists in
    `apg/specs/<project>.jsonl`; a note on a code node persists in the
    committed `apg/notes/<module>.jsonl` and survives a scan rebuild.
12. Feedback routes correctly and round-trips: spec-review feedback persists in
    `apg/specs/<project>.jsonl`, plan/implementation feedback in
    `apg/.trans/plans/<project>.jsonl`, both survive a scan rebuild, and a
    `Feedback` whose target was deleted is reported as drift, never fatal.
13. `agent-builder` (distributed, `mode: primary`) scaffolds a repo's
    code-writer suite into `.opencode/agents/` with a single `edit` grant
    (`.opencode/agents/**`), tailoring permissions to the detected stack and
    gates — no distributed code-writer agents exist; every repo's writers are
    generated by it.

## Open Questions

None. (`apg spec render` defaults to `apg/.trans/specs/<project>.md` with an
`--out` override; every other decision above is settled.)