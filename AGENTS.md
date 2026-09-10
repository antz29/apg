# APG Agent Guide

## Pipeline

Scanner (per language) → Rust ingestor → `apg/.trans/db.lbug` + `apg/.trans/graph.jsonl`.

- The **scanner** (Go: `src/golib/main.go`, Java: `src/javalib/CallGraphBuilder.java`,
  C++: `src/cpplib/main.cpp`, Rust: `src/rustlib/src/main.rs` — a standalone
  `rustfrontend` binary built on rust-analyzer's `ra_ap_*`-era engine crates,
  pulled from the rust-analyzer repo at a pinned release tag, TypeScript:
  `src/tslib/scanner.mjs` — a Node script using the official `typescript`
  compiler API, npm-installed with a committed `package-lock.json`, C#:
  `src/csharplib/Program.cs` — a standalone single-file `csharpfrontend` binary
  built on Roslyn `Microsoft.CodeAnalysis.CSharp`) parses a codebase and streams
  one JSON object per
  line to stdout — the **unified JSONL schema** (see `SPEC.md` §2). It emits
  *facts only*: declarations, references, edges. It never computes FQNs and
  never does graph assembly.
- The **ingestor** (`src/ingest.rs`) spawns the scanner, resolves identity
  (canonical FQN), builds the graph, bulk-loads LadybugDB, and writes
  `graph.jsonl` as the export. `apg/.trans/db.lbug` is the query index;
  `apg/.trans/graph.jsonl`
  is the self-contained export artifact (canonical FQNs, no opaque ids).
- Build: `build.rs` compiles the frontends (`gcc`/`g++` tree-sitter for C++,
  `go build` for Go, `javac` for Java, `cargo build` for the Rust frontend —
  `src/rustlib`, a separate Cargo project, pinned to a rust-analyzer release
  tag, `npm ci` for the TypeScript frontend — `src/tslib`) and stages them to
  `target/<profile>/frontends`. Run a scan with `apg scan <dir>` (or the
  `apg_scan` tool). `apg` resolves frontends at runtime relative to the binary
  (`<exe_dir>/frontends` or `<exe_dir>/../libexec/frontends`) or via
  `APG_FRONTEND_DIR`. `APG_BUILD_FRONTENDS` (comma-separated: `go`, `java`,
  `cpp`, `rust`, `ts`, `csharp`; `0` to skip) limits what build.rs compiles.

## Project flow (the verified pattern)

A change-set is a **project** = a git branch + its worktree. The verified
pattern — visible to the next feature's agents without reading the suite
internals:

1. **Start from main.** The navigator runs `apg project start <name>` from the
   **main checkout** (never inside a worktree). The binary branches off the
   repo's default branch, creates the worktree at `<main>/apg/.worktrees/<name>`,
   auto-scans it (worktree + branch + branch DB in one command), and **prints
   the worktree path**. Idempotent only inside that same project; every
   collision is a hard refuse.
2. **Operate in-worktree.** Sessions and subagents run with **cwd inside the
   printed worktree path**. Suite-tool walk-up discovery finds the worktree's
   **own** `apg/` (its layout + branch DB), so the tools work unchanged.
   **Main is never a mutation place** — durable and transient mutations refuse
   outside a project context.
3. **Author the durable spec tiers** via `apg node add|rm <layer> <type>
   <name>` / `apg edge add|rm <kind> <from> <to>` into `apg/layers/` — one
   JSON file per node in the **six-layer tree** (requirements / domain /
   solution / implementation / global durably; plans transient). FQN =
   `<layer>.<type>.<name>`, the **file name IS the identity**, no project
   prefix. Node files carry paired `out`/`in` edges (both endpoint files,
   validated at scan); constraints are **prose** — structure/references are
   checked at write time, satisfaction is by review. One logical mutation
   writes all affected files and **auto-commits once** on the project branch.
4. **Plan and review are transient**: `apg/.trans/plans/<project>.jsonl` plus
   the `.trans/<tier>/<project>.jsonl` feedback mirrors — never committed,
   they die with the branch (the node files persist). A plan task names its
   verb + target: `creates` (a planned node), `modifies`/`deletes` (code that
   must already resolve), `renames`/`moves` (`--fqn` source + `--to`
   destination).
5. **Verify, then merge.** `apg plan verify <project>` is the pre-merge
   coherence gate — every planned node realized, all feedback resolved,
   derived solution coverage holds — and prints the merge handoff.
   `apg project merge <name>` from the main checkout runs verify → merge →
   unguarded main rebuild. Cleanup deletes no branch; push/tag remain human.
6. **Version gate + `apg init`.** `apg/config.json` carries the
   binary-managed `version` field; `apg scan`/`apg project start` **block**
   unless the layout and the binary share major.minor. **`apg init` is the
   upgrade act**: re-run it idempotently to write the current version, scaffold
   `apg/.worktrees/` + its `.gitignore` entry, and update the installed suite.

The suite agents (`codebase-navigator`, `spec-writer`, `plan-writer`,
`spec-review`, `plan-review`, `agent-builder`) hold the operational detail.

### CLI

The project builds a single `apg` binary (package `apg`, was `java_apg`):

- `apg init [dir]` — create `apg/` (committed `config.json` carrying the
  binary-managed `version` field + gitignored `.trans/`), scaffold the repo
  `.gitignore` for `apg/.trans/` and `apg/.worktrees/` (added if
  missing, other lines untouched), and install (or update, where contents
  differ) the opencode apg tool suite into `~/.opencode/tools/` +
  `~/.opencode/lib/` plus the **six distributed agents** — `codebase-navigator`,
  `spec-writer`, `plan-writer`, `spec-review`, `plan-review`, `agent-builder` —
  into `~/.opencode/agents/`. Project-specific implementer/reviewer agents are
  installed into the project's `.opencode/agents/` by the **`agent-builder`**
  agent, never by init. If the project's `.opencode/` holds files that duplicate
  the installed suite (`~/.opencode/`), init prints a **loud warning** listing
  them — it never deletes anything.
- `apg scan [dir] [--language L[,L...]] [--exclude-path G]* [--module M]* [--no-build-scripts]
  [blacklist...]`
  — run the pipeline; writes `apg/.trans/db.lbug`, `apg/.trans/graph.jsonl`,
  `apg/.trans/apg-frontend.log`. `--language` accepts one or more comma-separated
  languages; when omitted, `apg` auto-detects **every** language present and
  scans them all, merging their graphs into one database (a multi-language repo
  like a Go backend + TS frontend gets a single `apg/.trans/db.lbug`). Each frontend's
  opaque ids are namespaced per language (`--id-prefix`), and the ingestor
  classifies `code_type` and renders FQNs per record (a `lang_switch` control
  record precedes each frontend's stream). `--no-build-scripts` is Rust-only
  (skip cargo build scripts and the proc-macro server); the Rust frontend
  requires a Cargo manifest (C++ tolerates bare dirs; Rust scans nothing
  without one; TS needs a `package.json`/`.ts`/`.tsx` sources, and `node_modules`
  is always skipped).
- `apg query [--json] "<cypher>"` — read-only Cypher over `apg/.trans/db.lbug`
  (found by walking up from cwd); CSV with header row by default, `--json` for
  JSON rows.
- `apg node <sub> …` / `apg edge <sub> …` — the durable mutation surface:
  `node add|rm <layer> <type> <name>` writes/removes one node file under
  `apg/layers/` (the file name is the identity); `edge add|rm <kind> <from>
  <to>` writes **both** endpoint files in one atomic, auto-committed mutation
  (the out half in the source's file, the matching in half in the target's).
  Guarded — refuses outside a project worktree.
- `apg plan <sub> …` — the phased execution plan (transient, serialized to
  `apg/.trans/plans/<project>.jsonl`, branch-local): `init`, `add`
  (phase/task/planned), `link`, `done`/`undone` (assertion-only), `note` (task notes), `complete`
  (milestone-only), `render`, `verify` (the pre-merge coherence gate; the old
  `apply` was renamed — the binary applies nothing).
- `apg project <sub> …` — project contexts (worktrees, git2-operated):
  `start <name>` (from the main checkout: worktree + branch + branch DB off
  the default branch, prints `apg/.worktrees/<name>`), `merge <name>` (from
  the main checkout: verify gate → merge → unguarded main rebuild).
- `apg review <sub> …` — the closed writer↔reviewer feedback cycle over the
  transient `.trans` mirrors: `add`, `action`, `resolve`, `reject`, `list`.
- `apg --version`, `apg --help`.

`apg init` also installs the **apg opencode tool suite** into the user-level
`~/.opencode/` (single-sourced from this repo's `opencode-suite/`, embedded in
`src/main.rs` via `include_str!`): `apg_scan`, `apg_query`, plus curated
abstractions over common lookups — `apg_find_symbol`, `apg_modules`,
`apg_module_files`, `apg_module_structs`, `apg_file_units`, `apg_file_path`,
`apg_methods`, `apg_struct`, `apg_callers`, `apg_callees`, `apg_uses`,
`apg_unresolved`, `apg_hunk` — and the project/spec/plan/review suite:
`apg_project` (start/verify/merge), `apg_node` / `apg_edge` (durable
node-file mutations), `apg_plan` (+ phases/tasks/verify/init/add/link/done/
undone/note/complete/render), `apg_review` (+ add/action/resolve/reject).
Shared plumbing
lives in `~/.opencode/lib/apg.ts`
(root discovery, `apg query`/`apg project`/`apg node`/`apg edge`/`apg plan`/`apg review` subprocess,
Cypher literal escaping). All suite
tools take an optional `codeType` (default: all code); exact-FQN tools hint
when a lookup comes up empty (overloads carry `(params)` suffixes).

The `apg` binary is brew-installable via split formulae (tap
`https://github.com/antz29/apg.git`): `scanner` (the binary), plus six frontend
formulae — `apg-go`, `apg-java`, `apg-cpp`, `apg-rust`, `apg-ts`, `apg-csharp` —
each dropping its artifacts into `$(brew --prefix)/share/apg/frontends` (the
`scanner` formula's `bin/apg` wrapper sets `APG_FRONTEND_DIR` to that dir).
On Linux there's a `curl | sh` installer (`install.sh` — installs the base
`apg` scanner binary and/or separate language frontends to `/usr/local` or `--user`'s
`~/.local`, layout `bin/apg` + `libexec/apg/`); the linux-release workflow
builds individual packages (`apg-linux-*.tar.gz`, `apg-<lang>-linux-*.tar.gz`)
plus `sha256sums.txt` per tag.
In the repo itself, run it via `cargo run -- scan <dir>` or
`target/debug/apg scan <dir>`.

## Deploying a release (cutting a tag)

Releases are cut by pushing an annotated `vX.Y.Z` tag; `bottle.yml` (ARM bottle)
and `linux-release.yml` (x86_64/aarch64 tarballs) build and publish on that tag
push and create the GitHub release.

**Order matters — get this wrong and the bottles are the previous version.**
The Linux job checks out the tag and builds its source, so the tarballs are
always correct. The **bottle** job does *not* build from the tag: it builds via
the Homebrew tap formulae on `main`, so the formulae must already point at the
new version *before* the tag is pushed. Tagging first (with the formulae still
on the old version) ships stale `OLD-version` bottles — the v0.10.3 mistake.

Forward release (the `scripts/release.sh <version>` helper automates steps 4–5):

1. **Gate green**: `cargo test` passes (not just `cargo check` — the
   release-version guard tests only run under `cargo test`, and a stale
   `RELEASE_VERSION` literal ships the release HEAD red).
2. **Bump the version** in `Cargo.toml` **and** `Cargo.lock`
   (`version = "X.Y.Z"`).
3. **Commit the release content** (the version bump + whatever ships in it).
   This commit is the **release HEAD**.
4. **Repoint all 7 formulae** (`Formula/scanner.rb`, `apg-go`, `apg-java`,
   `apg-cpp`, `apg-rust`, `apg-ts`, `apg-csharp`): `tag:` → `vX.Y.Z`,
   `revision:` → the release-HEAD SHA, `root_url` →
   `releases/download/vX.Y.Z`, and bump each `rebuild N` by 1. Commit as
   *"Point formula revisions at the vX.Y.Z release HEAD"*.
5. **Annotated tag** pointing at the **release HEAD** (not the
   formula-revision commit): `git tag -a vX.Y.Z -m "apg X.Y.Z" <release-sha>`.
6. **Push (human-approved)**: `git push origin main` then
   `git push origin vX.Y.Z`. CI builds + creates the release; the bottle bot
   then auto-commits *"Update bottle sha256s for vX.Y.Z"* to `main`.
7. **Verify the assets are the new version**: `gh release view vX.Y.Z` must list
   `*-X.Y.Z.arm64_sonoma.bottle.*.tar.gz` (NOT the previous version) plus the
   `apg-linux-*` tarballs. If the bottles show the old version, you tagged
   before step 4.

Repairing stale bottles (tag was pushed before the formula repoint — the 0.10.3
case): after the formula-revision commit pointing at the released version lands
on `main`, re-dispatch the bottle job — `gh workflow run bottle.yml --ref main`
(or GitHub UI → Actions → "Build and publish bottles" → Run workflow) — then
`brew bottle --merge` / the bot commits the new sha256s. The tag and release
stay put; only the bottle assets get rebuilt/uploaded.

`scripts/release.sh` automates steps 4–5: it verifies the version is already
bumped and the tree is clean, rewrites every `Formula/*.rb`, commits the formula
revisions, and creates the annotated tag at the release HEAD. It **never
pushes** — push/tag remain human-approved acts (it prints the exact commands).

### Unified JSONL schema (abridged)

Node records:

```jsonl
{"type":"module","fqn":"github.com/foundry/flow"}
{"type":"file","path":"/abs/store.go","parent":"github.com/foundry/flow","start_line":1,"end_line":142}
{"type":"struct","id":"n12","parent":"...v1","name":"Error","path":"/abs/error.go","start":12,"end":300,"start_line":12,"end_line":45}
{"type":"function","id":"n13","parent":"...Store","name":"ComputeContentHash","params":["[]byte","int"],"file":"/abs/store.go","path":"/abs/store.go","start":1,"end":99,"start_line":34,"end_line":99}
{"type":"unresolved","fqn":"fmt.Errorf","category":"stdlib"}
```

Edge records:

```jsonl
{"type":"contains","from":"n12","to":"n13"}
{"type":"calls","from":"n13","to":"n14"}
{"type":"uses","from":"n13","to":"n12"}
{"type":"unresolved_call","from":"n13","to":"fmt.Errorf","target_type":"context.CancelFunc"}
{"type":"unresolved_use","from":"n13","to":"protoimpl.Pointer"}
```

- `id` is a scanner-local opaque counter (`n1`, `n2`, …); edges reference
  project nodes by `id`, unresolved targets by `fqn`. The ingestor maps `id`s
  to canonical FQNs. `code_type` is **not** emitted by scanners — the ingestor
  computes it from `path` + `apg/config.json` (`src/classify.rs`).
- `apg scan` injects a control record before each frontend's stream in a
  multi-language scan: `{"type":"lang_switch","language":"go"}`. The ingestor
  uses the current language for `code_type` classification and FQN rendering,
  and `--id-prefix` (per-frontend, `g1`/`t1`/…) keeps opaque ids unique across
  merged streams.
- Line numbers (`start_line`/`end_line`) are computed by the scanners (Go and
  C++ from byte offsets, Java from javac's UTF-16 char positions), never
  derived from bytes ingestor-side. `file` nodes (emitted one per scanned
  source file, including files with no declarations) carry `1..total-lines`;
  the scanner also sets the file's `parent` module.

### FQN convention (rendered ingestor-side, SPEC §4)

| kind | FQN |
|---|---|
| module | `fqn` verbatim |
| struct | `parent.name` |
| function (unique in scope) | `parent.name` |
| function (overloaded) | `parent.name(T1,T2,...)` — erased, comma-separated params |
| Go `init` | `parent.init#<file-basename>` |
| TypeScript | `pkg.relpath.name` — npm package + dot-path prefix per ES-module file (`@co/ui.src.components.Button.Button`); TS getters/setters land as overloads (`diameter()` / `diameter(number)`) |

Overloads are grouped by `(parent, name)`; any group of size > 1 renders every
member with the `(params)` suffix. The ingestor fails loudly (panics) on any
residual FQN collision rather than silently overwriting.

## LadybugDB Tooling

> **LadybugDB is a real, actively-developed project** — the successor to
> [KuzuDB](https://github.com/kuzudb/kuzu) (formerly known as Kuzu; ~6k commits,
> MIT-licensed, live releases). Do **not** assume it's fake or obsolete. Before
> dismissing it, read https://github.com/LadybugDB/ladybug (README, releases,
> docs at https://docs.ladybugdb.com) for current info — installable via
> `pip install ladybug`, `npm install @ladybugdb/core`, `cargo add lbug`, or the
> Go/Java/C++/CLI binaries.

The workspace has a LadybugDB graph database at `apg/.trans/db.lbug` containing the parsed codebase. Interact via the `apg_query` tool — a Cypher-like query interface. (The legacy `ladybug_query`/`ladybug_scan` tools were renamed to `apg_query`/`apg_scan`.)

### Query syntax

- Only `MATCH/RETURN` Cypher (no raw SQL). End with `;` (tool adds it if missing).
- Use backticks for reserved words: `` n.`end` ``
- `labels(n)` works (returns array), but `n._LABEL` or `r._LABEL` does **NOT**.
- `count(*)` works. Prefer it over `count(r)`.
- `ORDER BY`, `LIMIT`, `GROUP BY` work. Use `GROUP BY` (not `GROUP`).
- No joins, no subqueries, no `CALL db_info()`.
- `PROFILE` and `EXPLAIN` work for debugging plans.

### Quirks

- **`end` is a DuckDB reserved word** — you MUST quote it: `` n.`end` ``
- **Stderr is silenced** by `2>/dev/null` — if a query fails, you'll only see a parser/binder error.
- **`_LABEL` is internal** — you see it in raw output like `{_LABEL: Module, fqn: ...}` but you cannot filter/return on it. Use `labels(n)` instead.
- **Node IDs encode the label index**: `0:29` = label 0 entry 29, `1:5` = label 1, `2:8265` = label 2.

### Data model

- 5 code node types:
  - `Module` — property: `fqn`
  - `File` — properties: `fqn` (the absolute path), `start_line`, `end_line` (`1..total-lines`), `code_type`
  - `Struct` — properties: `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type`
  - `Function` — properties: `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type`
  - `UnresolvedTarget` — properties: `fqn`, `category` (a call/type reference the scanner could not resolve to a project symbol; deduplicated by name)
- 5 code edge types:
  - `Contains` — Module↔Module, Module→File, File→Struct, File→Function, Struct→Struct, Struct→Function
  - `Calls` — Function→Function
  - `Uses` — Function→Struct, Struct→Struct. Rust `impl X for Y` is a `Uses`
    edge `Y → X` (the type implements the trait); a foreign trait lands as an
    `UnresolvedUse` instead. Rust impl methods (inherent and trait) hang under
    the **self type** (`Type.method`); trait declarations/defaults under the
    trait.
  - `UnresolvedCall` — Function→UnresolvedTarget; rel-table property `target_type` (function type of a func-value call, Go-only, empty otherwise)
  - `UnresolvedUse` — Function→UnresolvedTarget, Struct→UnresolvedTarget
- Authored node labels (the `apg/layers/` node files): `Requirement` (fqn,
  body, feature), `Stakeholder`, `User`, `DomainGroup` (the `domain.group`
  type — `Group` is a reserved word; `attribute` core/supporting/generic,
  optional `root`), `Entity`, `Value`, `Service`, `System`, `Container`
  (`kind` app/service/db/queue), `Component`, `Person`, `Constraint` (prose;
  local ones carry `attaches_to`), `Note`. Transient labels: `Plan`,
  `PlanPhase`, `Task` (kind/tier/status), `Feedback` (body/status/
  disposition). The four Implementation kinds may carry `status: planned` for
  not-yet-built code (see "Planned Implementation nodes" below).
- Authored edge labels: `Contains` (hierarchy), `Drives` (Requirement →
  Group/Entity/Value/Service), `RealisedBy` (Group/Entity/Service →
  System/Container/Component), `SpecImplementedBy` (the DB rel-table name for
  the authored `implemented-by` kind: System/Container/Component → code),
  `Calls` (Service→Service), `Publishes`/`Subscribes` (Service → event
  Entity), `DependsOn` (Requirement→Requirement), `Uses` (Person→System),
  `Represents` (User→Entity, Entity→Person), `Details` (Note→any).
  Transient: `Gates` (PlanPhase→PlanPhase), `Satisfies` (PlanPhase→
  Requirement), `Reviews` (Feedback→any). Dependency and `contains` trees are
  acyclic; a dangling FQN is a write-time error (see the layers section
  below).

### `UnresolvedTarget.category`

One of `builtin` (Go predeclared func/type), `stdlib`, `external`, `func-value` (call through a function-valued variable or IIFE), `interface-method` (method on a universe-scope interface, e.g. `error.Error`), or `unknown` (fallback / frontend omitted it). Go populates this exactly from the type checker; Java classifies stdlib (`java.*`/`javax.*`/`jdk.*`) vs `external`; C++ is heuristic (`external` for qualified names, `func-value` for bare identifiers). Rust classifies exactly via crate origin (sysroot `std`/`core`/`alloc` → `stdlib`, dependency crates → `external`, project `macro_rules!` → `unknown`, closure calls → `func-value`).

Type conversions in Go (`[]byte(x)`, `protoimpl.Pointer(x)`, `(*T)(nil)`) are routed to `Uses`/`UnresolvedUse` edges, not `UnresolvedCall`. The `target_type` property only carries data on `UnresolvedCall` edges whose target is `func-value`.

### The authored graph: layers, spine, plans, feedback

The **durable spec** is a node-file store, not a JSONL: one file per node under
`apg/layers/`, authored via `apg node add|rm` / `apg edge add|rm` (never by
editing files by hand — the binary validates the schema, the pairings, and the
references, and auto-commits each mutation). FQN = **`<layer>.<type>.<name>`**,
no project prefix, no `spec.<id>` vocabulary; the **file name IS the identity**
and the node-file's `layer`/`type`/`name` fields must match the path. Both
halves of every edge live in the node files (out in the source's file, the
matching in in the target's); a pairing mismatch fails at scan. The six layers:

| layer | types |
|---|---|
| `requirements` | `stakeholder`, `user`, `requirement`, `note`, `constraint` |
| `domain` | `group` (attribute core/supporting/generic, optional `root`), `entity` (kind `entity`\|`event`), `value`, `service`, `note`, `constraint` |
| `solution` | `system`, `container` (kind app/service/db/queue), `component`, `person`, `note`, `constraint` |
| `implementation` | `note`, `constraint` (attach-only — the real nodes are scanned code) |
| `global` | `constraint`, `note` (the laws) |
| `plans` (transient) | `PlanPhase`, `Task`, planned Implementation nodes — `.trans/plans/` only |

**Constraints are prose**: the binary validates structure + references at write
time; whether the prose holds is assessed by review, never executed. A
`domain-rule`-style law is a `constraint` node, not an invariant.

The **spine** threads the tiers end to end:
`Stakeholder ⊃ Requirement —drives→ Domain —realised-by→ Solution
—implemented-by→ code`. Authored edge kinds: `contains`, `drives`,
`realised-by`, `implemented-by` (target is a code FQN, validated against the
scanned graph: resolves → real; planned → pending, not an error; gone →
drift, an error), `calls`, `publishes`/`subscribes` (Service → event Entity),
`depends-on`, `uses` (Person → System), `represents` (User → Entity, Entity →
Person), `details` (Note → any). Any requirement traces down to the code that
implements it; any code traces up to the why. A requirement is `delivered`
when review concludes the spine reaches it — satisfaction is by review, not
asserted by the binary.

### Planned Implementation nodes

- Code a plan will build exists in the graph *before* it is written: the
  plan-writer declares it as a **planned Implementation node** — a
  `Module`/`File`/`Struct`/`Function` record at its **real code FQN** carrying
  `status: planned` (`apg plan add <project> planned <kind> <fqn>`). A planned
  node has no location and is the target of a `creates` task.
- When the code actually exists, the next **branch scan replaces the planned
  node**: the scanner finds the FQN, clears `status`, fills in the location,
  and re-points incident edges. On `main` there are no planned nodes — every
  Implementation node is real code and the spine resolves straight through to
  it.
- The plan-writer never authors spec tiers 1–3 and the spec-writer never
  authors planned code.

### Plans, tasks & feedback (transient)

- `Task {fqn, title, kind, tier, status}` carries a two-axis classification:
  `kind` ∈ source/test/gate/docs is the **owning role** (orthogonal,
  `source` default); `tier` ∈ unit/int/e2e is the verification depth,
  required iff `kind = test`, rejected otherwise. Every task is
  implementer-workable — the human's decision point is plan end: the verify
  gate + merge act are the single delivery moment.
- `Feedback {fqn, body, status, disposition}` is a review item;
  `status` ∈ open/actioned/resolved. An artifact is done only when every
  `Feedback` on it is `resolved` — `apg plan complete` and the verify gate
  refuse otherwise. Feedback (and the plan itself) lives in the `.trans`
  mirrors and dies with the branch.
- Query patterns: the spine `MATCH (r:Requirement)-[:Drives]->(:Entity)-[:RealisedBy]->(:Container)-[:SpecImplementedBy]->(c) RETURN r.fqn, c.fqn`; plan health via the suite tools (`apg_plan`, `apg_plan_phases`, `apg_plan_tasks`, `apg_review`).
- The six distributed agents (installed by `apg init`): `codebase-navigator`
  (orchestrates the flow — `apg project start` from main, per-branch DB
  build, feedback routing, human-gate summary, and the merge act (verify
  gate → merge → rebuild) on approval), `spec-writer` / `plan-writer` (author
  through the `apg_node`/`apg_edge`/`apg_plan_*` tools, **no file writes**;
  the spec-writer authors the layer tiers + spine + reconciliation mode, the
  plan-writer the tier-4 delta with structural/holistic gates), `spec-review` /
  `plan-review` (attach/resolve/reject feedback, **no authoring tools**;
  approval-only wont-fix — a `--wont-fix` action is a proposal only the
  reviewer makes terminal), and `agent-builder` (`mode: primary`, the only
  write grant `.opencode/agents/**`, scaffolds a repo's code-writer agents).
  Repo-defined implementer / implementation-phase-reviewer agents are generated
  by `agent-builder` (assertion-only `plan done`, task notes, branch commits;
  phase review on branch scans + the final implementation review discovering
  divergence — fix code or reconcile the spec).

### `Struct.code_type` / `Function.code_type`

Classifies what kind of code a node lives in: `src` (default), `test`, `generated`, `external`, `lib`, or a user-defined value. All code is included in the graph; this column is how you filter it. Example: `MATCH (n:Function) WHERE n.code_type = 'test' RETURN n.fqn`.

Built-in defaults (per language):
- **Go**: `test` = `_test.go` or `test`/`tests` path segment; `generated` = `*.pb.go` or `gen`/`generated` segment; `external` = `vendor` segment.
- **Java**: `test` = `*Test.java`/`*Tests.java` or `test`/`tests` segment; `generated` = `gen`/`generated` segment; `external` = `vendor`/`third_party`.
- **C++**: `test` = `*_test.cpp`/`test_*.cpp` or `test`/`tests` segment; `generated` = `*.pb.cc`/`*.pb.h` or `gen`/`generated` segment; `external` = `vendor`/`third_party`/`external`.
- **Rust**: `test` = `*_test.rs` or `test`/`tests` segment; `generated` = `gen`/`generated` segment; `external` = `vendor`; else `src`.

An `apg/config.json` at the project root **replaces** the defaults. Shape:

```json
{
  "default": "src",
  "types": [
    { "name": "test", "globs": ["**/test/**", "**/*_test.go"], "names": ["Test*"] },
    { "name": "generated", "globs": ["**/*.pb.go", "**/gen/**"] },
    { "name": "external", "globs": ["vendor/**"] }
  ]
}
```

`globs` match the full path; `names` match the node simple name or FQN. First matching type (list order) wins, else `default`.
- Nodes without locations (Modules, UnresolvedTargets) have no `path`/`start`/`end` (and no `code_type`).
- `start` and `end` are **0-based byte indices**, not line numbers. Every
  located node also carries `start_line`/`end_line`, **1-based inclusive line
  numbers**; use them (not byte offsets) when joining against diffs, hunks, or
  anything line-oriented.
- `path` is an **absolute filesystem path** under the project directory. Read those files with `read`, `grep`, or `bash`.

### Fidelity & noise

- **Java, Go, Rust, TypeScript, and C# edges are exact** — resolved via the
  compiler's type checker (javac attribution / `types.Info`), rust-analyzer,
  the official TypeScript compiler API, or Roslyn. A `Calls` edge always points
  at the real declared method.
- **C++ edges are heuristic** (tree-sitter + scope/type tracking). Unresolvable calls/types are recorded as `UnresolvedCall`/`UnresolvedUse` rather than guessed.
- **The scanner never guesses**: if a call/type can't be resolved to a project symbol, it becomes an `UnresolvedTarget` edge, never a fabricated FQN.
- **All code is included** — tests, generated, and vendored code are scanned like everything else (the only exclusions are user `--exclude-path` patterns, `node_modules`, and files the compiler/frontend can't process). Filter by `code_type` instead.
- **Multi-module repos**: Go workspaces (`go.work`), C++ monorepos, Cargo
  workspaces, and npm workspaces are supported. Each module is a top-level
  `Module` node; FQNs are module-prefixed so they stay unique across modules.
  Pass `modules: "dir1,dir2"` to `apg_scan` to restrict scanning to specific
  modules (Go/C++/Rust/TS/C#).
- **Multi-language repos** (a Go backend + TS frontend, say): `apg scan`
  auto-detects every language present and merges their graphs into one
  database. Opaque ids are namespaced per language (`--id-prefix`, `g1`/`t1`/…);
  a `lang_switch` control record before each stream drives per-record
  `code_type` classification and FQN rendering.
- To see what the scanner couldn't resolve: `MATCH (f)-[:UnresolvedCall]->(u) RETURN u.fqn, count(f) ORDER BY 2 DESC LIMIT 20`

### Other tools

- `read`, `grep`, `glob`, `bash` — standard file operations.
- Extract byte ranges with: `dd if=<file> bs=1 skip=<start> count=<end-start> 2>/dev/null`
- Use `rg` (ripgrep) in bash for fast content search.
- `task` — spawn sub-agents for complex multi-file exploration.
