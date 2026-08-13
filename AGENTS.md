# APG Agent Guide

## LadybugDB Tooling

The workspace has a LadybugDB graph database at `db.lbug` containing the parsed codebase. Interact via the `ladybug_query` tool — a Cypher-like query interface.

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

- 4 node types:
  - `Module` — property: `fqn`
  - `Struct` — properties: `fqn`, `path`, `start`, `end`
  - `Function` — properties: `fqn`, `path`, `start`, `end`
  - `UnresolvedTarget` — property: `fqn` (a call/type reference the scanner could not resolve to a project symbol; deduplicated by name)
- 5 edge types:
  - `Contains` — Module↔Module, Module→Struct, Struct→Struct, Struct→Function
  - `Calls` — Function→Function
  - `Uses` — Function→Struct, Struct→Struct
  - `UnresolvedCall` — Function→UnresolvedTarget
  - `UnresolvedUse` — Function→UnresolvedTarget, Struct→UnresolvedTarget
- Nodes without locations (Modules, UnresolvedTargets) have no `path`/`start`/`end`.
- `start` and `end` are **0-based byte indices**, not line numbers.
- `path` is an **absolute filesystem path** under the project directory. Read those files with `read`, `grep`, or `bash`.

### Fidelity & noise

- **Java and Go edges are exact** — resolved via the compiler's type checker (javac attribution / `types.Info`). A `Calls` edge always points at the real declared method.
- **C++ edges are heuristic** (tree-sitter + scope/type tracking). Unresolvable calls/types are recorded as `UnresolvedCall`/`UnresolvedUse` rather than guessed.
- **The scanner never guesses**: if a call/type can't be resolved to a project symbol, it becomes an `UnresolvedTarget` edge, never a fabricated FQN.
- **Test/generated/build code is excluded by default** (`**/test/**`, `**/qa-functional/**`, `**/generated/**`, `**/build/**`, etc.). Pass `includeTests: true` to `ladybug_scan` to include it.
- **Multi-module repos**: Go workspaces (`go.work`) and C++ monorepos are supported. Each module is a top-level `Module` node; FQNs are module-prefixed so they stay unique across modules. Pass `modules: "dir1,dir2"` to `ladybug_scan` to restrict scanning to specific modules (Go/C++).
- To see what the scanner couldn't resolve: `MATCH (f)-[:UnresolvedCall]->(u) RETURN u.fqn, count(f) ORDER BY 2 DESC LIMIT 20`

### Other tools

- `read`, `grep`, `glob`, `bash` — standard file operations.
- Extract byte ranges with: `dd if=<file> bs=1 skip=<start> count=<end-start> 2>/dev/null`
- Use `rg` (ripgrep) in bash for fast content search.
- `task` — spawn sub-agents for complex multi-file exploration.
