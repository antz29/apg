# apg

**Program graph scanner + LadybugDB query CLI for opencode.**

`apg` parses a codebase (Go, Java, C++, Rust, TypeScript, or C#), builds a program graph of its
types, functions, and call/use relationships, and stores it in a LadybugDB
graph database that you can query with Cypher from inside opencode.

```
Scanner (per language) → Rust ingestor → apg/.trans/db.lbug + apg/.trans/graph.jsonl
```

## Features

- **Per-language scanner frontends** installed separately via brew — install
  only the languages you scan (Go, Java, C++, Rust, TypeScript, C#).
- **Exact edges for Go, Java, Rust, TypeScript, and C#** — call resolution uses
  the compiler's type checker (go/types, javac, rust-analyzer, the official
  TypeScript compiler, or Roslyn); C++ is heuristic (tree-sitter), and
  unresolvable refs become
  `UnresolvedTarget` nodes rather than guessed FQNs.
- **Multi-language codebases in one graph** — `apg scan` auto-detects every
  language present and merges their graphs into a single `apg/.trans/db.lbug` (a Go
  backend + TS frontend repo is one database, not two).
- **Everything is included** — tests, generated, and vendored code are scanned;
  filter by `code_type` (`src`, `test`, `generated`, `external`) in queries.
- **`apg init`** installs an opencode tool suite (find symbols, list methods,
  trace callers/callees, map diff hunks, …), so you can query the graph from
  chat without writing Cypher.
- **`apg query`** is self-contained — it uses the `lbug` crate directly, no
  separate LadybugDB shell needed.
- **Brew-installable** via the tap `antz29/apg`, plus a `curl | sh` installer
  for Linux (prebuilt x86_64 + aarch64 tarballs on each release).

## Requirements

- macOS (brew) or Linux (curl installer)
- [Homebrew](https://brew.sh/) (for the brew install)
- [opencode](https://opencode.ai) (for the chat plugin)

The `scanner` formula builds the `apg` binary; the language frontends are
separate formulae (`apg-go`, `apg-java`, `apg-cpp`, `apg-rust`). Install the
base plus the frontends for the languages you scan. Prebuilt bottles (macOS
arm64) are
published to each GitHub release by CI; if no bottle matches your system,
Homebrew falls back to building from source. Java projects additionally need
`java` on your PATH at scan time (see [below](#java-projects)); Rust projects
need a valid Cargo manifest (unlike C++, which tolerates bare directories), and
the `apg-rust` formula builds the frontend with the current stable toolchain.
TypeScript projects need `node` on your PATH at scan time (the `apg-ts`
frontend runs the official TypeScript compiler); a repo's `node_modules` is
always skipped, and workspace-package imports resolve even before `npm install`.
The C# frontend (`apg-csharp`) needs a .NET SDK only at build time — the
published binary is self-contained.

## Install (Homebrew)

```sh
brew tap antz29/apg https://github.com/antz29/apg.git
brew install antz29/apg/scanner \
             antz29/apg/apg-go \
             antz29/apg/apg-java \
             antz29/apg/apg-cpp \
             antz29/apg/apg-rust \
             antz29/apg/apg-ts \
             antz29/apg/apg-csharp
```

Install only the frontends you need:

```sh
brew install antz29/apg/scanner antz29/apg/apg-go   # Go only
```

Verify:

```sh
apg --version   # apg 0.11.x
apg --help
```

The stable install tracks the current `0.11.x` release tag. If you want the
latest unreleased code instead, pass `--HEAD`:

```sh
brew install antz29/apg/scanner --HEAD
```

## Install (Linux, `curl | sh`)

On Linux (x86_64 or aarch64), install the core `apg` scanner system-wide (requires root):

```sh
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sudo sh -s --
```

Or install to `~/.local` without root:

```sh
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user
```

Like the Homebrew setup, language frontends can be installed individually to keep installations lightweight:

```sh
# Install specific frontends only (e.g. Go and Rust):
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user go rust

# Or use the --frontends flag:
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user --frontends go,ts

# Install everything (scanner + all 6 frontends):
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user all
```

The installer verifies sha256 checksums for each component against `sha256sums.txt`.

Options:
- `--version 0.11.x`: pin a specific release tag
- `--user`: install under `~/.local` (no root required)
- `--prefix DIR`: choose a custom install location (default `/usr/local`)
- `--frontends L,L...`: comma-separated list of frontends to install
- `--all`: install scanner and all frontends
- `--force`: overwrite existing files without prompting
- `--uninstall [component...]`: remove specific components or the entire install

The binary links OpenSSL dynamically, so `libssl.so.3` must be present (it is on Ubuntu 22.04+/Debian 12+/Fedora 36+; the installer warns if it is missing). Java scan projects still need `java` on your PATH at scan time. TypeScript scan projects need `node` on your PATH.

Verify:

```sh
apg --version   # apg 0.11.x
apg --help
```

## Quick start

In your project directory:

```sh
apg init    # creates apg/ (committed config + gitignored .trans/), installs the opencode apg tool suite + six agents
apg scan    # scans the project, writes apg/.trans/db.lbug and apg/.trans/graph.jsonl
apg query "MATCH (m:Module) RETURN m.fqn LIMIT 10"
```

### 1. `apg init [dir]`

Sets up the project:

- creates `apg/` with a default `config.json` (classification rules + a
  binary-managed `version` field) and the gitignored `apg/.trans/`,
  scaffolds the repo `.gitignore` for `apg/.trans/` and `apg/.worktrees/`
  (added if missing, other lines untouched),
- installs the **apg tool suite** into `~/.opencode/tools/` (query tools +
  `apg_scan` + the spec/plan/review suite, shared plumbing in `~/.opencode/lib/`)
  and writes `~/.opencode/package.json` + runs `npm install` if needed,
- installs the **six distributed agents** into `~/.opencode/agents/`:
  `codebase-navigator`, `spec-writer`, `plan-writer`, `spec-review`,
  `plan-review`, and `agent-builder`.

The suite installs the first time and is then kept in sync (files are
re-written only when their contents change), so running `apg init` again after
upgrading `apg` updates the tools and agents where required.

Installing to `~/.opencode/` makes the tools and agents available to every
project's opencode session (not just this one). Project-specific implementer and
reviewer agents are installed into the project's `.opencode/agents/` by the
`agent-builder` agent, never by init. If the project's `.opencode/` holds files
that duplicate the installed suite, `apg init` prints a loud warning listing
them — it never deletes anything. The plugin and agents are auto-discovered by
opencode. **Restart opencode** after running `apg init` so the tools and agents
are available in chat.

### 2. `apg scan [dir] [options]`

Runs the scanner + ingestor for the project in `dir` (default: current
directory). Language is auto-detected from the source files — **every language
present** in a multi-language repo is scanned and merged into one graph.

```
apg scan
apg scan --language go /path/to/project
apg scan --language go,ts /path/to/project   # mixed repo, one graph
apg scan --exclude-path "**/*_test.go" --exclude-path "vendor/**"
apg scan --module dir1 --module dir2     # Go/C++/Rust/TS monorepos
apg scan --no-build-scripts              # Rust only: skip build scripts + proc-macro server
apg scan . example.com/pkg other.prefix  # blacklist FQN prefixes (after the dir)
```

Outputs (all under the gitignored `apg/.trans/` directory; the committed
`apg/` dir holds `config.json` plus the durable `apg/layers/` node files —
one JSON file per authored spec node, FQN `<layer>.<type>.<name>`):

| File | Contents |
|---|---|
| `db.lbug` | LadybugDB graph index (queryable) |
| `graph.jsonl` | Self-contained export (canonical FQNs, no opaque ids) |
| `config.json` | Classification config |
| `apg-frontend.log` | Full scanner + ingestor log |

### 3. `apg query "<cypher>"`

Runs a read-only Cypher query against `apg/.trans/db.lbug` (located by walking up
from the current directory). Default output is CSV with a header row; pass
`--json` for pretty-printed JSON objects:

```sh
apg query "MATCH (s:Struct) RETURN s.fqn, s.code_type"
apg query "MATCH (f:Function)-[:Calls]->(t:Function) RETURN f.fqn, t.fqn"
apg query "MATCH (f)-[:UnresolvedCall]->(u) RETURN u.fqn, count(f) ORDER BY 2 DESC LIMIT 20"
apg query --json "MATCH (s:Struct) RETURN s.fqn, s.code_type LIMIT 5"
```

Query syntax: `MATCH`/`RETURN` only (no raw SQL). `ORDER BY`, `LIMIT`,
`GROUP BY`, `labels()`, `count(*)` work. Backtick reserved words (`` n.`end` ``).

## Querying from opencode

`apg init` installs the **apg tool suite**. In an opencode session, ask the
agent to explore the graph directly — it will pick the right tool:

> *"List the methods of the `Transaction` type in `sdk/go`."*
> *"Who calls `ComputeContentHash`?"*
> *"What functions touch lines 190–240 of `store.go`?"*

| Tool | What it returns |
|---|---|
| `apg_find_symbol` | symbols whose FQN contains a string |
| `apg_modules` | list modules/packages |
| `apg_module_files` / `apg_module_structs` | files / types under a module |
| `apg_file_units` / `apg_file_path` | what a file contains; path → module |
| `apg_methods` / `apg_struct` | methods of a type; type + nested types |
| `apg_callers` / `apg_callees` | incoming / outgoing `Calls` |
| `apg_uses` | `Uses` edges in/out of a unit |
| `apg_unresolved` | unresolvable calls/uses for a unit or file |
| `apg_hunk` | units overlapping a line range (diff/review join) |
| `apg_query` | ad-hoc read-only Cypher (power users) |
| `apg_scan` | rebuild `apg/.trans/db.lbug` |
| `apg_project` | project lifecycle — `start` (main → worktree + branch + branch DB), `verify`, `merge` |
| `apg_node` / `apg_edge` | durable spec mutations — node files under `apg/layers/`, paired edges |
| `apg_plan` / `apg_plan_*` | plan phases/tasks/planned nodes, task notes, verify gate |
| `apg_review` / `apg_review_*` | writer↔reviewer feedback cycle (transient mirrors) |

The code-graph tools above return location data; the `apg_project`/`apg_node`/
`apg_edge`/`apg_plan_*`/`apg_review_*` tools operate on the project's spec/plan
graph (see [Graph-native specs and plans](#graph-native-specs-and-plans)).

Every row carries `fqn`, `path`, and `start_line`/`end_line` where relevant, so
the agent can jump straight to source. All suite tools accept an optional
`codeType` (`src`/`test`/`generated`/`external`); omitted = all code, matching
the raw graph.

## Graph data model

Node types:

| Label | Properties |
|---|---|
| `Module` | `fqn` |
| `File` | `fqn` (absolute path), `start_line`, `end_line`, `code_type` |
| `Struct` | `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type` |
| `Function` | `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type` |
| `UnresolvedTarget` | `fqn`, `category` (`builtin`/`stdlib`/`external`/`func-value`/`interface-method`/`unknown`) |

Edge types: `Contains` (Module→Module, Module→File, File→Struct, File→Function,
Struct→Struct, Struct→Function), `Calls` (Function→Function), `Uses`
(Function|Struct→Struct), `UnresolvedCall` (Function→UnresolvedTarget, prop
`target_type`), `UnresolvedUse` (Function|Struct→UnresolvedTarget).

Containment is a strict tree: a module contains files, and a file contains the
structs and functions declared in it. For review workflows, this gives you the
file scope directly: `MATCH (f:File {fqn:'...'})-[:Contains]->(n) RETURN n.fqn`
lists a file's units, and every node's `start_line`/`end_line` joins against
diff hunks (which are line-based).

FQN convention: `parent.name` for structs and unique functions;
`parent.name(T1,T2)` for overloads; Go `init` → `parent.init#<file.go>`. Rust
impl methods hang under their self type (`crate.Type.method`), trait
declarations and default methods under the trait; `Uses` edges record
`impl Trait for Type` relationships (`Type → Trait`). TypeScript FQNs are
npm-package + file-path-prefixed: a class `Button` in `src/components/Button.tsx`
of package `@co/ui` is `@co/ui.src.components.Button.Button`, and its method
`onClick` is `@co/ui.src.components.Button.Button.onClick` (each ES module file
is its own namespace, so same-named symbols in different files never collide).

`start`/`end` are **0-based byte offsets**; `start_line`/`end_line` are
**1-based inclusive line numbers**; `path` is absolute under the project
directory. (Java and TypeScript scanners report `start`/`end` as UTF-16 code-unit
offsets, matching their compilers' native positions.)

## Graph-native specs and plans (the project model)

A change-set is a **project**: a git branch + its worktree. `apg project start
<name>` — run from the main checkout — branches off the default branch, creates
the worktree at `apg/.worktrees/<name>`, auto-scans it, and **prints the
worktree path**; sessions then operate with cwd inside the worktree (walk-up
discovery finds the worktree's own `apg/` and its branch DB). Main is never a
mutation place — `apg node`/`apg edge`/`apg plan`/`apg review` refuse outside a
project context.

The durable spec is a **node-file store** under `apg/layers/` — one JSON file
per node, authored with `apg node add|rm <layer> <type> <name>` and
`apg edge add|rm <kind> <from> <to>` (never by hand-editing files: the binary
validates schema, pairings, and references, and auto-commits each mutation).
The FQN is **`<layer>.<type>.<name>`** — the file name IS the identity, no
project prefix. Six layers:

| layer | node types |
|---|---|
| `requirements` | `stakeholder`, `user`, `requirement`, `note`, `constraint` |
| `domain` | `group` (core/supporting/generic), `entity` (`entity`/`event`), `value`, `service`, `note`, `constraint` |
| `solution` | `system`, `container` (app/service/db/queue), `component`, `person`, `note`, `constraint` |
| `implementation` | `note`, `constraint` (attach-only — the real nodes are scanned code) |
| `global` | `constraint`, `note` (the laws) |
| `plans` (transient) | `PlanPhase`, `Task`, planned Implementation nodes — `.trans/plans/` only |

Node files carry both halves of every edge (the out half in the source's file,
the matching in half in the target's). Constraints are **prose** — the binary
validates structure and references at write time; whether the prose holds is
assessed by review, never executed. The **spine** threads the tiers end to end:
`Stakeholder ⊃ Requirement —drives→ Domain —realised-by→ Solution
—implemented-by→ code`; any requirement traces down to the code that implements
it, any code traces up to the why.

### Planned Implementation nodes (the pre-build bridge)

Code a plan will build exists in the graph *before* it is written: the
plan-writer declares it as a **planned Implementation node** — a
`Module`/`File`/`Struct`/`Function` record at its **real code FQN** carrying
`status: planned` (`apg plan add <project> planned <kind> <fqn>`). When the
code actually exists, the next **branch scan replaces the planned node**: the
scanner finds the FQN, clears `status`, fills in the location, and re-points
its incident edges. On `main` there are no planned nodes — every Implementation
node is real code, and the spine resolves straight through to it.

### Plans & feedback (transient)

The plan serializes to the gitignored, branch-local
`apg/.trans/plans/<project>.jsonl`; feedback lives in the `.trans/<tier>/`
mirrors (in the tier dir of the attached node). Both die with the branch —
nothing is committed. Tasks carry a **verb + target**: `creates` (a planned
node), `modifies`/`deletes` (existing code), `renames`/`moves` (`--fqn` source
→ `--to` destination).

- `apg plan done` is an **implementer assertion** — no graph verification.
- `apg plan complete` is a **milestone only** — the plan survives until verify.
- `apg plan note` attaches task notes (execution context, surfaced at the human gate).
- `apg plan verify <project>` runs the **pre-merge coherence gate** (every
  planned node **realized** — a branch scan found real code at its FQN; all
  feedback resolved; derived solution coverage holds) and prints the merge
  handoff. `apg project merge <name>` (from the main checkout) then runs
  verify → merge → unguarded main rebuild. Push/tag remain human.

## Configuration

`apg/config.json` (or a legacy `apg.json` at the project root) customizes
code-type classification. Built-in defaults per language (test/generated/
external) apply when no config is present. For Rust: `test` = `*_test.rs` or a
`test`/`tests` path segment; `generated` = `gen`/`generated` segment; `external`
= `vendor`. Shape:

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

First matching rule wins; otherwise `default`. `globs` match the full path;
`names` match the node simple name or FQN.

## Java projects

`apg` scans Java via a `java` process using javac's compiler API. The brew
formula depends on `openjdk`, but openjdk is keg-only, so `java` is not on
your PATH by default. Either link it or export it:

```sh
export PATH="$(brew --prefix openjdk)/bin:$PATH"
# or
brew link --force openjdk
```

## Building from source

Requires: Rust, `gcc`/`g++`, Go, `javac` (to build the frontends), plus
`cmake` and `openssl` for the bundled LadybugDB. The Rust frontend additionally
needs a current stable Rust toolchain (rust-analyzer tracks the newest stable)
and network at build time to fetch the pinned rust-analyzer crates; the
TypeScript frontend needs `node`/`npm` at build time (`build.rs` runs `npm ci`
in `src/tslib`); the C# frontend needs a .NET SDK at build time
(`build.rs` runs `dotnet publish`).

```sh
git clone git@github.com:antz29/apg.git
cd apg
cargo build --release
./target/release/apg --version
```

`build.rs` compiles the scanner frontends and stages them to
`target/<profile>/frontends`, which the binary finds at runtime relative to
itself (`<exe_dir>/frontends` or `<exe_dir>/../libexec/frontends`). Set
`APG_FRONTEND_DIR` to override, or `APG_BUILD_FRONTENDS` (comma-separated
allowlist: `go`, `java`, `cpp`, `rust`, `ts`, `csharp`; `0` to skip all) to
limit what build.rs compiles — the split brew formulae rely on this.

Run the test suite with `cargo test`.

## Project layout

```
src/main.rs          apg CLI (init / scan / query / node / edge / plan / project / review) + pipeline driver
src/ingest.rs        two-pass ingestion, canonical FQN rendering
src/load.rs          PARQUET load files → db.lbug, graph.jsonl export
src/classify.rs      code_type classification
src/layers.rs        node-file model: layers, validation, ingestion of apg/layers/
src/node_cmd.rs      durable node-file mutations (apg node / apg edge)
src/plan_cmd.rs      phased execution plan (apg plan, incl. the verify gate)
src/project_cmd.rs   project contexts (apg project start / merge, git2)
src/review_cmd.rs    writer↔reviewer feedback cycle (apg review)
src/specs.rs         plan/feedback JSONL serialization + re-ingest on scan
src/git.rs           git2 identity + worktree operations
src/version_gate.rs  apg/config.json layout version gate
src/golib/           Go scanner
src/javalib/         Java scanner (javac)
src/cpplib/          C++ scanner (tree-sitter)
src/rustlib/         Rust scanner (rust-analyzer engine; separate Cargo project)
src/tslib/           TypeScript scanner (official TypeScript compiler, Node)
src/csharplib/       C# scanner (Roslyn; separate build)
opencode-suite/      install template for `apg init` (tools/, lib/, agents/; embedded in src/main.rs)
install.sh           curl | sh installer for Linux (prebuilt release tarballs)
Formula/scanner.rb     apg binary (ingestor + query CLI)
Formula/apg-go.rb      Go scanner frontend
Formula/apg-java.rb    Java scanner frontend
Formula/apg-cpp.rb     C++ scanner frontend
Formula/apg-rust.rb    Rust scanner frontend
Formula/apg-ts.rb      TypeScript scanner frontend
Formula/apg-csharp.rb  C# scanner frontend
```

## License

MIT
