# SPEC: Unified Scanner → Ingestor JSONL → PARQUET → db.lbug

This spec defines the pipeline that replaces the current per-frontend ad-hoc JSON
formats and the CSV export. It is the single source of truth for the schema, the
identity model, and the FQN disambiguation convention.

## 1. Architecture

```
[scanner: go | java | cpp]  --unified JSONL on stdout-->  [rust ingestor]  -->  db.lbug + graph.jsonl
```

- The **scanner** (one per language) parses a codebase and streams one JSON
  object per line to stdout. It emits *facts* only: declarations, references,
  and edges. It never computes FQNs and never does graph assembly.
- The **ingestor** (Rust) spawns the scanner, reads its stdout, resolves identity
  (canonical FQN), builds the graph, bulk-loads LadybugDB via `COPY FROM`
  (PARQUET load files), and writes `graph.jsonl` as the export.

No backwards-compatibility shim, no CSV, no legacy formats. All three scanners
and the ingestor migrate together (see STAGE_1..STAGE_4).

## 2. Unified JSONL schema

One JSON object per line. Two kinds of records: nodes and edges.

### 2.1 Node records

```jsonl
{"type":"module","fqn":"github.com/foundry/flow"}
{"type":"file","path":"/abs/store.go","parent":"github.com/foundry/flow","start_line":1,"end_line":142}
{"type":"struct","id":"n12","parent":"...v1","name":"Error","path":"/abs/error.go","start":12,"end":300,"start_line":12,"end_line":45}
{"type":"function","id":"n13","parent":"...Store","name":"ComputeContentHash","params":["[]byte","int"],"file":"/abs/store.go","path":"/abs/store.go","start":1,"end":99,"start_line":34,"end_line":99}
{"type":"unresolved","fqn":"fmt.Errorf","category":"stdlib"}
```

### 2.2 Edge records

```jsonl
{"type":"contains","from":"n12","to":"n13"}
{"type":"calls","from":"n13","to":"n14"}
{"type":"uses","from":"n13","to":"n12"}
{"type":"unresolved_call","from":"n13","to":"fmt.Errorf","target_type":"context.CancelFunc"}
{"type":"unresolved_use","from":"n13","to":"protoimpl.Pointer"}
```

### 2.3 Field reference

| field | node types | meaning |
|---|---|---|
| `type` | all | discriminator: `module` / `file` / `struct` / `function` / `unresolved` / `contains` / `calls` / `uses` / `unresolved_call` / `unresolved_use` |
| `fqn` | `module`, `unresolved` | verbatim fully-qualified name (module path, or unresolved target name) |
| `id` | `struct`, `function` | scanner-local opaque identifier (plain counter `n1`, `n2`, …) |
| `parent` | `struct`, `function` | enclosing scope FQN: package (free func), type (method / struct), namespace (C++) |
| `parent` | `file` | the module FQN the file belongs to (`""` for a file in no module, e.g. Java's default package) |
| `name` | `struct`, `function` | simple name |
| `params` | `function` | parameter type names, in declaration order, for overload disambiguation |
| `file` | `function` | absolute source file path, for Go `init` disambiguation |
| `path` | `struct`, `function`, `file` | absolute source file path (location; for `file` nodes it is also their identity) |
| `start` / `end` | `struct`, `function` | 0-based byte offsets |
| `start_line` / `end_line` | `struct`, `function`, `file` | 1-based inclusive line range of the span; for `file` nodes it is `1..total-lines` |
| `category` | `unresolved` | `builtin` / `stdlib` / `external` / `func-value` / `interface-method` / `unknown` |
| `from` / `to` | edges | `id` (project node), `fqn` (unresolved target), or absolute path (file node) |
| `target_type` | `unresolved_call` | function type of a func-value call (Go-only, empty otherwise) |

`code_type` is **not** emitted by scanners. The ingestor computes it from `path`
plus the project `apg.json` (see `src/classify.rs`).

Line numbers are **computed by the scanners**, never derived from byte offsets
ingestor-side: Go offsets are bytes, Java offsets are UTF-16 character
positions (which drift from bytes on non-ASCII files), and C++ offsets are
bytes. Each scanner converts its own index space to 1-based line numbers and
emits them directly.

File nodes are emitted by the scanners (one per scanned source file, including
files with no declarations). The ingestor wires the File layer into
containment: `Module→File` from the file record's `parent`, and `File→
(Struct|Function)` derived from each node's `path`. The scanners never emit the
old `Module→Struct` / `Module→Function` edges; those are replaced by the file
hop.

## 3. Identity & opaque ids

- Project declarations (`struct`, `function`) carry a scanner-local opaque `id`.
  Edges that reference a project node use that `id`.
- The `id` is unique within a single scan and otherwise meaningless; the ingestor
  maps it to a canonical FQN. FQN stability across scans comes from the identity
  fields (`parent`/`name`/`params`/`file`), not from the `id`.
- Unresolved targets have no `id`; they are keyed by `fqn` (deduplicated by name,
  which is intentional for these).

## 4. FQN convention (disambiguation)

Rendered once, in the ingestor:

| kind | FQN |
|---|---|
| module | `fqn` verbatim |
| struct | `parent.name` |
| function (unique name in scope) | `parent.name` |
| function (overloaded) | `parent.name(T1,T2,...)` — erased parameter types, comma-separated, qualified for reference types |
| Go `init` | `parent.init#<file-basename>` (`init` has no signature; the file is the only stable discriminator) |

Rules:

- The ingestor groups functions by `(parent, name)`; any group of size > 1 is
  overloaded and every member gets the `(params)` suffix.
- The ingestor **fails loudly** on any residual FQN collision (two distinct
  declarations mapping to the same rendered FQN). Never silently overwrite.
- One legitimate exception: the JVM allows a **package** and a **type** to share
  a name (`pkg.A` the package and `pkg.A` the class, as in NetBeans' QA test-data
  layout). Flat FQN space cannot represent both, so the type wins and the
  shadowed `module` node is dropped; contains edges touching it are pruned.
- The same layout can make a **struct** and a **function** collide (a class in a
  shadowed package renders the same FQN as a method of the class that shadowed
  it). The struct wins; the function is dropped. Function-vs-function (a genuine
  duplicate declaration) still panics.
- A real declaration always replaces an `UnresolvedTarget` placeholder of the
  same FQN (unresolved nodes are only guesswork from the scanner).

## 5. code_type and category

- `code_type` (on `Struct`/`Function`): `src` (default), `test`, `generated`,
  `external`, `lib`, or user-defined. Computed ingestor-side by `classify.rs`
  from `path` + language + optional `apg.json` override. Unchanged from today.
- `category` (on `UnresolvedTarget`): as in §2.3, populated by the scanner
  (Go exact via type checker; Java coarse; C++ heuristic).

## 6. Ingestor pipeline

1. Spawn the scanner (`frontend_cmd`), stream stdout line by line.
2. Buffer all records, then two passes:
   a. node records → build `id → FQN` and `FQN → Node` (apply FQN rendering,
      compute `code_type` for `struct`/`function`/`file`, `category` for
      `unresolved`).
   b. edge records → resolve `from`/`to` (id → FQN for project nodes, fqn
      verbatim for unresolved targets) into edge sets.
3. Wire the File layer: `Module→File` from each file record's `parent`, and
   `File→(Struct|Function)` derived from each located node's `path`.
4. Cleanup: user `--exclude-path` globs, blacklist, Java span validation.
5. Bulk-load via `COPY FROM` PARQUET (LadybugDB's recommended bulk path):
   - lbug 0.19.1 `COPY FROM` supports exactly three file types: CSV, PARQUET,
     NPY (see `FileType` in `file_scan_info.h`). **NPY is numeric-only** (`f8/f4/
     i8/i4/i2`) — unusable for graph data. PARQUET is the chosen format: typed,
     compressed, columnar, no escaping issues. There is **no JSON file type**.
- Node tables: one PARQUET file per table (`module.parquet`, `struct.parquet`,
     `function.parquet`, `file.parquet`, `unresolved.parquet`), written to a
     temp dir, columns matching the LadybugDB table schema exactly.
   - Rel tables: one PARQUET file per `from`/`to` pair, loaded with the pair
     options — `COPY Contains FROM 'contains_mod_file.parquet' (from='Module',
     to='File')`. Rel-group binding parses `from`/`to` generically before any
     file-format logic (`bind_copy_from.cpp`), so this works identically for
     PARQUET and CSV. This is the documented path for multi-pair rel tables.
   - The load files are written with the `parquet` crate (arrow-rs family).
6. Write `graph.jsonl` export: the final graph, re-serialized with canonical
   `fqn`s (nodes) and resolved `from`/`to` (edges) — self-contained, mirroring
   this schema but without opaque ids.

## 7. LadybugDB schema

Node tables: `Module`, `Struct`, `Function`, `File`, `UnresolvedTarget`.

| label | columns | notes |
|---|---|---|
| `Module` | `fqn` (pk) | no location |
| `Struct` | `fqn` (pk), `path`, `start`, `` `end` ``, `start_line`, `end_line`, `code_type` | bytes + lines |
| `Function` | `fqn` (pk), `path`, `start`, `` `end` ``, `start_line`, `end_line`, `code_type` | bytes + lines |
| `File` | `fqn` (pk), `start_line`, `end_line`, `code_type` | `fqn` is the absolute path (identity); lines are `1..total`; no byte span |
| `UnresolvedTarget` | `fqn` (pk), `category` | no location |

Rel tables: `Contains`, `Calls`, `Uses`, `UnresolvedCall` (prop `target_type`),
`UnresolvedUse`.

Containment is a strict tree: `Module→Module`, `Module→File`, `File→Struct`,
`File→Function`, `Struct→Struct`, `Struct→Function`. A file contains every
struct and function declared in it (Go methods included, even when declared in
a different file than their receiver's struct). A `File` whose module is
shadowed (Java package/type name collision) or blacklisted stays in the graph
but loses its `Module→File` edge.

The query surface (`ladybug_query`, Cypher over `db.lbug`) is unaffected.

### Why not the `arrow` feature?

lbug's `arrow` feature registers in-memory tables via `create_arrow_table` /
`create_arrow_rel_table`. `createRelTableFromArrowTable` builds a
`CREATE REL TABLE (FROM src TO dst)` with a **single pair**
(`arrow_table_support.cpp`), so it cannot express the multi-pair rel tables
(`Contains` ×6, `Uses` ×2, `UnresolvedUse` ×2) without restructuring them into
12 separate rel tables — schema-breaking. Hence rel-table loading goes through
`COPY FROM`, not the Arrow API. The `parquet` crate pulls arrow-rs into the build
graph purely as a *file writer*; that is distinct from the lbug `arrow` feature
and does not change the query surface.

## 8. Dependencies

- `lbug = "0.19.1"` (no `arrow` feature — rel-table loading uses `COPY FROM`).
- `parquet = "55"` (arrow-rs family) for writing the PARQUET load files — same
  major as the `arrow = "55"` lbug builds against, keeping the FFI/format line
  consistent.
- PARQUET `COPY FROM` needs no extension; it is compiled into the engine
  (`function_collection.cpp` registers the Parquet scan function).

## 9. Non-goals / constraints

- No backwards-compatibility shim or legacy format handling in the ingestor.
- No CSV anywhere. PARQUET load files are transient, written to a temp dir;
  `graph.jsonl` is the export artifact, `db.lbug` is the query index.
- Scanners do not know about graph identity, FQNs, or disambiguation.
