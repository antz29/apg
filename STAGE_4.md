# STAGE 4 — C++ scanner + docs + finalize

Rewrite `src/cpplib/main.cpp` to emit the unified schema from [SPEC.md](SPEC.md),
then remove the last remnants of the old format and update the docs. This is the
final stage of the atomic migration.

## Files touched

- `src/cpplib/main.cpp`
- `AGENTS.md`
- `.opencode/agents/codebase-navigator.md`

## Work items

- [x] Assign an opaque `id` (counter) to every declared struct/class/function;
      reuse it in edge records.
- [x] Emit node records:
  - `module` for each module (as today).
  - `struct` with `id`, `parent` (module/namespace FQN), `name`, `path`, `start`,
    `end`.
  - `function` with `id`, `parent` (namespace or class FQN), `name`, `params`
    (parameter type names via `type_node_to_fqn`, best-effort), `file`, `path`,
    `start`, `end`.
  - `unresolved` with `fqn` and `category` (heuristic: `external` for qualified
    names, `func-value` for bare call targets, `unknown` for type refs) — reuse
    `category_for`.
- [x] Emit edge records (`contains`, `calls`, `uses`, `unresolved_call`,
      `unresolved_use`) with `id`/`fqn` endpoints as in the spec.
- [x] Keep heuristic resolution rules unchanged (no new guessing).
- [x] Stop emitting `code_type`.

## Docs

- [x] `AGENTS.md`: document the scanner→ingestor pipeline, the unified JSONL
      schema, the FQN convention (`parent.name(T1,T2)` / `init#file.go`), and
      that `graph.jsonl` is the export.
- [x] `.opencode/agents/codebase-navigator.md`: point agents at the new schema
      and FQN convention.

## Acceptance

- All three scanners + the ingestor run the unified schema end-to-end.
- No old-format messages, no CSV writers, no legacy code anywhere in `src/`.
- `cargo build --release` clean (all three frontends compile via `build.rs`).
- Rescan `/Users/jledrew/platform` (Go) and smoke-test Java/C++ fixtures:
  `db.lbug` correct, `graph.jsonl` valid, no duplicate FQNs.
