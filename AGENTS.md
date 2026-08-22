# APG Agent Guide

## Pipeline

Scanner (per language) → Rust ingestor → `.apg/db.lbug` + `.apg/graph.jsonl`.

- The **scanner** (Go: `src/golib/main.go`, Java: `src/javalib/CallGraphBuilder.java`,
  C++: `src/cpplib/main.cpp`, Rust: `src/rustlib/src/main.rs` — a standalone
  `rustfrontend` binary built on rust-analyzer's `ra_ap_*`-era engine crates,
  pulled from the rust-analyzer repo at a pinned release tag) parses a codebase
  and streams one JSON object per
  line to stdout — the **unified JSONL schema** (see `SPEC.md` §2). It emits
  *facts only*: declarations, references, edges. It never computes FQNs and
  never does graph assembly.
- The **ingestor** (`src/ingest.rs`) spawns the scanner, resolves identity
  (canonical FQN), builds the graph, bulk-loads LadybugDB, and writes
  `graph.jsonl` as the export. `.apg/db.lbug` is the query index; `.apg/graph.jsonl`
  is the self-contained export artifact (canonical FQNs, no opaque ids).
- Build: `build.rs` compiles the frontends (`gcc`/`g++` tree-sitter for C++,
  `go build` for Go, `javac` for Java, `cargo build` for the Rust frontend —
  `src/rustlib`, a separate Cargo project, pinned to a rust-analyzer release
  tag) and stages them to
  `target/<profile>/frontends`. Run a scan with `apg scan <dir>` (or the
  `apg_scan` tool). `apg` resolves frontends at runtime relative to the binary
  (`<exe_dir>/frontends` or `<exe_dir>/../libexec/frontends`) or via
  `APG_FRONTEND_DIR`. `APG_BUILD_FRONTENDS` (comma-separated: `go`, `java`,
  `cpp`, `rust`; `0` to skip) limits what build.rs compiles.

### CLI

The project builds a single `apg` binary (package `apg`, was `java_apg`):

- `apg init [dir]` — create `.apg/` with a default `config.json` and install the
  opencode apg tool suite into `<dir>/.opencode/tools/` + `.opencode/lib/` plus
  the `codebase-navigator` agent into `<dir>/.opencode/agents/codebase-navigator.md`.
- `apg scan [dir] [--language L] [--exclude-path G]* [--module M]* [--no-build-scripts]
  [blacklist...]`
  — run the pipeline; writes `.apg/db.lbug`, `.apg/graph.jsonl`,
  `.apg/apg-frontend.log`. `--no-build-scripts` is Rust-only (skip cargo build
  scripts and the proc-macro server); the Rust frontend requires a Cargo
  manifest (C++ tolerates bare dirs; Rust scans nothing without one).
- `apg query "<cypher>"` — read-only Cypher over `.apg/db.lbug` (found by walking
  up from cwd), CSV output with header row.
- `apg --version`, `apg --help`.

`apg init` also installs the **apg opencode tool suite** into the project's
`.opencode/` (single-sourced from this repo's own `.opencode/`, embedded in
`src/main.rs` via `include_str!`): `apg_scan`, `apg_query`, plus curated
abstractions over common lookups — `apg_find_symbol`, `apg_modules`,
`apg_module_files`, `apg_module_structs`, `apg_file_units`, `apg_file_path`,
`apg_methods`, `apg_struct`, `apg_callers`, `apg_callees`, `apg_uses`,
`apg_unresolved`, `apg_hunk`. Shared plumbing lives in `.opencode/lib/apg.ts`
(root discovery, `apg query` subprocess, Cypher literal escaping). All suite
tools take an optional `codeType` (default: all code); exact-FQN tools hint
when a lookup comes up empty (overloads carry `(params)` suffixes).

The `apg` binary is brew-installable via split formulae (tap
`https://github.com/antz29/apg.git`): `scanner` (the binary), plus `apg-go`,
`apg-java`, `apg-cpp` frontends installed to `$(brew --prefix)/share/apg/frontends`
(the `scanner` formula's `bin/apg` wrapper sets `APG_FRONTEND_DIR` to that dir).
On Linux there's a `curl | sh` installer (`install.sh` — installs a single
tarball with the binary + all frontends to `/usr/local` or `--user`'s
`~/.local`, layout `bin/apg` + `libexec/apg/`); the linux-release workflow
builds `apg-linux-{x86_64,aarch64}.tar.gz` + `sha256sums.txt` per tag.
In the repo itself, run it via `cargo run -- scan <dir>` or
`target/debug/apg scan <dir>`.

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
  computes it from `path` + `apg.json`/`.apg/config.json` (`src/classify.rs`).
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

The workspace has a LadybugDB graph database at `.apg/db.lbug` containing the parsed codebase. Interact via the `apg_query` tool — a Cypher-like query interface. (The legacy `ladybug_query`/`ladybug_scan` tools were renamed to `apg_query`/`apg_scan`.)

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

- 5 node types:
  - `Module` — property: `fqn`
  - `File` — properties: `fqn` (the absolute path), `start_line`, `end_line` (`1..total-lines`), `code_type`
  - `Struct` — properties: `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type`
  - `Function` — properties: `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type`
  - `UnresolvedTarget` — properties: `fqn`, `category` (a call/type reference the scanner could not resolve to a project symbol; deduplicated by name)
- 5 edge types:
  - `Contains` — Module↔Module, Module→File, File→Struct, File→Function, Struct→Struct, Struct→Function
  - `Calls` — Function→Function
  - `Uses` — Function→Struct, Struct→Struct. Rust `impl X for Y` is a `Uses`
    edge `Y → X` (the type implements the trait); a foreign trait lands as an
    `UnresolvedUse` instead. Rust impl methods (inherent and trait) hang under
    the **self type** (`Type.method`); trait declarations/defaults under the
    trait.
  - `UnresolvedCall` — Function→UnresolvedTarget; rel-table property `target_type` (function type of a func-value call, Go-only, empty otherwise)
  - `UnresolvedUse` — Function→UnresolvedTarget, Struct→UnresolvedTarget

### `UnresolvedTarget.category`

One of `builtin` (Go predeclared func/type), `stdlib`, `external`, `func-value` (call through a function-valued variable or IIFE), `interface-method` (method on a universe-scope interface, e.g. `error.Error`), or `unknown` (fallback / frontend omitted it). Go populates this exactly from the type checker; Java classifies stdlib (`java.*`/`javax.*`/`jdk.*`) vs `external`; C++ is heuristic (`external` for qualified names, `func-value` for bare identifiers). Rust classifies exactly via crate origin (sysroot `std`/`core`/`alloc` → `stdlib`, dependency crates → `external`, project `macro_rules!` → `unknown`, closure calls → `func-value`).

Type conversions in Go (`[]byte(x)`, `protoimpl.Pointer(x)`, `(*T)(nil)`) are routed to `Uses`/`UnresolvedUse` edges, not `UnresolvedCall`. The `target_type` property only carries data on `UnresolvedCall` edges whose target is `func-value`.

### `Struct.code_type` / `Function.code_type`

Classifies what kind of code a node lives in: `src` (default), `test`, `generated`, `external`, `lib`, or a user-defined value. All code is included in the graph; this column is how you filter it. Example: `MATCH (n:Function) WHERE n.code_type = 'test' RETURN n.fqn`.

Built-in defaults (per language):
- **Go**: `test` = `_test.go` or `test`/`tests` path segment; `generated` = `*.pb.go` or `gen`/`generated` segment; `external` = `vendor` segment.
- **Java**: `test` = `*Test.java`/`*Tests.java` or `test`/`tests` segment; `generated` = `gen`/`generated` segment; `external` = `vendor`/`third_party`.
- **C++**: `test` = `*_test.cpp`/`test_*.cpp` or `test`/`tests` segment; `generated` = `*.pb.cc`/`*.pb.h` or `gen`/`generated` segment; `external` = `vendor`/`third_party`/`external`.
- **Rust**: `test` = `*_test.rs` or `test`/`tests` segment; `generated` = `gen`/`generated` segment; `external` = `vendor`; else `src`.

An `apg.json` at the project root or `.apg/config.json` **replaces** the defaults. Shape:

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

- **Java, Go, and Rust edges are exact** — resolved via the compiler's type
  checker (javac attribution / `types.Info`) or rust-analyzer. A `Calls` edge
  always points at the real declared method.
- **C++ edges are heuristic** (tree-sitter + scope/type tracking). Unresolvable calls/types are recorded as `UnresolvedCall`/`UnresolvedUse` rather than guessed.
- **The scanner never guesses**: if a call/type can't be resolved to a project symbol, it becomes an `UnresolvedTarget` edge, never a fabricated FQN.
- **All code is included** — tests, generated, and vendored code are scanned like everything else (the only exclusions are user `--exclude-path` patterns and files the compiler/frontend can't process). Filter by `code_type` instead.
- **Multi-module repos**: Go workspaces (`go.work`), C++ monorepos, and Cargo
  workspaces are supported. Each module is a top-level `Module` node; FQNs are
  module-prefixed so they stay unique across modules. Pass `modules: "dir1,dir2"`
  to `apg_scan` to restrict scanning to specific modules (Go/C++/Rust).
- To see what the scanner couldn't resolve: `MATCH (f)-[:UnresolvedCall]->(u) RETURN u.fqn, count(f) ORDER BY 2 DESC LIMIT 20`

### Other tools

- `read`, `grep`, `glob`, `bash` — standard file operations.
- Extract byte ranges with: `dd if=<file> bs=1 skip=<start> count=<end-start> 2>/dev/null`
- Use `rg` (ripgrep) in bash for fast content search.
- `task` — spawn sub-agents for complex multi-file exploration.
