# STAGE 3 — Java scanner

Rewrite `src/javalib/CallGraphBuilder.java` to emit the unified schema from
[SPEC.md](SPEC.md). This also fixes the Java overload/constructor FQN collision
(disambiguation is now handled ingestor-side via `params`).

## Files touched

- `src/javalib/CallGraphBuilder.java`

## Work items

- [ ] Assign an opaque `id` (counter) to every declared class and method;
      reuse it in edge records.
- [ ] Emit node records:
  - `module` for each package (as today).
  - `struct` with `id`, `parent` (enclosing package/outer class FQN), `name`,
    `path`, `start`, `end`.
  - `function` with `id`, `parent` (enclosing class FQN), `name`, `params`
    (erased, qualified parameter types via `MethodSymbol`/`VarSymbol`),
    `file`, `path`, `start`, `end`. Constructors are `name = "<init>"`.
  - `unresolved` with `fqn` and `category` (`stdlib` for `java.*`/`javax.*`/
    `jdk.*`, `external` otherwise, `unknown` when unresolved) — reuse
    `categoryOf`.
- [ ] Emit edge records:
  - `contains` (package/class→class, class→method).
  - `calls` (method→method by `id`).
  - `uses` (method→class, class→class by `id`).
  - `unresolved_call` (method→unresolved `fqn`; `target_type` empty).
  - `unresolved_use` (method/class→unresolved `fqn`).
- [ ] Preserve the crash-isolation (re-attribute after dropping crashing files).
- [ ] Keep constructor/overload resolution exact via `params` (no more simple-name
      collision).
- [ ] Stop emitting `code_type`.

## Acceptance

- Scanner output is pure unified-schema JSONL.
- `javac` build (via `build.rs`) clean.
- End-to-end on a Java fixture: overloaded methods/constructors produce distinct
  FQNs (`pkg.Cls.foo(int)` vs `pkg.Cls.foo(String)`, `pkg.Cls.<init>(...)`).
- `category` correct for stdlib vs external.
