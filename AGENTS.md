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

- 3 node types:
  - `Module` — property: `fqn`
  - `Struct` — properties: `fqn`, `path`, `start`, `end`
  - `Function` — properties: `fqn`, `path`, `start`, `end`
- 3 edge types:
  - `Contains` — Module↔Module, Module→Struct, Struct→Struct, Struct→Function
  - `Calls` — Function→Function
  - `Uses` — Function→Struct, Struct→Struct
- Nodes without locations (Modules) have no `path`/`start`/`end`.
- `start` and `end` are **0-based byte indices**, not line numbers.
- `path` is an **absolute filesystem path** under the project directory. Read those files with `read`, `grep`, or `bash`.

### Other tools

- `read`, `grep`, `glob`, `bash` — standard file operations.
- Extract byte ranges with: `dd if=<file> bs=1 skip=<start> count=<end-start> 2>/dev/null`
- Use `rg` (ripgrep) in bash for fast content search.
- `task` — spawn sub-agents for complex multi-file exploration.
