# SPEC — `fortran` frontend (`fortranfrontend`)

A standalone Rust frontend that scans Fortran source and streams the unified JSONL facts
(SPEC §2) that the ingestor turns into the graph. The default dialect is gfortran's
`-std=gnu`, the compiler's own default (§6). Status: **planned, not implemented.** Items
marked **OPEN** are not decided and are collected in §19. Decisions are recorded in §21.
Facts checked against GCC's sources and manual are logged in §23.

## 1. Governing principles

1. **Facts only.** The frontend emits declarations, references and edges. It never computes
   canonical FQNs and never assembles the graph; the ingestor does both (SPEC §2, §4).
2. **Shaped like real Fortran projects.** The scanner is designed around how Fortran projects
   are laid out in practice (§4): module-per-file libraries, `app/` and `test/` programs,
   vendored dependencies, and legacy trees of free-standing procedures.
3. **gfortran is the reference for defaults.** Where the language standard and gfortran's
   default behaviour differ, the scanner follows gfortran's default, because a project is
   compiled with it. Each such choice is cited in §23.
4. **Never guess.** A reference that cannot be resolved to a project symbol is an unresolved
   target with a category, never a fabricated identity.
5. **Tolerant.** A syntax error costs the statements it touches, never the file or the scan.
6. **One scanner for every form.** The source form is chosen by file extension (§2, §3). There
   is no global mode switch.
7. **No compiler at scan time.** The frontend needs no Fortran compiler, module files or build
   tree. It runs on sources alone.
8. **Case-insensitive identities.** Fortran names are case-insensitive, so the frontend lowercases
   every name it emits.
9. **Never emit a duplicate identity.** The ingestor panics on a residual same-kind FQN
   collision (AGENTS.md). The frontend therefore drops a second declaration with the same
   identity and reports it (§11).

## 2. Names and placement

| Item | Name |
|---|---|
| Language id (stream id, `lang_switch`) | `fortran` |
| Crate directory | `src/fortranlib` (standalone Cargo crate, not in the root workspace, like `src/pylib`) |
| Binary | `fortranfrontend` |
| Homebrew formula | `apg-fortran` |
| Env var | `APG_FRONTEND_FORTRAN` |
| `APG_BUILD_FRONTENDS` value | `fortran` |
| Id prefix (`--id-prefix`) | `fo` (proposed, §19 O12) |

**Source extensions.** These are gfortran's own driver suffixes (§23.1), matched
case-sensitively on the stored file name, because the uppercase names select preprocessing:

| Form | Not preprocessed | Preprocessed |
|---|---|---|
| Fixed form | `.f`, `.for`, `.ftn`, `.fi` | `.F`, `.FOR`, `.FTN`, `.fpp`, `.FPP` |
| Free form | `.f90`, `.f95`, `.f03`, `.f08`, `.fii` | `.F90`, `.F95`, `.F03`, `.F08` |

- `.fi` and `.fii` are gfortran's own names for preprocessed output. gfortran accepts them as
  source, so the scanner does too.
- `.f77` and `.f18` are not gfortran suffixes. gfortran passes them to the linker as object
  files, so the scanner does not claim them (decision D3, §21.2).
- The lowercase and uppercase forms of the same name are not interchangeable. Whether
  preprocessing applies is decided by the stored name only, not by the host filesystem. The
  manual makes gfortran's automatic rule depend on case-preserving filenames (§23.2). The
  scanner's results therefore do not vary by machine.

**Include files** (§5): `.inc`, `.fh`. They are not compiled on their own. Each gets a `File`
node.

Any other extension is not Fortran and is not claimed by this frontend.

## 3. Source form

- The form comes from the extension (§2). gfortran's `-ffixed-form` and `-ffree-form` set the
  form per compilation; the extension rule is their default (§23.3). There is no
  column-based detection, unlike COBOL.
- **Fixed form** (`.f`, `.for`, `.ftn`, `.fi`, `.F`, `.FOR`, `.FTN`, `.fpp`, `.FPP`): columns
  1–5 are the statement label. Column 6 marks a continuation. Columns 7–72 hold the statement.
  **Columns 73 onward are ignored** (gfortran's default `-ffixed-line-length-72`, §23.3). A `c`,
  `C`, `*` or `!` in column 1 starts a comment.
- **Free form** (`.f90`, `.f95`, `.f03`, `.f08`, `.fii`, `.F90`, `.F95`, `.F03`, `.F08`): `!`
  starts a comment, `&` continues a line. **Columns 133 onward are ignored** (gfortran's default
  `-ffree-line-length-132`, §23.3). Whether the scanner truncates like gfortran or reads the
  whole line is decided in §21.2 (D2).
- **Sentinels** are comments unless their option is on (§23.4). `!$` (conditional compilation),
  `!$omp` and `c$omp`/`*$omp` in fixed form are comments unless `-fopenmp` or `-fopenmp-simd`
  is given. `!$acc`, and its fixed-form forms, are comments unless `-fopenacc` is given.
- **Names.** `$` is not a valid name character by default (`-fdollar-ok` is off). Backslashes
  in strings are literal (`-fbackslash` is off).
- Whether the grammar parses fixed form directly, or whether the frontend must normalise it,
  is decided in P0 (O2).

## 4. Project layouts

Layouts the scanner must handle, and how each is treated. None of them is required. The scanner
finds sources by extension, not by project marker.

| Layout | Typical content | Treatment |
|---|---|---|
| `src/` | Library: one module per file, usually named after the module | Module `Struct`s under their `File`s (§10) |
| `app/`, `example/`, `examples/` | Main programs | `program` units; `code_type` `src` |
| `test/`, `tests/` | Test programs, often `test_*.f90` | `code_type` `test` (§12) |
| `include/` | `.inc`, `.fh`, sometimes `.h` | Include files (§5). `.h` is not Fortran and is not claimed |
| `external/`, `extern/`, `thirdparty/`, `vendor/` | Bundled dependencies | `code_type` `external` (§12) |
| `build/` | Build output: `.mod`/`.smod`, objects, sometimes preprocessed copies of sources | **Not scanned** (§13) |
| Legacy trees | `src/*.f` with many free-standing subroutines, `COMMON`, `EXTERNAL`, `INCLUDE`d `.inc` | Procedures are top-level `Function`s; calls resolve by unique name (§9) |

Project marker files (`fpm.toml`, `CMakeLists.txt`, `Makefile`) are not required and not read.

## 5. Preprocessing and includes

**When preprocessing applies.** Only to the preprocessed extensions in §2 (the right-hand
column). gfortran runs its C preprocessor for those files, and for any other file only when
`-cpp` is given (§23.2). Preprocessing is therefore not triggered by the presence of `#` lines.
In a file that is not preprocessed, a line beginning with `#` is not a directive, and the
parser reports it as a syntax error.

**Preprocessor mode.** gfortran runs cpp in **traditional mode** (§23.2). The scanner's
directive handling follows traditional cpp.

**Predefined macros.** gfortran predefines macros before any file is read (§23.5). The scanner
follows the decision in §21.2 (D1):

- `__GFORTRAN__` and `_LANGUAGE_FORTRAN` are defined, as gfortran defines them.
- The version macros (`__GNUC__`, `__GNUC_MINOR__`, `__GNUC_PATCHLEVEL__`) are **not** defined
  (D1, pending). A branch that tests them takes its other arm.
- `_OPENMP` and `_OPENACC` are defined only when the matching option is on. No option is on by
  default, so neither is defined.
- The `__GFC_INT_*` and `__GFC_REAL_*` kind macros depend on the target. They are not defined.
- Target macros (`__linux__`, `__APPLE__`, `__x86_64__`): unverified (O16). Not defined until
  checked.
- No macros come from outside the file: no `-D` and no environment definitions. A `#define` in
  the file is honoured for the rest of that file.

**Conditionals.**
- `#ifdef X` is false, and `#ifndef X` is true, unless `X` is predefined (above) or defined in
  the file.
- `#if` and `#elif` expressions: an undefined identifier is 0, and `defined(X)` follows the same
  rule.
- `#else` and `#endif` are as usual.
- Only the active branch is parsed. Inactive branches produce no facts.
- A directive the frontend does not understand is a diagnostic; its line is skipped.

Rationale: a branch parsed in both arms would declare the same identity twice (§11). The rule
is deterministic and matches a plain compile without options.

**Includes.** `#include "file"` is preprocessed, and `INCLUDE 'file'` is not (§23.2). This is
gfortran's documented behaviour.

- `INCLUDE` text is parsed raw, so a `#` line inside an `INCLUDE`d file is a syntax error.
- An included file is parsed in the context of the unit that includes it, so its declarations
  belong to that unit. Its facts are located in the include file (§10).
- Search order: (1) the directory of the including file; (2) the scan root; (3) directories
  given by the repeatable `--include-path` flag, in order (§14).
  - Step 3 corresponds to gfortran's `-I`, which gfortran uses for both `INCLUDE` and `#include`
    (§23.6).
  - Step 1 is documented for `#include` (§23.6). For `INCLUDE` it is unverified (O17).
  - Step 2 is the scanner's own convention. There is no gfortran equivalent, because gfortran
    has no scan root.
- An include that is not found is a diagnostic at the statement. The scan continues.
- Circular includes and nesting beyond a fixed depth are diagnostics. The depth limit is set in
  P4 (O13).
- Every `.inc` and `.fh` file gets a `File` node, even when nothing includes it.
- The source map (as in COBOL §5) maps every parsed position back to its file, line and column.
  Declarations are located in the file they are written in.

## 6. Dialect scope

The default dialect is `-std=gnu` (§23.4). It is a superset of the latest standard that includes
GNU extensions, with warnings for obsolete features. The scanner accepts the same superset.

| Dialect | Scope |
|---|---|
| Fortran 77 | Full syntax as the grammar supports it: fixed form, `COMMON`, `EXTERNAL`, `DATA`, `SAVE`, `ENTRY`. Statement functions: OPEN (O8). Computed `GOTO` and arithmetic `IF` are parsed where the grammar allows; otherwise a diagnostic |
| Fortran 90/95 | Full: modules, `use` with `only` and `rename`, derived types, interfaces, internal procedures (`contains`), `recursive`, `pure`, `elemental`, `result`, `where`, `forall`, `select case` |
| Fortran 2003 | Type extension (`extends`), `class` declarations, type-bound procedures, abstract interfaces, procedure pointers (no facts) |
| Fortran 2008 | Submodules (§10), `block` construct, `error stop`, `contiguous`, `do concurrent` |
| GNU extensions | Accepted where the grammar supports them. Not facts-bearing |
| DEC extensions (`-fdec`) | **Not accepted** by default: `STRUCTURE`, `RECORD`, `UNION`, `MAP`, the `.` member separator, DEC `STATIC` and `AUTOMATIC`, and `d` lines as code. A diagnostic (§23.4) |
| Later syntax | Parsed where the grammar supports it. No fact depends on it |
| OpenMP, OpenACC | Comments unless their options are on (§3) |
| Coarrays | Parsed. A coindexed reference is an ordinary data reference. No coarray semantics |
| Out of scope | CUDA Fortran, vendor extensions, preprocessing beyond §5 |

**Reference limits.** The scanner is checked against the grammar, the test corpus (§18), gfortran's
source and manual (§23), and, once installed, the actual gfortran output for the cases in §23.
Where no source confirms a construct, the scanner emits a diagnostic rather than guessing.

## 7. Parser and error recovery

- The parser is the **tree-sitter-fortran** grammar through the tree-sitter runtime. The choice
  of crate or vendored layer is decided in P0 (§16). The frontend owns all semantic analysis:
  scopes, resolution and fact emission.
- An `ERROR` or `MISSING` node produces a diagnostic at its line and column. The enclosing
  statement is skipped, and the scan continues.
- A file is skipped entirely only if no unit in it can be recovered. The `File` node is always
  emitted.
- Declarations that parsed are emitted even when the file has errors.
- **Keywords are contextual.** Fortran has no reserved words. `type`, `module`, `procedure`,
  `interface`, `use`, `data`, `result` and others are valid identifiers in many positions. A
  program that uses them as variable names must produce the same facts as before. This is a
  regression test (§18).

## 8. Source normalisation

Only if P0 shows the grammar cannot parse a construct in §3 or §6: the frontend normalises the
text (for example, fixed-form layout) before parsing. Normalisation must keep the original
position of every token for the source map (§5).

## 9. Resolution rules

### 9.1 Name lookup

To resolve a name `n` referenced inside unit `U`, the frontend searches in this order and stops
at the first match:

1. Declarations of `U`: locals, dummy arguments, names declared `external` or in an `interface`
   block.
2. Host association: the enclosing procedures and the enclosing module, for internal procedures.
3. `use` statements in `U` and its enclosing units. An `only` list is checked first; a module
   without an `only` list contributes all its public names. Module entities are public by
   default (`-fmodule-private` is off, §23.4).
4. Top-level procedures in the same file (legacy layout).
5. Top-level procedures anywhere in the scan with that name, if there is exactly one. If there
   are several, the reference is unresolved and a diagnostic names all candidates.
6. A standard intrinsic procedure name (§9.9): `builtin`.
7. Otherwise: unresolved.

A local declaration or a use-associated name shadows an intrinsic, as in Fortran.

### 9.2 `name(args)`

- If the lookup finds a **variable** (a declared array, scalar or dummy argument), it is an array
  or substring reference. It produces no call.
- If the lookup finds a **procedure**, it is a function reference: a call (§10).
- If the lookup finds **nothing** in `U`, it is a function reference. Implicit typing is on by
  default (`-fimplicit-none` is off, §23.4). An undeclared name with parentheses is therefore
  not an array, and the reference continues at step 4 of §9.1.

### 9.3 `call name`

The same lookup, restricted to procedures. Not found: unresolved call.

### 9.4 Generic interfaces (decided: unresolved)

A reference through a generic name (a name declared by a `generic` interface block, or a generic
type-bound binding) is an unresolved call, category `unknown`, with a diagnostic naming the
generic. Choosing a specific procedure needs argument types, which the scanner does not model.
Emitting calls to every specific procedure would be a guess.

### 9.5 Interface blocks and `external`

A name declared only by an `interface` body or an `external` statement is a reference to a
procedure defined elsewhere. Resolution follows §9.1 step 5. If no body exists anywhere in the
scan, the reference is an unresolved call, category `external`.

### 9.6 Type-bound procedures

`obj%bind(...)` resolves the declared type of `obj` (by §9.1), then the binding name, then its
implementation. An implementation is either the target of `procedure :: bind => impl`, or the
module procedure named `bind` when there is no `=>`. The call edge goes to the implementation
`Function`. A binding has no node of its own (§10). An unknown type is an unresolved call,
category `unknown`. A generic binding follows §9.4.

### 9.7 Submodules (decided)

A `module procedure` or `module subroutine` body in a `submodule` belongs to the **ancestor
module's** identity. A reference resolves to the same `Function` whichever file holds the body.
The ancestor is the top-most module in the submodule chain.

### 9.8 Modules and `use`

- `use m` with `m` in the scan: a `uses` edge (§10).
- `use m` with `m` not in the scan:
  - an intrinsic module (`iso_fortran_env`, `iso_c_binding`, `ieee_arithmetic`,
    `ieee_exceptions`, `ieee_features`): unresolved use, category `stdlib`;
  - any other module (`mpi`, `omp_lib`, third-party): unresolved use, category `external`.
- A call or reference whose name is use-associated from an out-of-scan module is an unresolved
  call, category `external`.
- gfortran's `USE` finds `.mod` files through `-I` and `-J` (§23.6). The scanner reads no `.mod`
  files, so the rule above applies to every module not in the scan.

### 9.9 Intrinsics

Intrinsic procedure names come from a fixed list of the standard's intrinsic procedures (source
for the list: O7). A reference to one that no declaration shadows is an unresolved call,
category `builtin`, with the lowercased name as the target.

### 9.10 Derived types

`type(t)` or `class(t)` in a declaration is a type reference to the `Struct` `t` in scope (§10).
Components and their access (`p%x`) produce no facts beyond the type reference.

### 9.11 Include files

An included file's references resolve in the including unit's scope (§5).

## 10. Fact mapping

The frontend emits no `code_type`. The ingestor computes it from the path and `apg/config.json`
(§12). Module identities are scan-relative directories, rooted by the ingestor as `fortran.<dir>`
(the same convention as COBOL §10).

### 10.1 Schema constraints relied on

As recorded for COBOL (`plans/SPEC-cobol-frontend.md` §10.1), to be re-verified against
`src/load/tables.rs` in P1:

- `Contains` allows Module→Module, Module→File, File→Struct, File→Function, Struct→Struct and
  Struct→Function. **Function→Function is not allowed.** This is why a procedure with internal
  procedures becomes a `Struct` (below).
- `Calls` is Function→Function.
- `Uses` is Function→Struct and Struct→Struct.
- `UnresolvedCall` is Function→UnresolvedTarget. `UnresolvedUse` is Function→UnresolvedTarget and
  Struct→UnresolvedTarget.

**Edge source.** A reference is emitted from its innermost enclosing unit: a `Function` or a
`Struct`. A module-level declaration (for example a parameter initialised by a function call) is
emitted from the module `Struct`. An item outside any unit has no edge.

### 10.2 Table

| Fortran construct | Fact |
|---|---|
| Each scanned file, include files included | `File`, parent `Module` = its directory module |
| Directory | `Module`, rooted `fortran.<dir>` |
| `module m` | `Struct` `m`, contained by its `File` |
| `submodule (p) c` | `Struct` `c`, contained by its `File`; `uses` edge to the parent module `Struct` `p` |
| Procedure (`subroutine`, `function`) in a module | `Function`, contained by the module `Struct` |
| `module procedure` body in a submodule | `Function`, contained by the **ancestor** module `Struct` (§9.7) |
| Top-level procedure outside any module (legacy) | `Function`, contained by its `File`; FQN rooted at the directory module |
| `program` unit | `Function` (or `Struct` when it has internal procedures, as below) |
| Procedure with `contains` (internal procedures) | Host becomes a `Struct`, placed as its `Function` would be. Each internal procedure is a `Function` contained by the host `Struct`. A reference to the host is `uses`, not `calls` |
| Derived `type` | `Struct`, contained by its module `Struct` (or its `File`, when outside a module) |
| Type-bound binding (`procedure :: bind => impl`) | **No node.** Calls through it resolve to the implementation (§9.6) |
| Implementation of a binding | The module procedure it names: a normal `Function` |
| Module variable, module parameter, named constant | No node. A reference from `U` is `uses` `U → owning module Struct` |
| Dummy argument, local variable | No node |
| `COMMON` block member | No node, no edge (O5) |
| `namelist`, `data`, `equivalence`, `save` | No node |
| `interface` body for a procedure defined elsewhere | No node (§9.5) |
| Abstract interface | No node |
| Procedure pointer | No node |
| Statement function | OPEN (O8) |
| `call s` or `s(...)` resolved to a `Function` | `calls` (Function→Function) |
| `call s` or `s(...)` resolved to a host `Struct` (internal-procedure host) | `uses` (Function→Struct) |
| Call unresolved | `unresolved_call`, category per §9 (`unknown`, `external` or `builtin`) |
| Intrinsic call | `unresolved_call`, category `builtin` (§9.9) |
| `use m`, `m` in scan | `uses` from the innermost enclosing unit to the module `Struct` `m` |
| `use m`, `m` not in scan | `unresolved_use`, category `stdlib` (intrinsic module) or `external` |
| `type(t)` or `class(t)` in a declaration | `uses` from the enclosing unit to `Struct` `t`; `unresolved_use` `external` when `t` is not in the scan |
| `ENTRY` statement | OPEN (O15) |
| `INCLUDE` / `#include` of a file | The included file's `File` node; its declarations located in it |

## 11. Identity rules

- Every name is lowercased (§1, principle 8).
- Parent identities are emitted verbatim as §10 describes. The ingestor roots them and renders
  FQNs (SPEC §4).
- A `Function`'s `params` list is the normalised type of each dummy argument in order, for
  example `integer`, `real(8)`, `character(*)`, `type(point)`, `class(shape)`. The existing
  overload rule uses it when two units share a parent and name.
- **Duplicate identity.** If two declarations have the same parent, name and `params`, the frontend
  keeps the first, drops the second, and reports a diagnostic naming both locations. This avoids
  the ingestor's panic. The same rule applies to a duplicate top-level `program` in one directory.

## 12. Code types

A built-in `fortran` arm in `src/classify.rs`. The first matching rule wins:

| Code type | Rule (case-insensitive) |
|---|---|
| `test` | a `test` or `tests` path segment; or a filename matching `test_*` or `*_test.*` |
| `generated` | a `gen` or `generated` path segment |
| `external` | a `vendor`, `external`, `extern`, `third_party` or `thirdparty` path segment |
| `src` | everything else, including `app/`, `example/`, `include/` |

`build/` is never scanned (§13), so it has no code type.

## 13. Scan scope

- The frontend walks the project for the source extensions in §2, plus include files.
- Hidden directories are skipped, by the walker's standard rule.
- `build/` is skipped. This is a deliberate exception to the principle that all code is included.
  Build trees can contain preprocessed copies of sources, and those copies would declare the same
  identities as the originals (§11). A config key to change this is O6.
- Paths matched by `--exclude-path` globs are skipped.
- `--targets` is honoured: only facts of files in the target list are emitted. Resolution still
  uses the full context (`global.constraint.frontend-full-context`).
- Cache persistence (`--cache-dir`, `--cache-key`) is an OPEN item for P4 (O10).

## 14. Command line

```
fortranfrontend <project_dir> [--module <dir>]... [--id-prefix <p>] [--include-path <dir>]...
                [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude_globs...]
```

- `--module`, `--id-prefix`, `--targets`, `--cache-dir`, `--cache-key` and the exclude globs
  follow the handoff contract in `src/frontends.rs`.
- `--include-path` is new. It corresponds to gfortran's `-I` (§5, §23.6). `apg scan` gains a
  repeatable `--include-path` flag, and `spawn_frontend` passes it through. This is a
  shared-code change, the same kind as COBOL's `--copy-path`.

## 15. APG integration

| File | Change |
|---|---|
| `build.rs` | Add `src/fortranlib` to the crate list and a build step like `pylib`'s; `fortran` in `APG_BUILD_FRONTENDS` |
| `src/frontends.rs` | Detector entry (source extensions in §2, depth 8, skip set `target`, `node_modules`, `build`); binary lookup; `APG_FRONTEND_FORTRAN`; `id_prefix_for` → `fo`; `--include-path` passed by `spawn_frontend`. No scan-time tool (`scan_time_tools` returns none) |
| `src/incremental/language.rs` | Fortran extensions in §2 → `fortran`. **Changes graph output:** these files are currently `misc` |
| `src/classify.rs` | `fortran` arm in `builtin_code_type` (§12) |
| `src/impact/signature.rs` | `"fortran" => Granularity::File` |
| `src/structlib` discovery | Stop claiming the Fortran extensions in §2 |
| `src/lib.rs` | `fortran` in the `--language` usage text; `--include-path` on `apg scan` |
| `install.sh` | `fortran` in `ALL_FRONTENDS`, the component table and the install/check arms |
| `rust-toolchain.toml` | Add `src/fortranlib` to the comment listing the cargo frontend crates (the toolchain pin is shared) |
| `.gitignore` | `/src/fortranlib/target` |
| Packaging | `Formula/apg-fortran.rb`; `scripts/release.sh` and `.github/workflows/release.yml` from 8 to 9 formulae; Linux `fortranfrontend` tarball |
| Docs | README frontend dependency table; CHANGELOG, noting the `misc` → `fortran` graph change |

**Installed-binary rule (AGENTS.md).** The frontend emits identities verbatim and never
pre-roots them. The `fortran.<dir>...` FQNs in §10 are rendered by the ingestor. Plan targets and
planned nodes use the FQNs the installed binary renders.

**Impact.** Granularity is `File`. Cross-file dependencies come from `calls` and `uses` edges
(`src/impact/depindex.rs`). The `use` edges in §10.2 are what make a changed module reach the
files that use it.

**Build dependency.** Stable Rust and crates.io, or a git revision if the vendored layer is used
(§16). Nothing is spawned at scan time.

## 16. Parsing base (decision A — P0 decides)

Two candidates. The spike decides between them.

1. **Vendored `fortitude_sitter` layer.** `fortitude` (PlasmaFAIR, MIT) is a Fortran linter in
   Rust. Its `fortitude_sitter` crate wraps tree-sitter-fortran and has a typed AST layer
   (`src/ast/`). It is not on crates.io, so it would be copied into `src/fortranlib/vendor/` at a
   pinned revision, with the MIT notice kept. Only the sitter layer is taken. The linter, server,
   workspace and Ruff dependencies are not.
2. **`tree-sitter-fortran` 0.6.0 from crates.io**, with the tree-sitter runtime, and our own
   tree walking.

**Spike criteria** (P0, §20):

- parse-error rate on fixed-form and free-form samples;
- coverage of submodules, `#` directives and type-bound procedures;
- whether the typed AST covers the facts in §10, or only the syntax;
- compatibility with the tree-sitter runtime version. `fortitude` uses the 0.26 line. The
  `cpplib` frontend vendors its own C runtime, so this crate would be the first Rust-runtime
  Fortran dependency and the version must match.

**Licence.** MIT notices are kept with any copied code. The `fortitude` grammar revision is
recorded in the vendored layer's README.

## 17. Failure reporting

Follows COBOL §14:

- One line per error on the frontend's stderr, in the form `fortran: <file>:<line>:<col>: <message>`.
  The scan continues.
- A final count of files with errors, to stderr.
- A file that cannot be read or recovered: one line naming the file.
- A crash reason is the **last** line before exit, so the 10-line stderr tail in
  `spawn_frontend` shows it.

## 18. Testing

Every test sits in one of the three tiers defined by `global.constraint.test-tier-boundaries`.

| Suite | Tier | What it proves |
|---|---|---|
| Source form and lexing | unit / int (in-memory) | fixed and free handling, column limits (72 and 132), sentinels, continuation |
| Extension table | unit / int (in-memory) | every suffix in §2 maps to the stated form and preprocessing flag; `.f77` and `.f18` are not claimed |
| Preprocessor | unit / int (in-memory) | traditional-mode `#if`/`#ifdef`; predefined macros of §5 only (`__GFORTRAN__`, `_LANGUAGE_FORTRAN`); file-local `#define` |
| Parser per construct | unit / int (in-memory) | modules, procedures, derived types, interfaces, submodules, `use` parse to the expected tree |
| Include handling and source map | e2e (reads files) | `#include` preprocessed, `INCLUDE` raw, search order, `--include-path`, circular and missing includes, positions |
| Resolution | int (in-memory) | every rule in §9, including shadowing, host association, legacy unique-name, ambiguity |
| Legacy scan | e2e | a fixed-form tree with free-standing procedures and `INCLUDE`s yields the expected graph |
| Module scan | e2e | a free-form tree with modules, submodules, `use` and derived types yields the §10 facts |
| Keyword regression | e2e | programs using `type`, `module`, `use`, `data` and similar as variable names give the same facts as before |
| Dialect defaults | e2e | `$` in a name is rejected; a DEC-only construct is a diagnostic; a GNU extension the grammar supports is accepted |
| Error recovery | e2e | one bad statement keeps the rest of its unit; the error names file and line |
| Duplicate identity | int | a duplicate `program` or procedure is dropped with a diagnostic and the scan continues |
| Frontend plumbing | e2e | `--language fortran` is accepted; uppercase `.F90` is detected; a scratch repo scan writes the expected graph |

**Corpora.** Hand-written samples first, covering each layout in §4. Test data from `fortitude`
may be used under its MIT notice. External projects need a licence check before any file is
vendored. Tests are self-contained: each e2e scenario builds its own scratch repo under `/tmp`, and
no test reads a developer-local path.

**Oracle.** Once gfortran is installed, the scanner's extension, macro and include behaviour is
checked against gfortran's output for the same inputs (§23.7).

## 19. Open items

| # | Item | Needed by | Status |
|---|---|---|---|
| O2 | Whether the grammar parses fixed form directly, or needs normalisation (§8) | P0 | spike |
| O5 | `COMMON` blocks: no edge (current choice) or an unresolved use. Legacy coupling through `COMMON` is invisible in the graph | P2 | decision after P2 samples |
| O6 | Whether `build/` should be configurable (a `config.json` key) | P4 | decision |
| O7 | Source for the standard intrinsic procedure list (§9.9) | P2 | decision |
| O8 | Statement functions: a `Function` under the enclosing unit or file, or no node | P2 | decision |
| O9 | Reference text for the standard, beyond gfortran's source and manual | P2 | action |
| O10 | Incremental cache support (`--cache-dir`, `--cache-key`) | P4 | decision |
| O11 | Preprocessing trigger and uppercase rule | — | **resolved** from gfortran source (§23.1, §23.2); runtime check in §23.7 |
| O12 | Id prefix `fo` | P1 | confirm |
| O13 | Include nesting depth limit | P4 | decision |
| O14 | Licence checks for any external corpus | before vendoring | action |
| O15 | `ENTRY` statements: a second entry `Function` for the same unit, or no node | P2 | decision |
| O16 | Target macros (`__linux__`, `__APPLE__`, `__x86_64__`): defined by gfortran or not. The source suggests not (§23.5), but this is a reading of comments | P1 | verify with `gfortran -cpp -dM` |
| O17 | Whether `INCLUDE` searches the including file's directory, as `#include` does | P1 | verify with gfortran |
| O18 | Free-form lines beyond 132 columns: truncation diagnostic wording and whether the rest of a truncated line is kept for `&` continuation | P2 | decision |

## 20. Phases

| Phase | Deliverable | Exit criterion |
|---|---|---|
| **P0** Spike | Build the candidate parsing base (§16); measure parse errors on fixed- and free-form samples; check submodule, `#` and type-bound coverage; decide O2 and §16 | Base chosen and recorded; parse-error rate acceptable on the sample set |
| **P1** Declarations | Source form and extension table (§2, §3); preprocessor (§5); `File`/`Module`/`Struct`/`Function` facts (§10); identity rules (§11); `--targets`; O12, O16, O17 | Declaration facts match the expected output on the layout samples; extension table matches §2; schema constraints in §10.1 re-verified |
| **P2** References | Resolution (§9), `calls`, `uses`, unresolved targets, intrinsics, submodule identity | Resolution suite passes; legacy and module e2e scans give the expected graph |
| **P3** Integration | APG touchpoints (§15), `--include-path`, packaging, docs | `apg scan --language fortran` works in a scratch repo; release tooling lists 9 formulae |
| **P4** Hardening | Include depth, cache support (O10), error polish, performance on a large tree | All suites pass; a large scratch tree scans within the agreed time budget |

## 21. Decision log

### 21.1 Decided (accepted in spec review, 2026-10-27)

| Decision | Basis |
|---|---|
| Standalone Rust crate `src/fortranlib`, binary `fortranfrontend`, language id `fortran` | Repo pattern (`pylib`, `structlib`) |
| Form by extension; no column detection | Matches gfortran's default form selection (§23.3) |
| Layout-driven design (§4) | User direction |
| Syntax-only resolution through `use`, module scope and unique names; heuristic tier, like C++ | Recommended; no compiler at scan time |
| `build/` not scanned | Recommended; avoids duplicate identities |
| Submodule procedures under the ancestor module | Recommended; calls resolve to one identity |
| Procedure with internal procedures becomes a `Struct`; references to it are `uses` | Recommended; schema forbids Function→Function containment, as in COBOL sections |
| Generic interface calls unresolved (`unknown`) | Recommended; no guessing |
| Intrinsics → `builtin` | Recommended |
| `use` → `uses` edges (module Struct → module Struct) | Needed for impact: `depindex` builds file dependencies from `calls` and `uses` only |
| Names lowercased; duplicate identities dropped with a diagnostic | Case-insensitivity; ingestor panic rule |
| Include handling as COBOL copybooks: search order, declarations located in the include file | Recommended; step 1 of the order is cited for `#include` only (O17) |
| Default dialect `-std=gnu`; DEC extensions and `$` in names rejected | gfortran defaults (§23.4) |
| Preprocessing only for the preprocessed extensions; traditional mode | gfortran source (§23.2) |
| `#include` preprocessed, `INCLUDE` not | gfortran manual (§23.2) |
| Source extensions as gfortran's suffix table; `.f77` and `.f18` not claimed | gfortran source (§23.1) |
| Sentinels are comments unless `-fopenmp`/`-fopenacc`-equivalent options are on | gfortran manual (§23.4) |

### 21.2 Pending confirmation

These follow the recommendations in the verification discussion. They are recorded as the working
choice and need your confirmation.

| # | Decision | Working choice | Why |
|---|---|---|---|
| D1 | Predefined macros | Define `__GFORTRAN__` and `_LANGUAGE_FORTRAN` only. Leave version macros, `__GFC_*`, `_OPENMP` and `_OPENACC` undefined | gfortran defines the first two always (§23.5). The version macros are compiler-version-dependent, so defining them would be a guess |
| D2 | Free-form columns beyond 132 | Truncate, as gfortran does by default, and emit a diagnostic when non-blank text is cut | Matches what gfortran compiles by default. Reading the whole line would give facts a default compile never sees (O18) |
| D3 | `.f77` and `.f18` | Not claimed | gfortran does not recognise them (§23.1). A project using them must rename files, or the scanner would describe code gfortran never reads |

## 22. Summary of who owns what

- The **frontend** (`src/fortranlib`) owns parsing, preprocessing, scope and resolution, and fact
  emission.
- The **ingestor** owns FQN rendering, `code_type` computation and the graph.
- **Shared code** changes are limited to the touchpoints in §15, including `--include-path`.

## 23. Verification log

Checked against GCC's sources and the gfortran manual on 2026-10-27. gfortran is not installed
on the machine used for the check, so no runtime output was compared. The GCC sources are the
`gcc-mirror` development branch, which can differ from a released compiler. Every claim below is
to be re-checked against a real gfortran (§23.7).

### 23.1 Suffix table

Source: `gcc/fortran/lang-specs.h` (gcc-mirror, master).

- Fixed, cpp-preprocessed (`@f77-cpp-input`): `.F`, `.FOR`, `.FTN`, `.fpp`, `.FPP`.
- Fixed, not preprocessed (`@f77`): `.f`, `.for`, `.ftn`, `.fi`.
- Free, cpp-preprocessed (`@f95-cpp-input`): `.F90`, `.F95`, `.F03`, `.F08`.
- Free, not preprocessed (`@f95`): `.f90`, `.f95`, `.f03`, `.f08`, `.fii`.
- `.f77` and `.f18` do not appear.
- The GCC manual's driver section (§3.2) says a file with no recognised suffix is passed to the
  linker as an object file.

### 23.2 Preprocessing

Sources: manual, "Enable and customize preprocessing"; `gcc/fortran/cpp.cc`
(`gfc_cpp_post_options`).

- The preprocessor runs automatically for `.F`, `.FOR`, `.FTN`, `.fpp`, `.FPP`, `.F90`, `.F95`,
  `.F03` and `.F08`, "on systems with case-preserving file names". `-cpp` forces it on any
  file. `-nocpp` turns it off for those extensions.
- `cpp_option->traditional = 1`: traditional mode.
- `INCLUDE` inside a preprocessed file: the included file is not preprocessed. `#include` is the
  preprocessed form.

### 23.3 Line length and form

Source: manual, "Options controlling Fortran dialect".

- `-ffixed-line-length-n` default 72. Characters after that column are ignored.
- `-ffree-line-length-n` default 132.
- Form: "When neither option is specified, the source form is determined by the file extension."

### 23.4 Dialect defaults

Source: manual, "Options controlling Fortran dialect".

- `-std` default `gnu`: "a superset of the latest Fortran standard that includes all of the
  extensions supported by GNU Fortran". `legacy` is equivalent without the obsolescence warnings.
- `-fdollar-ok`: off. Without it, `$` is not a name character. `-fdec` enables it along with the
  other DEC extensions, and `-fdec` is off.
- `-fbackslash`: off. Backslashes are literal.
- `-fdec`: off. Its extensions include `STRUCTURE`, `RECORD`, `UNION`, `MAP`, `.` as a member
  separator, and `d` lines as code.
- `-fimplicit-none`: off. Implicit typing is on.
- `-fmodule-private`: off. Module entities are public.
- `-fallow-argument-mismatch`: off, and "implied by `-std=legacy`". Argument mismatches are
  errors without it.
- `-fopenmp`: enables `!$omp` and the `!$` conditional-compilation sentinel. `-fopenmp-simd`
  enables `!$` and the simd subset. In fixed form, sentinels may also start with `c` or `*`.
- `-fopenacc`: enables `!$acc`, and in fixed form `c$acc` and `*$acc`.

### 23.5 Predefined macros

Source: `gcc/fortran/cpp.cc` (`cpp_define_builtins`, `gfc_cpp_init`).

- Always defined: `__GFORTRAN__=1`, `_LANGUAGE_FORTRAN=1`, and the language-independent builtins
  from `define_language_independent_builtin_macros`. The manual documents `__GNUC__`,
  `__GNUC_MINOR__` and `__GNUC_PATCHLEVEL__` among these.
- `_OPENACC=201711` only with `-fopenacc`. `_OPENMP=202111` only with `-fopenmp`.
- `__GFC_INT_1__`, `__GFC_INT_2__`, `__GFC_INT_8__`, `__GFC_INT_16__`, `__GFC_REAL_10__` and
  `__GFC_REAL_16__`: defined when the target supports the kind.
- The target CPU and OS builtin macros are commented out in `cpp.cc` ("FIXME: Pandora's Box").
  Whether `__linux__`, `__APPLE__` or `__x86_64__` are therefore absent is not confirmed (O16).
- `-undef` suppresses the predefined macros (`gfc_cpp_option.no_predefined`).

### 23.6 Include search

Sources: manual, "Options for directory search" and "Options controlling the preprocessor".

- `-I` "affects interpretation of the `INCLUDE` directive (as well as of the `#include` directive
  of the `cpp` preprocessor)".
- `-J` names the directory for compiled `.mod` files, and `USE` searches `-I` and `-J` for them.
- `-nostdinc`: "Only the directories you have specified with `-I` options (and the directory of
  the current file, if appropriate) are searched." This is documented for `cpp`'s `#include`.
  Whether `INCLUDE` also searches the current file's directory is not stated (O17).

### 23.7 Still to verify

With gfortran installed:

1. `gfortran -cpp -dM -E` on an empty file: the predefined macros (§23.5, O16).
2. A `.f77` file and a `.f18` file compiled by name: confirm gfortran rejects them (§23.1).
3. An `INCLUDE` whose target is in the including file's directory, with no `-I`: confirm where it
   is found (O17).
4. A free-form line longer than 132 columns, with and without `-ffree-line-length-none`: confirm
   the truncation (D2, O18).
5. A `#define` in a `.F90` file, then `#ifdef` on that name, to confirm the file-local rule.
