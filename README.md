# apg

**Program graph scanner + LadybugDB query CLI for opencode.**

`apg` parses a codebase (Go, Java, C++, or Rust), builds a program graph of its
types, functions, and call/use relationships, and stores it in a LadybugDB
graph database that you can query with Cypher from inside opencode.

```
Scanner (per language) → Rust ingestor → .apg/db.lbug + .apg/graph.jsonl
```

## Features

- **Per-language scanner frontends** installed separately via brew — install
  only the languages you scan (Go, Java, C++, Rust).
- **Exact edges for Go, Java, and Rust** — call resolution uses the compiler's
  (or rust-analyzer's) type checker; C++ is heuristic (tree-sitter), and
  unresolvable refs become `UnresolvedTarget` nodes rather than guessed FQNs.
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

## Install (Homebrew)

```sh
brew tap antz29/apg https://github.com/antz29/apg.git
brew install antz29/apg/scanner \
             antz29/apg/apg-go \
             antz29/apg/apg-java \
             antz29/apg/apg-cpp \
             antz29/apg/apg-rust
```

Install only the frontends you need:

```sh
brew install antz29/apg/scanner antz29/apg/apg-go   # Go only
```

Verify:

```sh
apg --version   # apg 0.6.2
apg --help
```

`v0.6.2` is tagged, so the stable install works as-is. If you want the latest
unreleased code instead, pass `--HEAD`:

## Install (Linux, `curl | sh`)

On Linux (x86_64 or aarch64), install system-wide (requires root):

```sh
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sudo sh -s --
```

Or install to `~/.local` without root:

```sh
curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user
```

The installer fetches the latest release, verifies the tarball's sha256
against the `sha256sums.txt` published with it, and installs the `apg` binary
plus all four scanner frontends (Go, Java, C++, Rust) — no separate frontend
install needed, unlike the split brew formulae.

Options: `--version 0.6.2` to pin a specific release, `--prefix DIR` to choose
an install location, `--force` to overwrite an existing install, `--uninstall`
to remove it. The binary links OpenSSL dynamically, so `libssl.so.3` must be
present (it is on Ubuntu 22.04+/Debian 12+/Fedora 36+; the installer warns if
it is missing). Java scan projects still need `java` on your PATH at scan time.

Verify:

```sh
apg --version   # apg 0.6.2
apg --help
```

## Quick start

In your project directory:

```sh
apg init    # creates .apg/ (config + db location), installs the opencode apg tool suite and codebase-navigator agent
apg scan    # scans the project, writes .apg/db.lbug and .apg/graph.jsonl
apg query "MATCH (m:Module) RETURN m.fqn LIMIT 10"
```

### 1. `apg init [dir]`

Sets up the project:

- creates `.apg/` with a default `config.json` (classification rules),
- installs the **apg tool suite** into `~/.opencode/tools/` (query tools +
  `apg_scan`, shared plumbing in `~/.opencode/lib/`) and writes
  `~/.opencode/package.json` + runs `npm install` if needed,
- installs the `codebase-navigator` agent into `~/.opencode/agents/codebase-navigator.md`.

The suite installs the first time and is then kept in sync (files are
re-written only when their contents change), so running `apg init` again after
upgrading `apg` updates the tools and agent where required.

Installing to `~/.opencode/` makes the tools and agent available to every
project's opencode session (not just this one). As part of that move, `apg init`
also removes any legacy project-local `.opencode/` apg install left by older
versions — apg-owned tools/agent/lib files only; your own agents/tools and any
hand-written `package.json` are left alone. The plugin and agent are
auto-discovered by opencode. **Restart opencode** after running `apg init` so
the tools and `codebase-navigator` agent are available in chat.

### 2. `apg scan [dir] [options]`

Runs the scanner + ingestor for the project in `dir` (default: current
directory). Language is auto-detected from the source files.

```
apg scan
apg scan --language go /path/to/project
apg scan --exclude-path "**/*_test.go" --exclude-path "vendor/**"
apg scan --module dir1 --module dir2     # Go/C++/Rust monorepos
apg scan --no-build-scripts              # Rust only: skip build scripts + proc-macro server
apg scan . example.com/pkg other.prefix  # blacklist FQN prefixes (after the dir)
```

Outputs (all under the project's `.apg/` directory):

| File | Contents |
|---|---|
| `db.lbug` | LadybugDB graph index (queryable) |
| `graph.jsonl` | Self-contained export (canonical FQNs, no opaque ids) |
| `config.json` | Classification config |
| `apg-frontend.log` | Full scanner + ingestor log |

### 3. `apg query "<cypher>"`

Runs a read-only Cypher query against `.apg/db.lbug` (located by walking up
from the current directory), printing CSV with a header row:

```sh
apg query "MATCH (s:Struct) RETURN s.fqn, s.code_type"
apg query "MATCH (f:Function)-[:Calls]->(t:Function) RETURN f.fqn, t.fqn"
apg query "MATCH (f)-[:UnresolvedCall]->(u) RETURN u.fqn, count(f) ORDER BY 2 DESC LIMIT 20"
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
| `apg_scan` | rebuild `.apg/db.lbug` |

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
`impl Trait for Type` relationships (`Type → Trait`).

`start`/`end` are **0-based byte offsets**; `start_line`/`end_line` are
**1-based inclusive line numbers**; `path` is absolute under the project
directory.

## Configuration

`.apg/config.json` (or a legacy `apg.json` at the project root) customizes
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

Requires: Rust, `gcc`/`g++`, Go, and `javac` (to build the frontends), plus
`cmake` and `openssl` for the bundled LadybugDB. The Rust frontend additionally
needs a current stable Rust toolchain (rust-analyzer tracks the newest stable)
and network at build time to fetch the pinned rust-analyzer crates.

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
allowlist: `go`, `java`, `cpp`, `rust`; `0` to skip all) to limit what
build.rs compiles — the split brew formulae rely on this.

Run the test suite with `cargo test`.

## Project layout

```
src/main.rs        apg CLI (init / scan / query) + pipeline driver
src/ingest.rs      two-pass ingestion, canonical FQN rendering
src/load.rs        PARQUET load files → db.lbug, graph.jsonl export
src/classify.rs    code_type classification
src/golib/         Go scanner
src/javalib/       Java scanner (javac)
src/cpplib/        C++ scanner (tree-sitter)
src/rustlib/       Rust scanner (rust-analyzer engine; separate Cargo project)
install.sh         curl | sh installer for Linux (prebuilt release tarballs)
Formula/scanner.rb    apg binary (ingestor + query CLI)
Formula/apg-go.rb     Go scanner frontend
Formula/apg-java.rb   Java scanner frontend
Formula/apg-cpp.rb    C++ scanner frontend
Formula/apg-rust.rb   Rust scanner frontend
```

## License

MIT
