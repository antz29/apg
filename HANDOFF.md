# Love letter to my future compacted self

> **Status: ALL FIVE PHASES DONE (Aug/Sep 2026).** Phases 1–5 implemented and
> committed (`33c5681` → `938bb0a` → `b31beb6`, plus `621bf26`). `cargo test` =
> 29 green, clippy clean, `cargo build --release` clean. The only remaining
> items are the two interactive `human` checks from PLAN Phase 05 (agent prose
> checks + agent-builder scaffold demo) and the closing dogfood exercise
> (represent this SPEC.md + PLAN.md in the graph). If you're here after a
> compaction, skip to "Where we are" and don't redo anything — the system is
> built and verified.

Hey — if you're reading this, the context got compacted (or a session died) and
you need to pick up mid-build. This is everything you need. Breathe, read
`SPEC.md` (§1) and `PLAN.md`, then continue.

## What we're building

The **graph-native specs** feature for **apg** (`/Users/jledrew/apg`): specs,
plans, review feedback, and agent distribution live IN the code graph (a
LadybugDB database), authored via a CLI, serialized as committed JSONL, and
distributed to opencode as language-agnostic agents. The design is final in
`SPEC.md` (R1–R29, no open questions) and `PLAN.md` (5 phases). **These were
built in prose first — the graph tooling is what makes them graph-native later.**

## Where we are (start here)

**ALL FIVE PHASES ARE DONE and committed** (on `main`):
- `33c5681` — schema + serialization (Phase 1+2): 13 spec/plan node kinds, 9 rel
  tables, pending-anchor reconciliation, committed `apg/` layout + scan re-ingest.
- `621bf26` — the CLI (Phase 3): `apg spec|plan|review` with write-through.
- `938bb0a` — the suite tools (Phase 4): 30 `apg_spec_*`/`apg_plan_*`/`apg_review*`
  opencode tools, `apg.ts` helpers (runCli/projectOf/csvToRows/resolvesInCode),
  code-note write-through R5 fix.
- `b31beb6` — the agents (Phase 5): five new distributed agents + navigator
  `task`/delegation/reading, `apg init` six-agent install + ownership + `.gitignore`
  scaffold, AGENTS.md/README.md layout refresh, `is_apg_source_dir` tests.

`cargo test` = **29 passing**, `cargo clippy --all-targets` = **0 warnings**,
`cargo build --release` clean. Do NOT skip running both after any change.

**REMAINING (interactive-only, blocked on a human session):**
- PLAN Phase 05 `human` items: agent prose checks (navigator reads a prose spec
  → proposes graph structure; spec-writer materializes; plan-writer authors a
  plan; a review cycle attaches/actions/resolves/rejects; agent-builder
  scaffolds a sample repo's writer suite).
- The closing dogfood: represent this very `SPEC.md` + `PLAN.md` in the graph
  via the spec-writer → spec graph, plan-writer → plan graph.

## The layout (migrated, no more `.apg`)

- Committed `apg/` at repo root: `apg/config.json`, `apg/specs/<project>.jsonl`,
  `apg/notes/<module>.jsonl` (one per owning module, `_root.jsonl` fallback).
- Gitignored `apg/.trans/`: `db.lbug`, `graph.jsonl`, `apg-frontend.log`,
  `plans/<project>.jsonl`, rendered markdown.
- `apg scan` runs from inside `apg/.trans/`; spec/note/plan JSONLs are
  auto-discovered and re-ingested after code.
- FQN roots: spec = `future/<project>/spec`, plan = `future/<project>/plan`,
  future code = `future/<project>/<name>`. Feedback = `future/<project>/feedback-<n>`.

## Key modules (src/)

- `graph.rs` — NodeKind (18 kinds), `Node` with optional spec/plan fields,
  `Graph` with 9 spec/plan edge sets (`details`, `reviews`, `depends_on`,
  `gates`, `spec_depends`, `anchors`, `implements`, `satisfies`, `builds`).
- `schema.rs` — the `Record` enum (unified JSONL, Serialize+Deserialize).
- `ingest.rs` — FQN rendering, streaming ingest, `reconcile_pending_anchors`
  (dangling requirement anchors → synthesized Future, R10).
- `load.rs` — parquet load files, `create_schema` (18 node tables, 10 rel
  tables), `copy_from`, `Export` + `write_graph_jsonl`.
- `specs.rs` — serialization: read/write JSONL, discovery, note routing,
  `find_apg_root`/`find_or_create_apg_root`.
- `artifacts.rs` — **the write-through core**: `ArtifactDb`, Cypher MERGE
  upserts, `reingest_project`.
- `spec_cmd.rs` / `plan_cmd.rs` / `review_cmd.rs` — the CLIs.

## Hard-won lbug (LadybugDB) gotchas — READ BEFORE TOUCHING

1. **`Connection` borrows `Database`** (self-referential). `ArtifactDb` owns the
   `Database` and creates a fresh `Connection` per call via `conn()`/`q()`.
   Do NOT `Box::leak` the Database to store both — it pins the WAL file and
   breaks re-opening the same db in-process.
2. **Edge creation must be two-variable MATCH + MERGE rel**, NOT pattern MERGE:
   `MATCH (a:La {fqn:'x'}), (b:Lb {fqn:'y'}) MERGE (a)-[:R]->(b)`. The one-shot
   `MERGE (a:La {fqn})-[:R]->(b:Lb {fqn})` re-attempts endpoint creation when
   endpoints already exist → "duplicated primary key" error.
3. **The `number` column is INT64** — in MERGE SET emit `n.number = 1`, never
   `'1'` ("Implicit cast is not supported" binder error).
4. **One file-DB connection at a time** — if `ArtifactDb` is alive when
   `reingest_project` runs, the second `Database::new` fails silently (warnings
   are swallowed). Scope/drop validation DBs before write-through. This bit us
   in tests; it's now scoped everywhere.
5. Unlabeled `MATCH (n {fqn})` works for existence/`count(*)`, but rel creation
   needs labeled endpoints. `labels(n)` output is unreliable for label lookup —
   use per-label `MATCH (n:Label {fqn})` existence checks.
6. `apg query`/tools: `end` is reserved (backtick it). `count(*)` preferred.
7. **Tests run in parallel + share temp dirs** → any new integration test must
   use a per-test unique dir name, and DB-file tests must scope `ArtifactDb`
   handles (see `spec_cmd::tests::fixture_layout(name)`).

## How the write-through works (R5)

Every CLI mutation: load the project JSONL → apply change → write JSONL → 
`reingest_project` = `DETACH DELETE` all `future/<project>/…` nodes → re-MERGE
the spec + plan + notes records (code graph untouched). `promote_future`
(re-anchor + Implements + retire Future) is shared by `apg spec promote` and
`apg plan done`. `apg plan complete` on the final phase retires the plan (drops
its JSONL — plans are transient by design, R22).

## Phase 4 — Suite tools (R12/R16) — **DONE** (938bb0a)

New TypeScript tools in `.opencode/tools/` (install via `SUITE_TOOLS` in
`src/main.rs`, shared plumbing in `.opencode/lib/apg.ts`):
- Read/query: `apg_spec`, `apg_spec_requirements`, `apg_spec_phases`,
  `apg_spec_deps`, `apg_spec_anchors`, `apg_spec_trace`.
- Lint: `apg_spec_unresolved` (dangling refs, orphan reqs, ACs w/o covering req,
  unsatisfied Futures = `Future.target` not in graph, spec drift, open feedback).
- Author: `apg_spec_init/add/anchor/link/rm` (wrap the CLI via `apg spec …`).
- Lifecycle: `apg_spec_promote`, `apg_spec_archive`.
- Review: `apg_review` (+add/action/resolve/reject) → wraps `apg review …`.
- Plan: `apg_plan`, `apg_plan_phases`, `apg_plan_tasks`, `apg_plan_complete`,
  `apg_plan_render`, `apg_plan_init/add/link`, `apg_plan_done/undone`.
- `apg_spec_render` (R16 markdown).
- `apg.ts` discovery already updated for `apg/.trans/db.lbug` (Phase 2 did it).
- Verify: run every tool against a scanned fixture; smoke test in the apg repo
  itself after `apg scan /Users/jledrew/apg`.

## Phase 5 — Agents + `apg init` (R13/R14/R15/R24/R26/R27/R28/R29) — **DONE** (b31beb6)

Six distributed agents, single-sourced from `.opencode/agents/*.md` via
`include_str!` consts, installed by `apg init` into `~/.opencode/agents/`:
`codebase-navigator` (already exists — add `task: allow` + delegation +
spec-reading, R14), plus NEW `spec-writer`, `spec-review`, `plan-writer`,
`plan-review` (hidden subagents, NO file writes, apg tools + read suite +
`question` + read-only bash), and **`agent-builder`** (mode primary, the ONLY
write grant — `edit` scoped to `.opencode/agents/**`; detects repo stack,
interviews, scaffolds the repo's own code-writer agents).

`src/main.rs` to update: 5 new `include_str!` agent consts, `cmd_init`
install/update counts, `is_apg_source_dir` agents list → the 6-agent set,
`remove_legacy_project_install` also removes the 5 new files, `.gitignore`
scaffold for `apg/.trans/` (add line, don't touch others). Also update
`AGENTS.md` (it still documents the old `.apg/` layout — stale since Phase 2).

## After Phase 5

- **Dogfood**: represent this very `SPEC.md` + `PLAN.md` in the graph
  (spec-writer → spec graph, plan-writer → plan graph) — the closing proof.
- Update `AGENTS.md` to the `apg/` layout (overdue).
- Consider updating the opencode suite descriptions that still say `.apg/`.

## Style / behavior reminders (the user's directives)

- **Work from SPEC.md/PLAN.md; they are prose, final, and the source of truth.**
- The user gets frustrated by premature implementation — design/prose first,
  implement only when asked. Commit only when explicitly asked.
- Keep responses concise (CLI, no fluff). Run `cargo test` + clippy after every
  change; leave each phase committed and green.
- The repo's house style: concise commits, phases land atomic-ish but each
  leaves the build green.

## State at handoff

- `cargo test` 29/29 green, clippy clean, release build clean. 4 feature
  commits on `main` (`33c5681`, `621bf26`, `938bb0a`, `b31beb6`).
- `~/.opencode/agents/` holds the installed six agents; `apg init` idempotent.
- Fixture (scratch): `/var/folders/ff/x5n4qwb54c3dltyhgn7gl7440000gn/T/opencode/apg-fixture`
  — a Go project with a scanned db + fixture/lifecycle/2/3 spec projects used
  for e2e. Safe to recreate with `apg scan`.
- Tool smoke harness: `/var/folders/ff/x5n4qwb54c3dltyhgn7gl7440000gn/T/opencode/tool-smoke`
  (smoke.ts read tools, mutate.ts author wrappers). Uses `~/.opencode/node_modules`
  for the plugin import; PATH must point at `target/debug` first.

Remaining: the interactive human agent checks + the dogfood exercise (see
"Where we are"). Take a well-earned break. 🚀