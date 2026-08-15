# apg

**Program graph scanner + LadybugDB query CLI for opencode.**

`apg` parses a codebase (Go, Java, or C++), builds a program graph of its
types, functions, and call/use relationships, and stores it in a LadybugDB
graph database that you can query with Cypher from inside opencode.

```
Scanner (per language) → Rust ingestor → .apg/db.lbug + .apg/graph.jsonl
```

## Features

- **Per-language scanners** compiled in (Go, Java, C++) — no separate frontend
  install.
- **Exact edges for Go and Java** — call resolution uses the compiler's type
  checker; C++ is heuristic (tree-sitter), and unresolvable refs become
  `UnresolvedTarget` nodes rather than guessed FQNs.
- **Everything is included** — tests, generated, and vendored code are scanned;
  filter by `code_type` (`src`, `test`, `generated`, `external`) in queries.
- **`apg init`** installs an opencode plugin exposing the `apg_query` tool, so
  you can query the graph from chat.
- **`apg query`** is self-contained — it uses the `lbug` crate directly, no
  separate LadybugDB shell needed.
- **Brew-installable** via the tap `antz29/apg`.

## Requirements

- macOS or Linux
- [Homebrew](https://brew.sh/) (for the brew install)
- [opencode](https://opencode.ai) (for the chat plugin)

The brew formula builds all three scanners for you, so nothing else is needed
for Go and C++ projects. Java projects additionally need `java` on your PATH
at scan time (see [below](#java-projects)).

## Install (Homebrew)

```sh
brew tap antz29/apg https://github.com/antz29/apg.git
brew install antz29/apg/apg
```

Verify:

```sh
apg --version   # apg 0.1.0
apg --help
```

> **Note:** the formula's pinned release tarball (`v0.1.0`) is not published
> yet, so `brew install` will fail to fetch the stable URL until the tag is cut.
> Until then, install from HEAD:
>
> ```sh
> brew install --HEAD antz29/apg/apg
> ```
>
> The formula in this repo needs its `sha256` filled in once `v0.1.0` is tagged.

## Quick start

In your project directory:

```sh
apg init    # creates .apg/ (config + db location) and installs the opencode apg_query plugin
apg scan    # scans the project, writes .apg/db.lbug and .apg/graph.jsonl
apg query "MATCH (m:Module) RETURN m.fqn LIMIT 10"
```

### 1. `apg init [dir]`

Sets up the project:

- creates `.apg/` with a default `config.json` (classification rules),
- installs the opencode `apg_query` plugin into `.opencode/tools/apg_query.ts`
  (writing `.opencode/package.json` and running `npm install` if needed).

The plugin is auto-discovered by opencode. **Restart opencode** after running
`apg init` so the `apg_query` tool is available in chat.

### 2. `apg scan [dir] [options]`

Runs the scanner + ingestor for the project in `dir` (default: current
directory). Language is auto-detected from the source files.

```
apg scan
apg scan --language go /path/to/project
apg scan --exclude-path "**/*_test.go" --exclude-path "vendor/**"
apg scan --module dir1 --module dir2     # Go/C++ monorepos
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

`apg init` installs an `apg_query` tool. In an opencode session, ask the agent
to run graph queries directly:

> *"Find all functions in the `store` package."*
> *"Who calls `ComputeContentHash`?"*

The agent uses `apg_query` to traverse the graph (`Contains`, `Calls`, `Uses`,
`UnresolvedCall`, `UnresolvedUse` edges) and can read source files behind the
nodes via the `path`/`start`/`end` properties.

## Graph data model

Node types:

| Label | Properties |
|---|---|
| `Module` | `fqn` |
| `Struct` | `fqn`, `path`, `start`, `end`, `code_type` |
| `Function` | `fqn`, `path`, `start`, `end`, `code_type` |
| `UnresolvedTarget` | `fqn`, `category` (`builtin`/`stdlib`/`external`/`func-value`/`interface-method`/`unknown`) |

Edge types: `Contains` (Module→Module/Struct/Function, Struct→Struct/Function),
`Calls` (Function→Function), `Uses` (Function|Struct→Struct),
`UnresolvedCall` (Function→UnresolvedTarget, prop `target_type`),
`UnresolvedUse` (Function|Struct→UnresolvedTarget).

FQN convention: `parent.name` for structs and unique functions;
`parent.name(T1,T2)` for overloads; Go `init` → `parent.init#<file.go>`.

`start`/`end` are **0-based byte offsets**; `path` is absolute under the
project directory.

## Configuration

`.apg/config.json` (or a legacy `apg.json` at the project root) customizes
code-type classification. Built-in defaults per language (test/generated/
external) apply when no config is present. Shape:

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
`cmake` and `openssl` for the bundled LadybugDB.

```sh
git clone git@github.com:antz29/apg.git
cd apg
cargo build --release
./target/release/apg --version
```

`build.rs` compiles all three scanners and stages them to
`target/<profile>/frontends`, which the binary finds at runtime relative to
itself (`<exe_dir>/frontends` or `<exe_dir>/../libexec/frontends`). Set
`APG_FRONTEND_DIR` to override.

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
Formula/apg.rb     Homebrew formula
```

## License

MIT
