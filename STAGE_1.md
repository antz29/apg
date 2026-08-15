# STAGE 1 — Ingestor rewrite (Rust)

Rewrite the Rust side to consume **only** the unified schema defined in
[SPEC.md](SPEC.md), render canonical FQNs, load LadybugDB via `COPY FROM`
(PARQUET load files), and write `graph.jsonl`. Delete all CSV writing and the old
ad-hoc message handling.

There is no shim: after this stage the ingestor no longer understands the old
`pkg`/`decl`/`call`/`use`/`u_call`/`u_use` messages. Stages 1–4 land together as
one atomic migration.

## Files touched

- `Cargo.toml` — `lbug = "0.19.1"` (no Arrow feature) + `parquet = "55"`
  (arrow-rs family, same major as lbug's `arrow = "55"`) for writing load files.
- `src/graph.rs` — model is keyed by canonical FQN (unchanged shape); drop any
  field that was only needed for the old message flow.
- `src/main.rs` — replace `build_graph` with the new ingestor.
- `src/cleanup.rs` — unchanged (still user excludes + Java span validation).
- `src/classify.rs` — unchanged (`code_type` from path + `apg.json`).

## Work items

- [ ] Confirm `COPY FROM` PARQUET end-to-end on `lbug` 0.19.1: write a
      `*.parquet` load file with the `parquet` crate, load into a node table and
      into a multi-pair rel table (`Contains`, with `from`/`to` options). Note:
      lbug 0.19.1 `FileType` supports only CSV/PARQUET/NPY — no JSON; NPY is
      numeric-only and unusable.
- [ ] Define Rust structs deserializing the unified node/edge records
      (tagged by `type`).
- [ ] FQN renderer: module verbatim; struct `parent.name`; function `parent.name`
      or `parent.name(T1,T2)`; Go `init` → `parent.init#<file-basename>`.
      Group by `(parent, name)` to detect overloads; **panic** on residual
      collision.
- [ ] Two-pass ingestion: nodes (id→FQN, FQN→Node with `code_type`/`category`/
      location) then edges (resolve ids → FQN pairs).
- [ ] Keep `code_type` classification from `path` (ingestor-side, `apg.json`).
- [ ] Keep cleanup (excludes, blacklist, Java span validation).
- [ ] `COPY FROM` load: write one PARQUET file per node table, one PARQUET file
      per rel-table `(from,to)` pair, in a temp dir; `COPY ... FROM` with
      `from`/`to` options for multi-pair rel tables. PARQUET columns must match
      the LadybugDB table schema exactly (`Module(fqn)`, `Struct/Function
      (fqn,path,start,end,code_type)`, `UnresolvedTarget(fqn,category)`,
      rel tables `from,to` + `target_type` where applicable).
- [ ] Write `graph.jsonl` export with canonical FQNs and resolved edges.
- [ ] Delete all `*.csv` writers and old `COPY FROM` statements.
- [ ] Delete the old `build_graph` message dispatch.

## Acceptance

- `cargo build --release` clean.
- The scanner subprocess is spawned and its stdout consumed as JSONL.
- `db.lbug` builds with the same schema (Module/Struct/Function/UnresolvedTarget
  + 5 rel tables); `ladybug_query` works.
- Multi-pair rel tables (`Contains` ×5, `Uses` ×2, `UnresolvedUse` ×2) load
  correctly via per-pair `COPY FROM` PARQUET.
- `graph.jsonl` is valid JSONL and self-contained.
- `COPY FROM` load files are PARQUET in a temp dir; no CSV files are produced and
  no load files remain after the load.

Note: the ingestor won't run end-to-end until STAGE_2 (Go scanner emits the new
schema) lands; Stage 1 is verified by unit-driving the parser/renderer against
hand-written fixture JSONL.
