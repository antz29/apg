# STAGE 2 — Go scanner

Rewrite `src/golib/main.go` to emit the unified schema from [SPEC.md](SPEC.md)
instead of the current `pkg`/`decl`/`contains`/`call`/`use`/`u_call`/`u_use`
messages.

## Files touched

- `src/golib/main.go`

## Work items

- [ ] Assign an opaque `id` (monotonic counter) to every declared struct and
      function; reuse it in edge records.
- [ ] Emit node records:
  - `module` for each module path (as today).
  - `struct` with `id`, `parent` (package FQN), `name`, `path`, `start`, `end`.
  - `function` with `id`, `parent` (package FQN for free funcs, type FQN for
    methods), `name`, `params` (receiver type is *not* a param; list the actual
    call signature params), `file`, `path`, `start`, `end`.
  - `unresolved` with `fqn` and `category` (builtin/stdlib/external/func-value/
    interface-method/unknown) — reuse the existing `classifyCall` logic, but
    emit `category` on the target record instead of on the edge.
- [ ] Emit edge records:
  - `contains` (module→module/struct/function, struct→struct/function).
  - `calls` (function→function, by `id`).
  - `uses` (function→struct, struct→struct, by `id`).
  - `unresolved_call` (function→unresolved `fqn`; keep `target_type` on the edge).
  - `unresolved_use` (function/struct→unresolved `fqn`).
- [ ] Keep type conversions routed to `uses`/`unresolved_use` (not calls).
- [ ] Keep `Tests: true` and the absolute-path dedup of scanned files.
- [ ] Stop emitting `code_type` — the ingestor computes it.

## Acceptance

- Scanner output is pure unified-schema JSONL.
- `go build ./...` clean.
- Full pipeline (scanner → ingestor → db.lbug) runs end-to-end on
  `/Users/jledrew/platform`.
- No duplicate FQNs in `db.lbug` (Go `init` per file now disambiguated).
- `code_type` still classifies test/generated/src correctly (ingestor-side).
