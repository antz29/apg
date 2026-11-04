# SPEC — `cobol` frontend (`cobolfrontend`)

A standalone Rust frontend that scans COBOL source (COBOL-85, COBOL 2002 object-oriented
syntax, and IBM `EXEC CICS`/`EXEC SQL` blocks) and streams the unified JSONL facts
(SPEC §2) that the ingestor turns into the graph. Status: **planned, not implemented.**
Items marked **OPEN** are not yet decided; they are collected in §15. Decisions already made
are recorded in §18.

## 1. Governing principles

1. **Facts only.** The frontend emits declarations, references and edges. It never computes
   canonical FQNs and never assembles the graph; the ingestor does both (SPEC §2, §4).
2. **Same as the other languages.** Every mapping decision follows what the existing frontends
   already do: classes and their members as `Struct`/`Function`, top-level units contained by
   their `File`, class targets of calls as `uses`, unresolved targets with a fixed category.
   Where COBOL has no precedent, the decision is recorded here and flagged.
3. **Never guess.** A reference that cannot be resolved to a project symbol is emitted as an
   unresolved target, never as a fabricated FQN.
4. **Tolerant like the C++ frontend.** A syntax error costs the statements it touches, not the
   file, and never the scan. Every error is reported with its file and line.
5. **One scanner, dialect-gated.** The dialect (`cobol.dialect`, §2) controls OO recognition,
   reserved words, the default format and feature-level diagnostics (§3, §6, §8, §14).
   The dialects are `cobol85`, `cobol2002`, `cobol2014` and `ibm`. Within a dialect the parser still decides keywords
   by context. The dialect is set explicitly, never guessed from the source.
6. **Hand-written and owned by us.** The parser, preprocessor and AST are written in Rust in
   this repository. No external COBOL parser, grammar or runtime is a dependency. External
   COBOL sources are used as test data only, after a licence check.

## 2. Names and placement

| Item | Name |
|---|---|
| Language id (stream id, `lang_switch`) | `cobol` |
| Crate directory | `src/cobollib` (standalone Cargo crate, not in the root workspace, like `src/pylib`) |
| Binary | `cobolfrontend` |
| Homebrew formula | `apg-cobol` |
| Env var | `APG_FRONTEND_COBOL` |
| `APG_BUILD_FRONTENDS` value | `cobol` |
| Dialect config key | `cobol.dialect` in `apg/config.json`; default `cobol85` (decided) |
| Dialect flag | `--std <dialect>` on `apg scan`, overrides the config key for that run; passed to the frontend as `-std=<dialect>` (decided) |
| File extensions (case-insensitive) | `.cob`, `.cbl`, `.cobol`; `.cpy` (copybook, see §5) |

## 3. Source formats

- **The dialect sets the default format** (decided). `cobol85` and `ibm` default to the `cobol85`
  format, which is the fixed layout with area A checks (§19.1). `cobol2002` and `cobol2014`
  default to fixed format.
- Detection order, per file (O22, decided):
  1. A `>>SOURCE` directive, if present, in the form `>>SOURCE [FORMAT] [IS] {FIXED|FREE|COBOL85|VARIABLE|XOPEN|XCARD|CRT|TERMINAL|COBOLX}` (GnuCOBOL manual §2.1.3, §19.1).
  2. Otherwise fixed format. Columns 1–6
     are the sequence area, column 7 the indicator (` `, `*`, `-`, `D`), columns 8–72 the program text, and columns 73–80 are ignored.
     `/` and `$` are not indicators in this format; they are comments only under `-fmfcomment` and `-facucomment` (§19.1).
  3. A heuristic check flags a likely free-format file (for example, code in column 1 or text past column 72) and emits a warning naming the file and the reason. The file is still read as fixed.
- The dialect (`cobol.dialect`, §2) is the only language setting (decided). Other frontends derive settings from project markers, and no frontend takes a user-set language standard today: Java fixes `--release` at build time, and Python and Rust read project markers. The dialect is therefore the first such setting. The other
  frontends' markers (Go `go.mod`, Python `pyproject.toml`, Rust edition guessed by rust-analyzer)
  have no COBOL equivalent, so the dialect is set in config.
- In fixed format, the sequence area (columns 1–6) and the identification area (columns 73–80)
  are ignored; continuation lines (indicator `-`) are joined.
- **Area A checks** (decided, `cobol85` format and therefore `ibm`): division, section, paragraph
  and 01/77 level names must start in area A; statements and separator periods must not start in
  area A. Each violation is a diagnostic (§19.1).

## 4. Architecture

```
source file
  → format detection (§3)
  → lexer (tokens, with byte offsets and line/column)
  → preprocessor (COPY, REPLACE, COPY … REPLACING; source map, §5)
  → parser (recursive descent per division; recovery, §7)
  → AST (§8)
  → resolver (paragraphs, sections, programs, qualified data names; §9)
  → scanner (facts, §10)
  → JSONL on stdout; diagnostics on stderr
```

Modules, indicative: `lexer`, `format`, `preprocess` (with `copy_resolver`, `replace`,
`source_map`), `parser` (one module per division plus `oo`, `exec`), `ast`, `resolve`, `scan`,
`diag`, `main`. Tests sit in `src/cobollib/src/tests.rs` (unit/int) and
`src/cobollib/tests/cobollib_e2e.rs` (e2e), following the frontend crates' existing layout.

## 5. Preprocessor and source map

- COPY and REPLACE are expanded before parsing. The expanded text is what the parser sees.
- Every expanded byte maps back to an original `(file, line, column)`. Declaration locations
  and every diagnostic use the original location, so a declaration inside a copybook is located
  in the copybook file (same rule as C++ headers).
- Nested COPY is supported with a depth limit and circular-COPY detection, each reported as a
  diagnostic.
- A copybook is a `File` node of its own, like a C++ header. A copybook that is not found is a
  diagnostic at the COPY statement; the scan continues.
- **Copybook search path (decided).** Directories are searched in this order:
  1. the directory of the including file;
  2. the scan root;
  3. `copybooks/` and `copy/` under the scan root.

  Extra directories are given with a repeatable `--copy-path` flag on `apg scan`, the same way
  `--exclude-path` is passed today. GnuCOBOL takes the same kind of setting from `-I` (§19.2).
  `COB_COPY_DIR` does not appear in the manual, so it is not cited here. The manual documents `-I` only; the `copybooks/` and `copy/` defaults are our convention, not cobc's. No `apg/config.json` key is added for the copybook path.

## 6. Dialect scope

| Dialect | Scope |
|---|---|
| `cobol85` (default) | Full COBOL-85 scope: all divisions, sections and paragraphs, declaratives, all statements, data description including level-88 and OCCURS/REDEFINES/RENAMES. OO recognition is off |
| `cobol2002`, `cobol2014` | COBOL 2002 OO, enabled by the dialect: `CLASS-ID` (with `AS "external"` name), `INTERFACE-ID`, `METHOD-ID`, `FACTORY` and `OBJECT` sections, `INHERITS`, `IMPLEMENTS`, `REPOSITORY` class entries, `OBJECT REFERENCE`, `INVOKE` (instance, factory and `SUPER`), `SELF`, `SUPER`, `NULL`, and `PROPERTY` and `GET` in the Micro Focus form (vendor form, §10) |
| `cobol2023` | Dialect value added as an empty placeholder (decided); features are filled in later (P7). Selecting it refuses with a message until then (decided). The standard COBOL 2023 syntax for `PROPERTY`, `GET` and `SET` stays **OPEN (O7)**; GnuCOBOL has no reference for it |
| `ibm` (v1, phase P4) | IBM reserved words (§8), the `cobol85` default format with area A checks, and `EXEC CICS … END-EXEC` and `EXEC SQL … END-EXEC` kept as typed blocks where the scanner needs them (§9). EXEC blocks are recognised only under `ibm`, and OO recognition is on (decided, O26) |
| Out of scope | COBOL 2014/2023 JSON/XML statements, `FUNCTION-ID`/`TYPEDEF`, report writer, `SCREEN SECTION`, `COMMUNICATION SECTION` (parsed as opaque blocks so they cannot break a file) |

**Dialect values for v1** (decided): `cobol85` (default), `cobol2002`, `cobol2014` and `ibm`.
`xopen`, `mf`, `acu` and `rm` are added later, on demand. The per-dialect feature table is ours,
written from the GnuCOBOL manual (§19), not copied from GnuCOBOL's GPL config files (decided).
OO recognition is on under `cobol2002`, `cobol2014` and `ibm`, and off under `cobol85`. EXEC blocks are recognised only under `ibm` (decided, O26).

**Reference limits.** No COBOL standard text or reference compiler is available to the project.
Syntax is checked against the NIST CCVS85 suite (COBOL-85), against the examples in the
MainframeMaster COBOL object-oriented tutorial (OO), and against user-supplied programs. The
GnuCOBOL compiler source is used as a secondary reference for COBOL-85 behaviour, and the
GnuCOBOL 3.2-dev manual (§19) for syntax, reserved words and compiler options. Where no
source confirms a clause, the parser rejects it with a diagnostic rather than guessing.

## 7. Parser and error recovery

- Recursive descent, one function per construct, no backtracking beyond bounded lookahead.
- **Recovery.** On a syntax error inside a sentence, the parser records a diagnostic, skips to
  the next sentence-ending period, and resumes. Division, section and paragraph headers are
  synchronisation points: a broken paragraph loses only that paragraph's statements.
- A file is skipped entirely only when its divisions cannot be recovered, and then a diagnostic
  names the file and the first error. Every other file is unaffected.
- **Framework test files** (for example the TESTSUITE, TESTCASE, EXPECT and MOCK syntax of
  `cobol-unit-test`, which is precompiler input and not COBOL) are not valid COBOL. Their File
  node is always emitted, with `code_type` from §11. Each unparseable construct produces a
  diagnostic, and the scan continues. Their declarations that parse are emitted (decided, O12).
- Declarations that parsed are emitted even when the file has errors, so the graph keeps all
  the units that were understood.

## 8. Keyword policy (contextual OO keywords)

- The OO keywords `CLASS`, `OBJECT`, `METHOD`, `FACTORY`, `INTERFACE`, `SELF`, `SUPER`, `NEW`,
  `NULL`, `PROPERTY`, `GET`, `INHERITS`, `IMPLEMENTS` are not reserved under `cobol85`, where only `CLASS` is reserved (GnuCOBOL's COBOL-85 word list),
  `cobol2002` or `cobol2014`, where most are reserved (§19.4, O26). Where a dialect does not reserve one of them, it is
  keywords only where the grammar expects them (for example `CLASS-ID` at the start of an
  identification division, `OBJECT REFERENCE` in a data description, `INVOKE` in a procedure).
  Everywhere else they are ordinary identifiers.
- **`ibm`** (decided). The IBM word list reserves `CLASS`, `CLASS-ID`, `METHOD`, `METHOD-ID`,
  `OBJECT`, `INVOKE`, `INHERITS`, `SELF`, `SUPER`, `ENTRY`, `ALTER` and others (§19.4). Using one of
  them as a data name is an **error** under `ibm`.
- A COBOL-85 program that uses any of the non-reserved OO words as a data name must parse exactly as it did before
  OO support was added. This is tested (§12, keyword regression).

## 9. Resolution rules

- **Paragraphs and sections.** `PERFORM p` and `GO TO p` resolve to the paragraph Function in
  the same program. Qualified names (`p OF s`, `p IN s`) resolve through the section. A name
  that matches more than one paragraph and is not qualified is an unresolved reference with a
  diagnostic (see §10, unresolved `PERFORM`/`GO TO`).
- **`PERFORM p THRU q`** (decided): the range is every paragraph from `p` to `q` in source order,
  including paragraphs in other sections, and each one is a `calls` target.
- **Programs.** `CALL "NAME"` (literal) resolves to the program whose `PROGRAM-ID` (or
  `CLASS-ID`) matches. Matching is **exact first**; if that fails, a **case-insensitive** match
  is used and a warning is emitted (decided). GnuCOBOL matches `CALL` names case-sensitively by
  default (§19.2); see O23. `CALL identifier` (dynamic) is never resolved; it
  is an unresolved call. Literal `CALL` names resolve within the scan by `PROGRAM-ID`; cobc resolves them at run time through `COB_LIBRARY_PATH` (§19.2), so this is a scan-level rule of ours.
- **EXEC CICS.** `LINK PROGRAM('X')` and `XCTL PROGRAM('X')` are program calls, resolved like
  `CALL "X"`. `START TRANSID`, `RETURN TRANSID` and other transaction commands are **not**
  program calls and emit no program edge. **Decided (O4):** `EXEC CICS START` and
  `RETURN TRANSID` stay out of the call graph.
- **External invocations** (decided, following the Java frontend). A call whose target is outside
  the scan emits `unresolved_call` with category `external`. This covers `CALL`, `LINK`, `XCTL`,
  `EXEC CICS INVOKE SERVICE` and `WEBSERVICE`, and `EXEC SQL CALL` to a procedure. A resolved COBOL
  program target stays `uses`. A `LINK` or `XCTL` with a variable `PROGRAM(name)` is dynamic:
  `unresolved_call` with category `func-value`. SQL user-defined functions stay inside the opaque
  EXEC SQL block and emit no edge.
- **EXEC rules are unverified.** The EXEC CICS and EXEC SQL rules in this section have no reference in GnuCOBOL, the other scanners or the sources so far. They stay unverified until an IBM source is found (O27).
- **EXEC SQL.** Kept opaque. Host variables referenced in the block are data references and are
  resolved like any other data reference. Table names are not graph nodes in v1.
- **Data names.** Qualified data references resolve through the enclosing record (`OF`/`IN`).
  A reference that cannot be resolved is an unresolved use, never a guessed record.
- **Methods.** `INVOKE obj "m"` resolves through the declared class of `obj` (its `OBJECT
  REFERENCE` type), as the static type. `INVOKE SUPER "m"` resolves through the parent class.
  `INVOKE Class "m"` resolves to the factory method of that class.

## 10. Fact mapping

The frontend emits no `code_type`. The ingestor computes it from the path and `apg/config.json`
(§11), as for every other language.

### 10.1 Schema constraints this mapping relies on

Verified in `src/load/tables.rs`:

- `Contains` allows Language→Module, Module→Module, Module→File, File→Struct, File→Function,
  Struct→Struct and Struct→Function. **Function→Function and Module→Function containment are not allowed**, which is
  why sections are Structs (§10, Section).
- `Calls` is Function→Function and stores edge properties, so `kind` can be recorded (below). `kind` is the first edge property any frontend emits: the ingestor stores edge properties, but no existing frontend emits them, and the query tools do not read them yet.
- `Uses` is Function→Struct and Struct→Struct.
- `UnresolvedCall` is Function→UnresolvedTarget and carries only `target_type`, so the
  control-transfer kind cannot be stored on an unresolved edge.
- `UnresolvedUse` is Function→UnresolvedTarget and Struct→UnresolvedTarget.

### 10.2 Table

| COBOL construct | Fact |
|---|---|
| Each scanned file | `File` node, parent `Module` = its directory module |
| Directory | `Module` node, rooted `cobol.<dir>` |
| `PROGRAM-ID` | `Struct` (`cobol.<dir>.<NAME>`), contained by its File |
| `CLASS-ID` | `Struct`, contained by its File |
| Nested program | `Struct`, contained by the enclosing program's Struct |
| Section (including declarative sections) | `Struct`, contained by its program's Struct. Class-like: a container of paragraphs |
| Paragraph | `Function`, contained by its section's Struct, or by the program's Struct when it is outside any section. FQN `program.section.paragraph` (or `program.paragraph`) |
| 01-level record (under FILE SECTION, WORKING-STORAGE, LOCAL-STORAGE or LINKAGE) | `Struct` (`program.RECORD-NAME`), contained by its program's Struct |
| 77-level item | **No node.** Not a Struct (singleton, like a Python module-level variable) |
| 66 (RENAMES) and 88 (condition names) | No node |
| Items below 01 level (PIC, group and elementary items) | No node |
| `OBJECT REFERENCE` item below 01 | `uses` (owning record Struct → class Struct) |
| 77-level `OBJECT REFERENCE` (COBOL 2002) | `uses` (program Struct → class Struct) |
| `METHOD-ID` (instance or factory) | `Function`, contained by its class Struct. Factory and instance methods are not distinguished in the FQN |
| Overloaded `METHOD-ID` (same name, different parameters) | `Class.name(T1,T2)` FQN, per the existing overload rule |
| `PERFORM p`, `GO TO p`, resolved | `calls` (Function → Function), with edge property `kind` = `perform` or `goto` |
| `PERFORM p THRU q` | `calls` to each paragraph in the range (§9), each with `kind` = `perform` |
| `PERFORM section`, `GO TO section` | `uses` (Function → section Struct). Same rule as a call to a class |
| `PERFORM`/`GO TO` to a name not in the program | `unresolved_call`, category `unknown`. The kind (`perform`/`goto`) and the location go in the per-file diagnostic, not on the edge |
| `SORT`/`MERGE` `INPUT PROCEDURE` / `OUTPUT PROCEDURE` | `calls`, resolved the same way as `PERFORM` (GnuCOBOL parses these as performed procedures) |
| `CALL "PROG"`, resolved | `uses` (Function → program Struct) |
| `CALL "PROG"`, not in scan | `unresolved_call`, category `external` (decided, Java precedent) |
| `CALL identifier` (dynamic) | `unresolved_call`, category `func-value` |
| `EXEC CICS LINK`/`XCTL`, resolved | `uses` (as `CALL`) |
| `EXEC CICS LINK`/`XCTL`, not in scan | `unresolved_call`, category `external` |
| `EXEC CICS LINK`/`XCTL` with a variable `PROGRAM(name)` | `unresolved_call`, category `func-value` |
| `EXEC CICS INVOKE SERVICE` / `WEBSERVICE` | `unresolved_call`, category `external` |
| `EXEC SQL CALL` to a procedure in scan | `uses` (as `CALL`) |
| `EXEC SQL CALL` to a procedure not in scan | `unresolved_call`, category `external` |
| SQL user-defined function inside `EXEC SQL` | No edge (opaque block) |
| `EXEC CICS START`/`RETURN TRANSID` | No program edge (decided, O4) |
| `INVOKE` (instance, factory, `SUPER`) | `calls` to the method Function |
| `OBJECT REFERENCE Class` in a procedure (parameter, `RETURNING`, local use) | `uses` (Function → Struct) |
| Data reference in a statement (`MOVE`, `READ ... INTO`, `INVOKE ... USING`) | `uses` (Function → the enclosing 01-level record Struct). A 77-level or unresolved item produces no edge |
| `INHERITS Base` | `uses` (Struct → Struct) |
| `IMPLEMENTS Interface`, interface in scan | `uses` (Struct → Struct) |
| `IMPLEMENTS` or `INHERITS` target not in scan | `unresolved_use`, category `external` (Java style, decided) |
| `INTERFACE-ID … INHERITS FROM Interface` | `uses` (Struct → Struct); several interfaces allowed; same external rule as above |
| `INTERFACE-ID … USING Param` | `uses` to the parameter's class or interface when in scan; otherwise `unresolved_use`, category `external` |
| `REPOSITORY CLASS "java…"` | `unresolved_use`, category `external`, from the program's Struct |
| `COPY` of a copybook | `File` node for the copybook; its declarations located in it |
| `INTERFACE-ID` | `Struct`, contained by its File (as `CLASS-ID`). Its method prototypes are `Function`s contained by it (decided, O11) |
| `RAISE EXCEPTION` with a standard condition name (for example `EC-USER`) | No fact (decided, O13) |
| Micro Focus `RAISE` with an object | No extra fact; the object's declared type is already `uses` (decided, O13) |
| `RESUME AT procedure-name` | `calls` to the paragraph, with edge kind `resume` (decided, O13) |
| `RESUME AT NEXT STATEMENT` | No fact (decided, O13) |
| `PROPERTY` data item, Micro Focus form (`cobol2002`, `cobol2014`) | Implicit `Function`s `GET-<name>` and `SET-<name>`, contained by the class Struct, with the data declaration's span. `as "Name"` names the accessors; `with no set` suppresses `SET-<name>`; static properties are handled as instance ones (decided, O14). The property declaration itself has no node. Standard COBOL 2023 syntax stays **OPEN (O7)** |

The rules for `uses` on class targets and `unresolved_use` follow the Python frontend, which
emits `uses` for a call whose target is a class (`src/pylib/src/scanner/emit.rs`). External
invocations follow the Java frontend, which emits `unresolved_call` for an unresolved method or
constructor call (`src/javalib/Collector.java`, `resolveCall`). Structure
(File-contained top-level units, Struct-contained members) follows the Rust and C++ frontends,
verified against `src/rustlib/src/scanner.rs` and `src/cpplib/scan.h`. The rule that a 77-level
item is not a node follows the Python frontend, which emits only functions and classes
(`src/pylib/src/scanner/walk.rs`). C++ emits namespaces as Modules (`src/cpplib/scan.h`), but a COBOL section cannot be a Module, because a Module cannot contain a Function. Sections are therefore Structs.

## 11. Code types

Built-in `code_type` for `cobol` (a new arm in `src/classify.rs`), following the existing
directory-segment and filename pattern:

| Code type | Rule |
|---|---|
| `test` | a `test` or `tests` path segment (the same segment rule as Go, Rust, C++, Java and C#), or a filename matching one of the test markers below |
| `generated` | a `gen` or `generated` path segment |
| `external` | a `vendor`, `third_party` or `thirdparty` path segment |
| `src` | everything else, including copybooks (`.cpy`) |

Test filename markers (matched case-insensitively, as `classify.rs` lowercases the filename): `test_*`, `*_test.*`, `*-test.*`,
No `T_`/`_T` rule and no `*T` rule: the framework suffix on `SAMPLET` is not a
marker, because ordinary programs end in `T` too.

`integration-tests` is not a `test` segment; it stays `src` unless a config rule says otherwise.

## 12. Testing

Every test sits in one of the three tiers defined by `global.constraint.test-tier-boundaries`.

| Suite | Tier | What it proves |
|---|---|---|
| Lexer and format detection | unit / int (in-memory) | token stream, fixed and free handling, directive detection |
| Parser per construct | unit / int (in-memory) | each COBOL-85 and OO construct parses to the expected AST |
| Preprocessor and source map | e2e (reads files) | COPY, REPLACE, nested COPY, circular COPY, source map positions, `--copy-path` |
| **NIST CCVS85 parse** | e2e | every program in the in-scope modules (NC, IC, IF, IX, OB, RL, SG, SM, SQ, ST) parses with no error diagnostics under `cobol85`; OB and SG constructs give obsolete warnings, not errors |
| **NIST scan** | e2e | the scanner's facts for a sample of NIST programs match expected facts |
| **OO scan** | e2e | OO samples produce the Struct, Function, `uses` and `calls` facts of §10 |
| **Keyword regression** | e2e | COBOL-85 programs using the OO keywords as data names produce the same facts under `cobol2002` as under `cobol85` (§8) |
| **Dialect gating** | e2e | the same source gives the expected facts and diagnostics under each of `cobol85`, `cobol2002`, `cobol2014` and `ibm`; a reserved IBM word used as a data name is an error under `ibm`; area A violations are diagnostics under `cobol85` (§3, §8) |
| **EXEC scan** | e2e | IBM samples: `LINK`/`XCTL` become program calls; `START TRANSID` does not; `INVOKE SERVICE` and `EXEC SQL CALL` to external targets are `unresolved_call` |
| **Accessor and exception facts** | e2e | a `PROPERTY` item produces `GET-` and `SET-` Functions with the `AS` name, and `with no set` drops `SET-`; `RESUME AT p` produces a `calls` edge with kind `resume` (§10) |
| Error recovery | e2e | a file with one bad paragraph keeps its other declarations; the error names the file and line |
| Frontend plumbing | e2e | `--language cobol` is accepted; `.CBL` (uppercase) is detected; the scan writes the expected graph in a scratch repo |

Corpora:

- **NIST CCVS85** programs. Cleared for redistribution under NIST's own terms (O10). Vendor from
  NIST directly. The `yutaro-sakamoto/tree-sitter-cobol` repo carries a copy under
  `test/cobol85/src`, which is a locating aid only; its MIT licence covers that repo's own code,
  not the NIST files. The in-scope modules are the file prefixes NC, IC, IF, IX, OB, RL, SG, SM, SQ and ST. The copy lists 382 programs under those prefixes; the vendored set must come from NIST and be checked against that list. What each prefix covers is not documented in our sources.
- **OO samples.** Written from the MainframeMaster article examples, plus the ones the spike
  produces. The article is used as reference only.
- **IBM samples.** Small hand-written programs first. Larger IBM samples (for example the AWS
  CardDemo application, Apache 2.0) with its licence and notices kept (O10).
- Tests are self-contained: each e2e scenario builds its own scratch repo under `/tmp`. No test
  reads a developer-local path or an external checkout.

## 13. APG integration

Registration touchpoints (all follow the existing per-language pattern):

| File | Change |
|---|---|
| `build.rs` | add `src/cobollib` to the crate list and a build step like `pylib`'s; `cobol` in `APG_BUILD_FRONTENDS` |
| `src/frontends.rs` | detector entry (extensions, depth, skip set), binary lookup, `APG_FRONTEND_COBOL`, language id mapping; pass `-std=<dialect>` to the frontend |
| `src/incremental/language.rs` | extension → `cobol`, case-insensitive |
| `src/classify.rs` | `cobol` arm in `builtin_code_type` (§11) |
| `src/cache/manifest.rs` | add the effective dialect to `ScanConfigKey`, so a change to `cobol.dialect` or `--std` invalidates the cached COBOL facts |
| `src/impact/signature.rs` | `"cobol" => Granularity::File` in `granularity()` |
| `src/structlib` discovery | stop claiming `.cob`, `.cbl`, `.cobol`, `.cpy` |
| `src/lib.rs` | accept `cobol` in the `--language` usage text; accept `--copy-path` and `--std` on `apg scan`; read and write `cobol.dialect` in `apg/config.json` |
| `opencode-suite` (`apg_scan`) | accept the dialect parameter and pass it to `apg scan --std` |
| Packaging | `Formula/apg-cobol.rb`; `scripts/release.sh` and `release.yml` from 8 to 9 formulae; Linux packaging |
| Docs | README frontend dependency table; CHANGELOG |

The frontend has no scan-time tool dependency (nothing is spawned), so no pre-spawn preflight
is needed, unlike the Go and Java frontends.

**Installed-binary rule (AGENTS.md).** Any change-set that adds this frontend is finalised with
the installed `apg` binary. Plan targets, planned nodes and `implemented-by` targets use the FQNs
that the installed binary renders, never a rendering only a newer binary produces. The
`cobol.<dir>.<NAME>` FQNs in §10 are rendered by the ingestor, so the frontend must not
pre-root them.

**Impact granularity: `File`.** COBOL has no package, crate or project unit; each source file is
its own compilation unit, like C++ translation units and TS modules. Cascades through copybooks
work because a program that uses a copybook's declarations gets a file-level dependency on the
copybook (`src/impact/depindex.rs`).

## 14. Failure reporting

- **Per-file errors (decided).** One line per error on the frontend's stderr, in the form
  `cobol: <file>:<line>:<col>: <message>`. The line goes into the scan log through the existing
  spool. The scan continues. Unresolved `PERFORM`/`GO TO` diagnostics include the kind
  (`perform` or `goto`).
- **Per-language error count (decided, option A).** The frontend writes a final count of files
  with errors to its stderr, which also goes into the scan log. No shared-code change is made.
  The terminal progress summary is not changed: it is terminal-only and shows node counts only.
- **Feature-level diagnostics** (decided). Each construct has a level in our per-dialect table
  (O26). `error` and `unconformable` (GnuCOBOL's term) give a diagnostic and skip the construct. `warning`, `archaic` and `obsolete`
  give a diagnostic and still scan it. `ok`, `ignore` and `skip` give no diagnostic.
- **Length limits** (decided, O26). Word, literal and PIC lengths follow the dialect: `cobol85` 30, 160, 30; `cobol2002` 31, 160, 50; `cobol2014` 31, 8191, 63; `ibm` 30, 160, 50. Exceeding one is a diagnostic.
- **Storage and runtime settings** (binary size, truncation, byte order, alignment, OSVS, `MOVE` direction) do not change facts, so they are not part of the feature table (decided, O26).
- **Files that cannot be read or recovered.** One line naming the file.
- **Error limit.** cobc stops after 128 errors (`-fmax-errors`). The COBOL frontend does not stop: it keeps scanning, as the other frontends do (§1 principle 4).
- **Crash reason (decided).** A frontend process failure (non-zero exit, panic) prints its reason
  as its **last** line before exit. The existing 10-line stderr tail in `spawn_frontend` then
  shows it. No change to `spawn_frontend`.

## 15. Open items

| # | Item | Needed by | Status |
|---|---|---|---|
| O1 | `>>SOURCE` syntax | P1 | resolved by the GnuCOBOL manual (§19.1) |
| O4 | `EXEC CICS START`/`RETURN TRANSID` out of the call graph | P2 | decided: yes (§9, §10) |
| O7 | Standard COBOL 2023 syntax for PROPERTY, GET and SET | P7 (for `cobol2023` features) | open for the standard form only; the Micro Focus form is the source for `cobol2002` and `cobol2014` (§10, §19.4) |
| O10 | Licence checks: NIST CCVS85, AWS CardDemo, the MainframeMaster article | before vendoring | decided: NIST CCVS85 cleared under NIST's own terms; CardDemo is Apache 2.0 and keeps its notices; the article is reference only |
| O11 | `INTERFACE-ID`: a `Struct` with its method prototypes as `Function`s; `IMPLEMENTS`, `INHERITS FROM` and `USING` as `uses` (§10) | P3 | decided (§10); GnuCOBOL does not implement `INTERFACE-ID` (§19.4), so the rule rests on the vendor manuals |
| O12 | Framework test files: emit declarations that parse, or only the File node and diagnostics | P2 | decided: emit the declarations that parse |
| O13 | `RAISE` and `RESUME`: facts | P3 | decided (§10) |
| O14 | `PROPERTY` and `GET`: facts | P3 | decided (§10): implicit `GET-<name>` and `SET-<name>` accessors for the Micro Focus form; static handled as instance |
| O20 | Duplicate record names in one program: keep the first, skip the duplicate, emit a diagnostic | P2 | decided |
| O21 | Performance budget (§16, P5) | P5 | decided: at most 2× `cobc -fsyntax-only` on the same tree and machine; the baseline is measured once cobc is installed |
| O22 | Format detection and indicator set | P1 | decided (§3): follow `cobc`; the column check becomes a warning only; indicator set ` `, `*`, `-`, `D` |
| O23 | Program-name matching: §9's case-insensitive fallback differs from GnuCOBOL's case-sensitive default (§19.2) | P2 | decided: keep the fallback, with the warning |
| O24 | `ALTER`, `GO TO` without a name, and `ENTRY`: diagnostic only, no facts, through the dialect table (§14, §19.5) | P2 | decided (follows O26) |
| O25 | Dialect flag. Decided: the frontend accepts `-std=<dialect>` (for example `cobol85` or `cobol2002`), which selects the reserved-word set; the keyword regression test passes it. Decided: user-facing on `apg scan`, recorded as `cobol.dialect` in `apg/config.json`. A user-facing option is a shared-code change to frontend spawning (§13) ; §1 principle 5, §3 and §8 are updated to match. The default reserves `CLASS`, `OBJECT`, `NEW`, `NULL`, `PROPERTY`, `PROTOTYPE` and `RAISE` (§19.4) | P1 | decided |
| O26 | Per-dialect feature table (ours, from the manual, §19) and its gating: which dialects enable OO recognition (`ibm` included?), whether EXEC blocks are recognised only under `ibm` or in every dialect, and the feature level of each construct | P0 | decided (§6, §8, §14, §19.4); the full per-construct table is written in P0 |
| O27 | Source for the EXEC CICS and EXEC SQL mapping (§9, §10). None of GnuCOBOL, the other scanners or the sources so far covers it | P4 | needs a source (IBM), requested in P0 |

## 16. Phases

| Phase | Deliverable | Exit criterion |
|---|---|---|
| **P0** Prerequisites | the O26 feature table (§20); the cache-key design (§13); the NIST suite vendored from NIST, in-scope modules only (§12); cobc installed and the `cobc -fsyntax-only` baseline measured (O21); the IBM source for EXEC requested (O27) | all five done before P1 starts |
| **P1** COBOL-85 parser (`cobol85`) | format detection and area A checks (§3), lexer, preprocessor with source map, COBOL-85 AST and parser, recovery, `cobol85` reserved words, feature-level diagnostics (§14) | every program in the in-scope NIST modules (NC, IC, IF, IX, OB, RL, SG, SM, SQ, ST) parses with no error diagnostics under `cobol85`; keyword regression under `cobol85` passes, with `CLASS` data names expected to error |
| **P2** Scanner v1 (`cobol85`) | COBOL-85 facts (§10 non-OO rows), resolver (§9), failure reporting (§14), `apg scan --std` and `cobol.dialect` (§2), the cache key (§13), APG integration (§13) | NIST scan passes on the in-scope modules; e2e scratch repo gives the expected graph; `apg scan --language cobol` works; `--std cobol2023` refuses with its message |
| **P3** COBOL 2002 and 2014 OO | `cobol2002` and `cobol2014` gating, OO constructs (§6, §8), OO facts (§10), the Micro Focus `PROPERTY` form, `RAISE` and `RESUME` facts | OO scan suite passes; keyword regression under `cobol2002` matches `cobol85` for non-reserved words; no reference compiler exists for the OO syntax, so the OO samples and vendor manuals are the reference |
| **P4** IBM dialect and EXEC | the `ibm` dialect, IBM reserved words (§8), EXEC CICS and EXEC SQL blocks (§9, §10); needs O27 | EXEC scan suite passes on the IBM samples; reserved-word error under `ibm` is tested |
| **P5** Later dialects | `xopen`, `mf`, `acu`, `rm`, on demand (§6) | each added dialect passes the dialect gating tests (§12) |
| **P6** Hardening and release | fixed-format edge cases, diagnostics polish, performance against the O21 baseline, release steps (8 to 9 formulae, `release.sh`, `release.yml`, README, CHANGELOG), installed-binary finalisation (§13) | all suites pass; within 2× `cobc -fsyntax-only` on the same tree and machine; release steps done |
| **P7** `cobol2023` features | the `cobol2023` placeholder gets its features, once the standard syntax is found (O7) | `cobol2023` dialect tests pass; `--std cobol2023` no longer refuses |

P0 must finish before P1 starts, and P2 lands before P3 so that COBOL-85 scanning is usable early. The phase order is accepted (decided).

## 17. Risks

- **Size.** P1 is the largest phase. The full COBOL-85 statement set and data description are
  the bulk of the work.
- **Reference limits.** Without the standard text or a reference compiler, OO behaviour rests on
  the article and the samples. GnuCOBOL has no class definitions, so the OO class syntax has no
  compiler reference at all. Every OO clause that is not confirmed by a source is a risk.
- **Questionable secondary sources.** Some MainframeMaster quick-reference pages contain generic
  text that does not match COBOL (for example, the GET page). They are used only where they agree
  with GnuCOBOL or the NIST suite.
- **Keyword conflicts.** A mistake in the contextual keyword rules breaks COBOL-85 programs in
  silence. The keyword regression suite is the guard and must run on every change.
- **Fixed-format fidelity.** Real files vary in columns 1–6 and 73–80. The NIST suite covers
  many cases but not all.
- **Maintenance.** The parser is ours, so every dialect fix is ours too.
- **Name collisions.** The ingestor panics on any same-kind FQN collision (AGENTS.md). Duplicate names
  are skipped with a diagnostic (O20, decided), so they cannot stop the scan.

## 18. Decision log

### 18.1 Decided

| Decision | Decided by |
|---|---|
| Hand-written Rust parser, not rust-cobol or a tree-sitter grammar; rust-cobol is not a dependency or a behaviour reference | user |
| COBOL-85 and COBOL 2002 OO in one scanner, gated by dialect (§1); OO keywords contextual under `cobol85`, `cobol2002` and `cobol2014`; regression test required | user |
| Extensions `.cob`, `.cbl`, `.cobol`, `.cpy`, matched case-insensitively (`.CBL` included) | user |
| Format: superseded by the O22 row below | user |
| Format default follows the dialect: `cobol85` and `ibm` use the `cobol85` format with area A checks; `cobol2002` and `cobol2014` use fixed format (§3) | user |
| Hand-written, not a tree-sitter grammar; per-file tolerance with recovery at sentence and header boundaries | user |
| EXEC SQL kept opaque; host variables are data references | user (implied by the EXEC decisions) |
| EXEC CICS `LINK` and `XCTL` are program calls | user |
| PROPERTY and GET in scope (syntax open, O7) | user |
| Program or class → `Struct`, contained by its File | user |
| Section → `Struct`, contained by its program; paragraph → `Function`, contained by its section | user |
| 01-level record → `Struct` | user |
| 77-level item → no node; references to it produce no edge | user |
| 77-level `OBJECT REFERENCE` → `uses` from the program Struct (COBOL 2002 only) | user |
| Items below 01 → no node; `OBJECT REFERENCE` items → `uses` from the owning record | user |
| Data references in statements → `uses` (Function → enclosing record Struct) | user |
| `PERFORM`/`GO TO` (resolved) → `calls`, with edge property `kind` | user |
| `PERFORM p THRU q` → `calls` to each paragraph in the range, including other sections | user |
| `PERFORM section` → `uses` | user (with the section decision) |
| `CALL`, resolved → `uses`; not in scan → `unresolved_call` (`external`), following the Java frontend (replaces the earlier `unresolved_use` rule) | user |
| `CALL` dynamic → `unresolved_call`, category `func-value` | user (implied by the CALL decision) |
| `INVOKE` → `calls`; `INHERITS` and `IMPLEMENTS` → `uses` (Struct → Struct) | user |
| `SORT`/`MERGE` procedures → `calls` | confirmed against GnuCOBOL; no objection |
| Declaratives → ordinary Functions (and Structs for sections); the `USE` statement is not a node | confirmed against GnuCOBOL; no objection |
| Unresolved `PERFORM`/`GO TO` → `unresolved_call`, category `unknown`; kind in the diagnostic, not on the edge | user |
| Program-name matching: exact first, then case-insensitive with a warning | user |
| Copybook search: including file's directory, scan root, `copybooks/`, `copy/`; extra directories via repeatable `--copy-path` | user |
| Per-language error count: frontend stderr to the scan log, no shared-code change | user |
| Crash reason: frontend prints its reason as its last line; no change to `spawn_frontend` | user |
| Impact granularity `File` | user |
| Copybook declarations located in the copybook file | user |
| Test by segment and by filename markers `test_*`, `*_test.*`, `*-test.*`, `*TEST.*`, `*-TEST.*`; no `T_`/`_T`/`*T` rule | user |
| Frontend emits no `code_type`; the ingestor computes it | AGENTS.md |
| Installed-binary rule: FQNs rendered by the installed binary, frontend emits identities verbatim | AGENTS.md |
| Framework test files: File node always emitted; declarations that parse are emitted, with diagnostics for the rest (O12) | user |
| Duplicate record name in one program: keep the first, skip the duplicate, emit a diagnostic (O20) | user |
| Program-name fallback (case-insensitive, with a warning) kept, although GnuCOBOL is case-sensitive by default (O23) | user |
| Licence: NIST CCVS85 cleared for redistribution under NIST's own terms, vendored from NIST directly; CardDemo is Apache 2.0 and keeps its notices; the MainframeMaster article is reference only (O10) | user |
| Format: directive, else fixed; a heuristic warns on likely free-format files; indicator set ` `, `*`, `-`, `D` (O22) | user (accepted recommendation) |
| `--std <dialect>` on `apg scan`, recorded as `cobol.dialect` in `apg/config.json`; default `cobol85`; overridable per run (O25) | user |
| The dialect gates OO recognition, reserved words, default format and feature-level diagnostics; v1 values `cobol85`, `cobol2002`, `cobol2014`, `ibm` | user |
| Feature levels: `error` skips with a diagnostic; `warning`, `archaic`, `obsolete` diagnose and scan; `ok`, `ignore`, `skip` are silent (§14) | user |
| The feature table is ours, written from the GnuCOBOL manual, not copied from GPL config files | user |
| `ibm` in v1; EXEC CICS and EXEC SQL blocks move to P2 (superseded by the revised outline: P4, after O27) | user |
| Under `ibm`, using `OBJECT`, `CLASS`, `METHOD`, `INVOKE`, `ENTRY`, `ALTER` (and other IBM reserved words) as a data name is an error | user |
| Area A checks run as diagnostics under the `cobol85` format (§3) | user |
| `EXEC CICS START`/`RETURN TRANSID` out of the call graph (O4) | user |
| External invocations (`CALL`, `LINK`, `XCTL`, `INVOKE SERVICE`/`WEBSERVICE`, `EXEC SQL CALL`) to targets outside the scan are `unresolved_call`, category `external`, following the Java frontend (§9, §10) | user |
| Dynamic `LINK`/`XCTL` with `PROGRAM(name)` is `unresolved_call`, category `func-value`; SQL user-defined functions emit no edge | user |
| Reserved words follow GnuCOBOL's per-dialect word lists: `CLASS` is reserved under `cobol85`; most OO words are reserved under `cobol2002` and `cobol2014` (§8, §19.4, O26) | user |
| `unconformable` is treated as `error`: a diagnostic, and the construct is skipped (O26) | user |
| Word, literal and PIC length limits follow the dialect and are diagnostics (§14, O26) | user |
| OO recognition is on under `cobol2002`, `cobol2014` and `ibm`, and off under `cobol85` (O26) | user |
| EXEC blocks are recognised only under `ibm`; elsewhere an EXEC block is an error and is skipped (O26) | user |
| Storage and runtime settings are excluded from the feature table, since they don't change facts (O26) | user |
| `ALTER`, `GO TO` without a name and `ENTRY` give diagnostics and no facts, through the dialect table (O24) | user (follows O26) |
| Performance budget: at most 2× `cobc -fsyntax-only` on the same tree and machine (O21) | user |
| `INTERFACE-ID` is a Struct with its method prototypes as Functions; `INHERITS FROM` and `IMPLEMENTS` are `uses`; `USING` parameters are type references; no edge from a class method to the interface method it implements (O11) | user |
| `IMPLEMENTS`, `INHERITS` and `REPOSITORY CLASS` targets outside the scan are `unresolved_use`, category `external` (Java style) | user |
| `GET <name>` accessor is a Function named `GET-<name>` under its class Struct, with no node for the property declaration (O14) | user |
| `PROPERTY` (Micro Focus form) accepted in `cobol2002` and `cobol2014`, labelled a vendor form (O7, O14) | user |
| Accessors: `GET-<name>` and `SET-<name>` Functions under the class Struct, with the data declaration's span; `as "Name"` names them; `with no set` suppresses `SET-`; no static flag, so static properties are handled as instance ones (O14) | user |
| `cobol2023` added as a dialect value, an empty placeholder with features to be added later | user |
| RAISE and RESUME facts: a standard `RAISE EXCEPTION` gives no fact; a Micro Focus `RAISE` with an object gives no extra fact; `RESUME AT procedure-name` gives a `calls` edge with kind `resume`; `RESUME AT NEXT STATEMENT` gives no fact (O13) | user |
| Review fixes recorded: the scan cache key includes the dialect (§13); the dialect is the first user-set language setting (§3); `kind` is the first edge property (§10); EXEC rules are unverified until a source is found (O27); Module→Function containment is not allowed (§10); test markers and the copybook defaults are reworded (§5, §11) | user |
| Revised phase outline P0 to P7 accepted (§16) | user |
| NIST exit criterion: the in-scope modules are the file prefixes NC, IC, IF, IX, OB, RL, SG, SM, SQ and ST (§12, §16) | user |
| `cobol2023` refuses with a message until its features exist; its features are planned after the scanner is released (§6, §16) | user |

### 18.2 Still open

O7 (standard COBOL 2023 form only) and O27, listed in §15. O1, O4, O10, O11, O12, O13, O14, O20, O21, O22, O23, O24, O25 and O26 are decided. O1 is resolved by the
GnuCOBOL manual (§19.1).

## 19. Reference: GnuCOBOL manual

Source: *GnuCOBOL Manual*, for GnuCOBOL 3.2-dev, 9 October 2022
(https://gnucobol.sourceforge.io/doc/gnucobol.pdf). It documents the `cobc` compiler, not the
COBOL standard, so it is a secondary reference (§6). No text from it is copied into the repo.

### 19.1 Source format

- The default is fixed format. It is overridden by the directive
  `>>SOURCE [FORMAT] [IS] {FIXED|FREE|COBOL85|VARIABLE|XOPEN|XCARD|CRT|TERMINAL|COBOLX}`, or by
  options (`-free`/`-F`, `-fformat=...`, `-fixed`).
- Fixed format: columns 1–6 sequence area, column 7 indicator, columns 8–72 program text,
  columns 73–80 reference area (ignored).
- Indicator column: `*` comment, `-` continuation, `D` debugging line (`-fdebugging-line` or
  `>>D`). `/` is a comment only with `-fmfcomment`; `$` is treated as `*` only with
  `-facucomment`.
- `cobc` does not detect the format from the columns. A file with no directive or option is read
  as fixed format.
- Free format: program text runs from column 1 to the end of the line; `*>` starts an inline
  comment.

### 19.2 Program names, copybooks and case

- `-I directory` adds a directory to the copy/include search path (§5, `--copy-path`).
- `-ext extension` adds a default extension for resolving `COPY`.
- `-ffold-copy` and `-ffold-call` (`UPPER`/`LOWER`) change case only when given; the default is
  no transformation.
- At run time, with `COB_LOAD_CASE` unset, program names in `CALL` are case-sensitive.
- `-fstatic-call` applies only to literal names. A data name in `CALL` is always dynamic.
- `COB_COPY_DIR` does not appear in the manual.

### 19.3 File extensions

- `cobc` treats `*.cob` and `*.cbl` as COBOL source. `.cobol` and `.cpy` are not mentioned.

### 19.4 OO keywords and reserved words

From the reserved-word list (Appendix B, default dialect), "Implemented" column:

- Marked implemented: `CLASS`, `OBJECT`, `NEW`, `NULL`, `PROPERTY`, `PROTOTYPE`, `RAISE`.
- Marked not implemented: `CLASS-ID`, `INTERFACE`, `INTERFACE-ID`, `METHOD`, `METHOD-ID`,
  `FACTORY`, `INVOKE`, `INHERITS`, `IMPLEMENTS`, `OBJECT-REFERENCE`, `SELF`, `SUPER`, `GET`,
  `RESUME`.
- The list depends on `-std` and on the reserved-word options. The `cobol85` list is not
  recorded here in full. The OO-relevant entries are: `cobol85` reserves `CLASS` and `ALTER`; `cobol2002` and `cobol2014` reserve most OO words, with `IMPLEMENTS` context-sensitive; `ibm` reserves `CLASS`, `CLASS-ID`, `OBJECT`, `METHOD`, `INVOKE`, `INHERITS`, `SELF`, `SUPER`, `NULL`, `ENTRY` and `ALTER` (O26). Micro Focus reserves `PROPERTY`, `GET` and `SET`; ACUCOBOL-GT reserves `PROPERTY` only; RM-COBOL and IBM reserve neither.
- The manual gives no syntax for `CLASS-ID`, `METHOD-ID`, `INVOKE`, `OBJECT REFERENCE`,
  `PROPERTY`, `GET` or `RESUME`. It therefore does not confirm the OO syntax in §6 or the facts
  in §10.
- Appendix F lists `EC-OO-*` exception names. These are runtime checks, not syntax.

Consequence: O7, O11, O13 and O14 remain open. The manual adds no confirmation for them.

### 19.5 Other statements and options

- `ALTER`: `alter-statement: obsolete` in the default configuration (Appendix G).
- `GO TO` without a name: `goto-statement-without-name: obsolete`.
- `ENTRY`: `entry-statement: ok`.
- Section segments: `section-segments: ignore`.
- None of these appears in §6 or §10 yet (O24).

### 19.6 Not covered by the manual

- The standard COBOL 2023 syntax for `PROPERTY`, `GET` and `SET` (O7). The Micro Focus form is covered by §10 and §19.4.
- `EXEC CICS` and `EXEC SQL` (O4).
- The full reserved-word lists beyond the OO-relevant entries recorded in §19.4.
