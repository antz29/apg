# APG Agent Guide

## Pipeline

Scanner (per language) → Rust ingestor → `apg/.trans/db.lbug` + `apg/.trans/graph.jsonl`.

- The **scanner** (Go: `src/golib/main.go`, Java: `src/javalib/CallGraphBuilder.java`,
  C++: `src/cpplib/main.cpp`, Rust: `src/rustlib/src/main.rs` — a standalone
  `rustfrontend` binary built on rust-analyzer's `ra_ap_*`-era engine crates,
  pulled from the rust-analyzer repo at a pinned release tag, TypeScript:
  `src/tslib/scanner.mjs` — a Node script using the official `typescript`
  compiler API, npm-installed with a committed `package-lock.json`, C#:
  `src/csharplib/Program.cs` — a standalone single-file `csharpfrontend` binary
  built on Roslyn `Microsoft.CodeAnalysis.CSharp`, Python: `src/pylib` — a
  standalone `pyfrontend` built on Astral's `ty`/Ruff engine crates, Markdown:
  `src/mdlib` — a standalone `mdfrontend`) parses a codebase and streams
  one JSON object per
  line to stdout — the **unified JSONL schema** (see `SPEC.md` §2). It emits
  *facts only*: declarations, references, edges. It never computes FQNs and
  never does graph assembly.
- The **ingestor** (`src/ingest.rs`) spawns the scanner, resolves identity
  (canonical FQN), builds the graph, bulk-loads LadybugDB, and writes
  `graph.jsonl` as the export. `apg/.trans/db.lbug` is the query index;
  `apg/.trans/graph.jsonl`
  is the self-contained export artifact (canonical FQNs, no opaque ids).
- Build: `build.rs` compiles the frontends (`gcc`/`g++` tree-sitter for C++,
  `go build` for Go, `javac` for Java, `cargo build` for the Rust frontend —
  `src/rustlib`, a separate Cargo project, pinned to a rust-analyzer release
  tag, `npm ci` for the TypeScript frontend — `src/tslib`, `cargo build` for
  the Python (`src/pylib`) and Markdown (`src/mdlib`) crates) and stages them to
  `target/<profile>/frontends`. Run a scan with `apg scan <dir>` (or the
  `apg_scan` tool). `apg` resolves frontends at runtime relative to the binary
  (`<exe_dir>/frontends` or `<exe_dir>/../libexec/frontends`) or via
  `APG_FRONTEND_DIR`. `APG_BUILD_FRONTENDS` (comma-separated: `go`, `java`,
  `cpp`, `rust`, `ts`, `csharp`, `py`, `md`; `0` to skip) limits what build.rs
  compiles.
- **Frontend dependency baseline.** Each frontend's build-time deps, scan-time
  deps (what must be on `PATH` when a user runs `apg scan`), and the engine pin
  that sets its language-version ceiling are declared on its solution container
  in the durable spec, and restated per frontend in README.md's *Frontend
  dependency contract* table. In brief: Go builds with Go ≥ 1.25 + network
  (`golang.org/x/tools v0.48.0`), needs `go` on `PATH`, and is bounded by
  `x/tools v0.48.0` jointly with the compiling toolchain's `go/types`; Java
  builds with `javac --release 21`, needs `java` (JDK ≥ 21), and is bounded by
  the runtime JDK's `javac`; C++ builds with `gcc`/`g++` and vendored
  tree-sitter, needs nothing at scan time, and is bounded by the vendored
  tree-sitter language ABI v15 + tree-sitter-cpp grammar content; Rust builds
  with stable Rust + network, needs `cargo`/`rustc`, and is bounded by
  rust-analyzer tag `2026-08-17` (0.0.348); TypeScript builds with `node` +
  `npm ci`, needs `node`, and is bounded by `typescript 5.9.3`; C# builds with a
  .NET SDK (`net9.0`) + NuGet, is self-contained at scan time, and is bounded by
  Roslyn `4.12.0`; Python builds with stable Rust + network (git-pinned Ruff/ty
  crates) and Markdown with stable Rust + crates.io, neither needing a runtime
  at scan time, bounded by Ruff/ty tag `0.16.6` (`salsa 0.27`) and
  `serde`/`serde_json`/`unicode-normalization` respectively. The build
  toolchains are pinned in repo-visible files consumed by every
  frontend-compiling path, exact where the mechanism can enforce it: Rust
  `1.98.1` (the repo-root `rust-toolchain.toml`, covering the main build and all
  three cargo frontend crates) and Node `26.9.0` (`src/tslib/package.json`
  `engines.node`, enforced fail-closed by `engine-strict=true` in
  `src/tslib/.npmrc`). Go is exact on the release Linux CI path (`GOTOOLCHAIN` +
  `actions/setup-go`, `1.27.1`) and a documented floor elsewhere —
  `src/golib/go.mod`'s `toolchain go1.27.1` directive bounds the bottle/ambient
  path, whose formula `depends_on "go"` is unversioned — matching the pins in
  `.github/workflows/release.yml`.

## Project flow (the verified pattern)

A change-set is a **project** = a git branch + its worktree. The verified
pattern — visible to the next feature's agents without reading the suite
internals:

1. **Start from main.** The navigator runs `apg project start <name>` from the
   **main checkout** (never inside a worktree). The binary branches off the
   repo's default branch, creates the worktree at `<main>/apg/.worktrees/<name>`,
   and seeds the branch DB by copying the main checkout's `apg/.trans` scan
   verbatim (worktree + branch + branch DB in one command, no frontend scan).
   Start refuses when the main checkout's scan is stale or missing for main's
   HEAD, so run `apg scan` in the main checkout first. Then it **prints
   the worktree path**. Idempotent only inside that same project; every
   collision is a hard refuse.
2. **Operate in-worktree.** Sessions and subagents run with **cwd inside the
   printed worktree path**. Suite-tool walk-up discovery finds the worktree's
   **own** `apg/` (its layout + branch DB), so the tools work unchanged.
   **Main is never a mutation place** — durable and transient mutations refuse
   outside a project context.
3. **Author the durable spec tiers** via `apg node add|rm <layer> <type>
   <name>` / `apg edge add|rm <kind> <from> <to>` into `apg/layers/` — one
   JSON file per node in the **six-layer tree** (requirements / domain /
   solution / implementation / global durably; plans transient). FQN =
   `<layer>.<type>.<name>`, the **file name IS the identity**, no project
   prefix. Node files carry paired `out`/`in` edges (both endpoint files,
   validated at scan); constraints are **prose** — structure/references are
   checked at write time, satisfaction is by review. One logical mutation
   writes all affected files and **auto-commits once** on the project branch.
4. **Plan and review are transient**: `apg/.trans/plans/<project>.jsonl` plus
   the `.trans/<tier>/<project>.jsonl` feedback mirrors — never committed,
   they die with the branch (the node files persist). A plan task names its
   verb + target: `creates` (a planned node), `modifies`/`deletes` (code that
   must already resolve), `renames`/`moves` (`--fqn` source + `--to`
   destination).
5. **Verify, then merge.** `apg plan verify <project>` is the pre-merge
   coherence gate — every planned node realized, all feedback resolved,
   derived solution coverage holds — and prints the merge handoff.
   `apg project merge <name>` from the main checkout runs verify → merge →
   unguarded main rebuild, then **self-cleans** on that success path:
   the merged project's worktree is removed and its branch deleted (the
   default branch and the main checkout are never touched); push/tag remain
   human acts. Both the gate and the merge run in the **installed** binary, so
   the plan/spec must use that binary's FQNs — see *The installed binary is the
   contract* below.
6. **Version gate + `apg init`.** `apg/config.json` carries the
   binary-managed `version` field; `apg scan`/`apg project start` **block**
   unless the layout and the binary share major.minor. **`apg init` is the
   upgrade act**: re-run it idempotently to write the current version, scaffold
   `apg/.worktrees/` + its `.gitignore` entry, and update the installed suite.

### The installed binary is the contract — never self-host a change-set

`apg plan verify` and `apg project merge` are executed by the **installed** `apg`
binary. That binary is fixed, and its checks are **exact-match** against the graph
*it* builds. The change-set must therefore be expressible in the running binary's
vocabulary — **the parent can never depend on its future child**:

- **Never point a branch-built `apg` at the apg repo** — not to `scan`, not to
  `verify`, not to `merge`. The apg repo's own graph is (re)built by the installed
  release only, and only the installed release runs `project merge` on it.
  "Build the new binary and use it to finalise" is the oroboros — do not do it.
- **The plan and the durable spec use the running binary's FQNs.** Task targets,
  planned-node FQNs and `implemented-by` targets are the *language-agnostic code
  identities the installed binary renders* (`apg.cmd_scan`,
  `apg-tsfrontend.scanner.collectFile`), never a rendering only the next binary
  produces (`rust.apg.cmd_scan`). A change-set that alters how the *next* binary
  renders FQNs (e.g. language rooting) must not make the *current* plan or spec
  depend on that rendering; the next binary's tolerance
  (`layers::classify_code_ref`) resolves the same identities once it is released.
- **A durable artifact only the next binary can read is broken, not advanced** —
  fix it to the running vocabulary.
- **Realize planned nodes in the running binary's graph.** The verify gate
  realizes a planned node by the FQN the installed binary renders; once the code
  exists, the planned node is obsolete and must be removed — a child-rendered FQN
  (`rust.X`) will never realize against the bare graph (`X`) the installed binary
  builds.
- **Verify a change-set by tests, not by scanning the repo** — the unit/int
  default suite plus the opt-in e2e tier against scratch `/tmp` repos; the apg
  repo's own graph is a navigation aid.

### Discovered work: stop, re-plan, then implement

A plan cannot enumerate every unit a change needs — discovery during
implementation is expected. What is not optional is the order: **discovered work
is planned before it is implemented.**

- An implementer that finds its task's verb/target does not cover the change it
  must make — a unit no task owns, a different mechanism than the task names, a
  spec/constraint the code contradicts — **stops before editing** and returns the
  diagnosis (what it found, the units/behaviour needed, "nothing written yet",
  and a proposed task shape) to the coordinator.
- The coordinator routes the discovery back to authoring: the **plan-writer** adds
  the missing coverage — a **planned Implementation node plus a `creates` task per
  new unit, declared before the code exists** (a planned FQN is refused once the
  code resolves in a scan); a spec gap goes to the **spec-writer** in
  reconciliation mode, through spec-review.
- Only then is the implementer re-dispatched against the amended plan. "Everything
  ends up planned at some point" is the invariant; the verify gate (every unit
  covered by a task, planned nodes realized, feedback resolved) is where it is
  checked.
- Corollary: `creates` is only authorable while the FQN is absent from the scanned
  graph. Landing the code first forfeits it — the back-fill is then a `modifies`
  task plus a note saying so, which is strictly worse.

The suite agents (`codebase-navigator`, `spec-writer`, `plan-writer`,
`spec-review`, `plan-review`, `agent-builder`) hold the operational detail.

### CLI

The project builds a single `apg` binary (package `apg`):

- `apg init [dir]` — create `apg/` (committed `config.json` carrying the
  binary-managed `version` field + gitignored `.trans/`), scaffold the repo
  `.gitignore` for `apg/.trans/` and `apg/.worktrees/` (added if
  missing, other lines untouched), and install (or update, where contents
  differ) the opencode apg tool suite into `~/.opencode/tools/` +
  `~/.opencode/lib/` plus the **six distributed agents** — `codebase-navigator`,
  `spec-writer`, `plan-writer`, `spec-review`, `plan-review`, `agent-builder` —
  into `~/.opencode/agents/`. Project-specific implementer/reviewer agents are
  installed into the project's `.opencode/agents/` by the **`agent-builder`**
  agent, never by init. If the project's `.opencode/` holds files that duplicate
  the installed suite (`~/.opencode/`), init prints a **loud warning** listing
  them — it never deletes anything.
- `apg scan [dir] [--language L[,L...]] [--exclude-path G]* [--module M]* [--no-build-scripts]
  [blacklist...]`
  — run the pipeline; writes `apg/.trans/db.lbug`, `apg/.trans/graph.jsonl`,
  `apg/.trans/apg-frontend.log`. `--language` accepts one or more comma-separated
  languages; when omitted, `apg` auto-detects **every** language present and
  scans them all, merging their graphs into one database (a multi-language repo
  like a Go backend + TS frontend gets a single `apg/.trans/db.lbug`). Each frontend's
  opaque ids are namespaced per language (`--id-prefix`), and the ingestor
  classifies `code_type` and renders FQNs per record (a `lang_switch` control
  record precedes each frontend's stream). `--no-build-scripts` is Rust-only
  (skip cargo build scripts and the proc-macro server); the Rust frontend
  requires a Cargo manifest (C++ tolerates bare dirs; Rust scans nothing
  without one; TS needs a `package.json`/`.ts`/`.tsx` sources, and `node_modules`
  is always skipped).
- `apg query [--json] "<cypher>"` — read-only Cypher over `apg/.trans/db.lbug`
  (found by walking up from cwd); CSV with header row by default, `--json` for
  JSON rows.
- `apg node <sub> …` / `apg edge <sub> …` — the durable mutation surface:
  `node add|update|rm <layer> <type> <name>` writes/updates/removes one node
  file under `apg/layers/` (the file name is the identity; `add` refuses an
  existing FQN, `update` merges body/properties and is edge-preserving — the
  name is immutable); `edge add|update|rm <kind> <from> <to>` writes **both**
  endpoint files in one atomic, auto-committed mutation (the out half in the
  source's file, the matching in half in the target's; `add` refuses a
  duplicate, `update` is properties-only). Guarded — refuses outside a project
  worktree.
- `apg plan <sub> …` — the phased execution plan (transient, serialized to
  `apg/.trans/plans/<project>.jsonl`, branch-local): `add`
  (`add <project>` creates the plan, refusing when it exists; then
  `phase`/`task`/`planned` refuse an existing entity), `update` (`<project>
  [--title] [--strategy]`, or `phase <n>`/`task <phase> <k>`/`planned <fqn>`;
  edge-preserving, and sets/replaces a phase's `Satisfies`/`Gates` via
  `--satisfies`/`--prereq`), `rm`
  (`<project>|phase|task|planned [--force]`; refuses while dependents exist),
  `done`/`undone` (assertion-only), `note` (task notes), `complete`
  (milestone-only), `render`, `verify` (the pre-merge coherence gate; the
  binary applies nothing).
- `apg project <sub> …` — project contexts (worktrees, git2-operated):
  `start <name>` (from the main checkout: worktree + branch + branch DB off
  the default branch, prints `apg/.worktrees/<name>`), `merge <name>` (from
  the main checkout: verify gate → merge → unguarded main rebuild).
- `apg review <sub> …` — the closed, coordinator-mediated writer↔reviewer
  feedback cycle over the transient `.trans` mirrors: `add`, `action`,
  `resolve`, `reject`, `list`. The owning writer returns an ACTIONED/WONT-FIX
  claim; the coordinator runs `action` after the shallow consistency check;
  only the reviewer resolves/rejects.
- `apg --version`, `apg --help`.

`apg init` also installs the **apg opencode tool suite** into the user-level
`~/.opencode/` (single-sourced from this repo's `opencode-suite/`, embedded in
`src/main.rs` via `include_str!`): `apg_scan`, `apg_query`, plus curated
abstractions over common lookups — `apg_find_symbol`, `apg_modules`,
`apg_module_files`, `apg_module_structs`, `apg_file_units`, `apg_file_path`,
`apg_methods`, `apg_struct`, `apg_callers`, `apg_callees`, `apg_uses`,
`apg_unresolved`, `apg_hunk` — and the project/spec/plan/review suite:
`apg_project` (start/verify/merge), `apg_node` / `apg_edge` (durable
node-file add/update/rm mutations), `apg_plan_add` (the plan add/update/rm
authoring surface), `apg_plan` (+ phases/tasks/verify/done/undone/note/complete/
render), `apg_review` (+ add/action/resolve/reject).
Shared plumbing
lives in `~/.opencode/lib/apg.ts`
(root discovery, `apg query`/`apg project`/`apg node`/`apg edge`/`apg plan`/`apg review` subprocess,
Cypher literal escaping). All suite
tools take an optional `codeType` (default: all code); exact-FQN tools hint
when a lookup comes up empty (overloads carry `(params)` suffixes).

The `apg` binary is brew-installable via split formulae (tap
`https://github.com/antz29/apg.git`): `scanner` (the binary), plus eight frontend
formulae — `apg-go`, `apg-java`, `apg-cpp`, `apg-rust`, `apg-ts`, `apg-csharp`,
`apg-py`, `apg-md` —
each dropping its artifacts into `$(brew --prefix)/share/apg/frontends` (the
`scanner` formula's `bin/apg` wrapper sets `APG_FRONTEND_DIR` to that dir).
On Linux there's a `curl | sh` installer (`install.sh` — installs the base
`apg` scanner binary and/or separate language frontends to `/usr/local` or `--user`'s
`~/.local`, layout `bin/apg` + `libexec/apg/`); the linux-release workflow
builds individual packages (`apg-linux-*.tar.gz`, `apg-<lang>-linux-*.tar.gz`)
plus `sha256sums.txt` per tag.
In the repo itself, run it via `cargo run -- scan <dir>` or
`target/debug/apg scan <dir>`.

## Test tiers & the tier-separable harness

Every test in the repo belongs to exactly one of three tiers, defined by the law
`global.constraint.test-tier-boundaries`:

- **unit** — exactly **one** unit under test, everything else faked; **pure
  in-memory** (no filesystem, database, git or process).
- **int** — **two or more** units wired together; still **pure in-memory**
  (filesystem/database/git/process faked).
- **e2e** — one or more units with **real I/O of any kind**. By definition:
  any filesystem access (**including `std::env::temp_dir()`/`TempDir`**), opening
  `db.lbug`, any git operation, or any process spawn is **e2e**, whatever the
  test is named or wherever it lives. The body wins over any name or evidence
  list — if it does real I/O, it is e2e.

**Tier-marker convention.** Inside each `#[cfg(test)] mod tests`, every test
lives in exactly one tier submodule named `unit`, `int` or `e2e`, so libtest's
path filter selects a tier (`…::tests::unit::`, `…::tests::int::`,
`…::tests::e2e::`). Every **e2e** test also carries
`#[ignore = "e2e tier: …; run via cargo test-e2e"]`, so a plain `cargo test` can
never reach it. Each tier submodule starts with `use super::*;`.

**Helpers stay at the `mod tests` root.** Only `#[test]` functions move into a
tier. Every non-test helper/fixture/builder/test-util stays at the `mod tests`
root (or in one shared `common` child module of `mod tests`), is never moved into
a tier and never duplicated per tier; a helper that sibling tiers must reach
stays at the root with `pub(super)`/`pub(crate)` visibility.

**A tier may select ZERO tests.** The repo has few genuine pure unit tests, and
no genuine 2+-unit pure tests at all, so an empty selection is a valid, named
invocation; a module with no test of a given tier simply omits that submodule.

**The four documented invocations** (aliases live in `.cargo/config.toml`):

```sh
cargo test          # DEFAULT GATE: unit+int only, seconds, e2e unreachable
cargo test-unit     # unit tier only     (= cargo test tests::unit::)
cargo test-int      # int tier only      (= cargo test tests::int::)
cargo test-e2e      # e2e tier only, opt-in (= cargo test tests::e2e:: -- --ignored)
```

`cargo test` runs **unit+int only** and stays seconds-fast: the e2e tests are
`#[ignore]`d and live under `tests::e2e::`, so they are excluded from and
unreachable through the default gate. **e2e is the FINAL gate only** and obeys
`global.constraint.no-real-project-test`: the candidate binary is exercised
against a scratch `/tmp` git repo, never a real project (see below).

**The repo gate is `scripts/gate.sh`** — the single gate command. It runs the
exact sequence `cargo fmt --check` → `cargo check --all-targets` → `cargo clippy
--all-targets -- -D warnings` → `cargo build` → `cargo test` (the default
unit+int suite), stopping at the first failure; `scripts/gate.sh --e2e` appends
the opt-in e2e tier. It is **run-only** — `scripts/**` is not in an agent's edit
grant, so the sequence cannot be rewritten beneath a running agent.

Listing quirks (verified): `cargo test -- --list` **includes `#[ignore]`d
tests**, and `cargo test-e2e -- --list` mis-composes to a double `--` and
*executes* the e2e tier instead of listing it — to list the e2e tier use the raw
form `cargo test tests::e2e:: -- --ignored --list`.

**Release gate note.** The release-version guard tests
(`cargo_manifest_and_lockfile_declare_release_version`,
`readme_documents_release_version`) read `Cargo.toml`/`Cargo.lock`/`README.md`
from disk, so they are **e2e** and are **no longer run by a plain `cargo test`**.
The default gate stays fast and the guard is exercised by the opt-in e2e tier.
The release gate is therefore `cargo build` + `cargo test` (fast unit+int) +
`cargo test-e2e` (which runs the guard); see the release section below.

**Self-contained e2e.** The whole test suite is self-contained
(`global.constraint.self-contained-tests`): every e2e scenario builds its own
scratch `/tmp` git repo (or synthetic `target/`/DB tree) and drives the candidate
binary against it. No test reads `$HOME`, an external checkout, or any other
developer-local path, so `cargo test-e2e` needs no `cargo build --release` —
`cargo build` (debug) is enough.

## Testing a new binary (scratch repo in /tmp)

**Never point a freshly built `apg` at the apg repo** (or any project you care
about). A candidate binary is exercised against a throwaway git repo under
`/tmp`, so `init` / `scan` / `project` / `merge` mutations never touch real
state; the apg repo's own graph is only ever rebuilt by the installed (released)
binary. This covers generated agents too: the apg repo's own
`.opencode/agents/**` are (re)generated with the installed release, not by an
in-tree project.

This is the **parent-cannot-depend-on-the-child** rule: the change-set is
finalised with the installed binary, so nothing it must read — plan targets,
planned nodes, `implemented-by` targets — may be written in a future binary's
FQN rendering. See *The installed binary is the contract* in the project flow.

1. **Build the candidate.** `cargo build` produces `target/debug/apg` and
   stages the frontends to `target/debug/frontends`, so the binary finds them
   automatically (no `APG_FRONTEND_DIR` needed). Always invoke it by full path
   (`"$BIN"`) — never a bare `apg`, which resolves to the brew-installed
   release:

   ```sh
   BIN="$PWD/target/debug/apg"
   ```

2. **Spin up a scratch repo with at least one real source file.** The source
   matters: `apg project merge` rebuilds the main graph and **self-cleans only
   on success**, so a source-less repo makes the rebuild fail and skips
   worktree/branch removal.

   ```sh
   BASE=/tmp/apg-spin              # throwaway; rm -rf when done
   rm -rf "$BASE"; mkdir -p "$BASE"; cd "$BASE"
   git init -q -b main
   git config user.email apg@localhost; git config user.name apg
   printf 'package main\n\nfunc main() {}\n' > main.go
   git add -A && git commit -q -m init
   ```

3. **Init with an isolated HOME** so the suite install `apg init` performs
   lands in a scratch `~/.opencode` instead of your real one, then commit the
   scaffold:

   ```sh
   HOME=/tmp/apg-home "$BIN" init .   # writes apg/config.json at the binary's version
   git add -A && git commit -q -m "apg init"
   ```

   The new `apg/config.json` carries the binary's version; the version gate
   (`apg scan` / `apg project start`) **blocks unless layout and binary share
   major.minor**, so re-run `apg init` after a version bump.

4. **Exercise the lifecycle** exactly as the flow section describes — the
   scratch repo is a real project:

   ```sh
   "$BIN" scan .                             # main must have a current scan: start copies it
   "$BIN" project start demo                 # worktree + branch + branch DB copied from main's scan
   cd apg/.worktrees/demo
   "$BIN" node add requirements requirement demo-req --body "…"
   "$BIN" node add domain value demo-val --body "…"
   "$BIN" edge add drives requirements.requirement.demo-req domain.value.demo-val
   "$BIN" node add requirements requirement demo-req --body dup  # must refuse (use update/rm)
   "$BIN" plan add demo --title Demo --strategy "…"
   "$BIN" plan add demo phase 1 --title P1 --deliverable "…" --satisfies demo-req
   "$BIN" plan verify demo                   # pre-merge coherence gate
   cd ../..
   "$BIN" project merge demo                 # verify → merge → main rebuild → self-clean
   ```

5. **Clean up.** `rm -rf /tmp/apg-spin /tmp/apg-home` — nothing in the apg repo
   was touched.

Traps the gate enforces that surprise first-timers: `project merge` refuses with
*"no plan for project …"* until `apg plan add <project>` has run; durable
`node`/`edge` mutations refuse on the default branch (author **inside** the
worktree); and a refused/failed merge leaves the worktree and branch untouched.

## Deploying a release (cutting a tag)

Releases are cut by pushing an annotated `vX.Y.Z` tag; `release.yml` builds the
ARM bottles and the x86_64/aarch64 tarballs, then a single publish job creates
the release as a draft, attaches every asset, and publishes it.

**Published releases are immutable.** A release's tag and its assets are final
once published: never move the tag, and never overwrite, re-upload, or otherwise
mutate a published release's assets. If a release is wrong or incomplete — a
failed build, a missing platform, a bad artifact — cut a new patch version
through the normal flow; `latest` moves to it and the superseded release is left
exactly as published.

**Order matters — the formulae must point at the new version before the tag.**
The Linux job checks out the tag and builds its source, so the tarballs are
always correct. The **bottle** job does *not* build from the tag: it builds via
the Homebrew tap formulae on `main`, so the formulae must already point at the
new version *before* the tag is pushed. Tagging first, with the formulae still
on the old version, ships stale `OLD-version` bottles.

Forward release (the `scripts/release.sh <version>` helper automates steps 4–5):

1. **Gate green**: run `scripts/gate.sh --e2e` — the single gate command
   (`scripts/gate.sh` = `cargo fmt --check` → `check --all-targets` → `clippy
   --all-targets -D warnings` → `build` → `test`; `--e2e` appends the opt-in e2e
   tier) — i.e. `cargo build` first, then **`cargo test` AND `cargo test-e2e`**
   pass (not just `cargo check`). `cargo test` is the fast unit+int default gate
   and does **not** run the release-version guard: those tests read
   `Cargo.toml`/`Cargo.lock`/`README.md` from disk and are therefore **e2e**, so
   the guard runs under `cargo test-e2e` (`cargo test tests::e2e:: -- --ignored`)
   — a stale `RELEASE_VERSION` literal ships the release HEAD red unless the e2e
   tier is run. Build first because the cross-process tests spawn
   `target/<profile>/apg` (`src/testutil.rs`), which `cargo test` alone does not
   rebuild: a stale artifact makes the session/lock tests fail against an old CLI
   with misleading errors.
2. **Bump the version** in `Cargo.toml`, `Cargo.lock`, **and** `src/main.rs`'s
   `RELEASE_VERSION` literal (`version = "X.Y.Z"`).
3. **Commit the release content** (the version bump + whatever ships in it).
   This commit is the **release HEAD**.
4. **Repoint all 8 formulae** (`Formula/scanner.rb`, `apg-go`, `apg-java`,
   `apg-cpp`, `apg-rust`, `apg-ts`, `apg-csharp`, `apg-py`, `apg-md`):
   `tag:` → `vX.Y.Z`,
   `revision:` → the release-HEAD SHA, `root_url` →
   `releases/download/vX.Y.Z`, and bump each `rebuild N` by 1. Commit as
   *"Point formula revisions at the vX.Y.Z release HEAD"*.
5. **Annotated tag** pointing at the **release HEAD** (not the
   formula-revision commit): `git tag -a vX.Y.Z -m "apg X.Y.Z" <release-sha>`.
6. **Push (human-approved)**: `git push origin main` then
   `git push origin vX.Y.Z`. CI builds the bottles and the tarballs, then a
   single publish job commits the bottle sha256s to `main`, creates the release
   as a draft, attaches every asset, and publishes it.
7. **Verify the assets are the new version**: `gh release view vX.Y.Z` must list
   `*-X.Y.Z.arm64_sonoma.bottle.*.tar.gz` (NOT the previous version) plus the
   `apg-linux-*` tarballs. If the bottles show the old version, you tagged
   before step 4.

`scripts/release.sh` automates steps 4–5: it verifies the version is already
bumped and the tree is clean, rewrites every `Formula/*.rb`, commits the formula
revisions, and creates the annotated tag at the release HEAD. It **never
pushes** — push/tag remain human-approved acts (it prints the exact commands).

### Unified JSONL schema (abridged)

Node records:

```jsonl
{"type":"module","fqn":"github.com/foundry/flow"}
{"type":"file","path":"/abs/store.go","parent":"github.com/foundry/flow","start_line":1,"end_line":142}
{"type":"struct","id":"n12","parent":"...v1","name":"Error","path":"/abs/error.go","start":12,"end":300,"start_line":12,"end_line":45}
{"type":"function","id":"n13","parent":"...Store","name":"ComputeContentHash","params":["[]byte","int"],"file":"/abs/store.go","path":"/abs/store.go","start":1,"end":99,"start_line":34,"end_line":99}
{"type":"unresolved","fqn":"fmt.Errorf","category":"stdlib"}
```

Edge records:

```jsonl
{"type":"contains","from":"n12","to":"n13"}
{"type":"calls","from":"n13","to":"n14"}
{"type":"uses","from":"n13","to":"n12"}
{"type":"unresolved_call","from":"n13","to":"fmt.Errorf","target_type":"context.CancelFunc"}
{"type":"unresolved_use","from":"n13","to":"protoimpl.Pointer"}
```

- `id` is a scanner-local opaque counter (`n1`, `n2`, …); edges reference
  project nodes by `id`, unresolved targets by `fqn`. The ingestor maps `id`s
  to canonical FQNs. `code_type` is **not** emitted by scanners — the ingestor
  computes it from `path` + `apg/config.json` (`src/classify.rs`).
- `apg scan` injects a control record before each frontend's stream in a
  multi-language scan: `{"type":"lang_switch","language":"go"}`. The ingestor
  uses the current language for `code_type` classification and FQN rendering,
  and `--id-prefix` (per-frontend, `g1`/`t1`/…) keeps opaque ids unique across
  merged streams.
- Line numbers (`start_line`/`end_line`) are computed by the scanners (Go and
  C++ from byte offsets, Java from javac's UTF-16 char positions), never
  derived from bytes ingestor-side. `file` nodes (emitted one per scanned
  source file, including files with no declarations) carry `1..total-lines`;
  the scanner also sets the file's `parent` module.

### FQN convention (rendered ingestor-side, SPEC §4)

Every code FQN is **language-rooted** (PHASE_09): the ingestor materialises one
`Language` node per `lang_switch` stream (its FQN is the bare language id, e.g.
`rust`) and roots every module FQN under it. `<language-id>` is the stream's
declared id verbatim (`go`/`java`/`cpp`/`rust`/`ts`/`js`/`csharp`/`py`/`md`).

| kind | FQN |
|---|---|
| language root | `<language-id>` — the bare `lang_switch` id (e.g. `rust`); `Language —Contains→ Module` |
| module | `<language-id>.<module-identity>` — the frontend emits the identity verbatim, the ingestor roots it (`rust.apg`, `py.pkg.sub`, `ts.@co/ui`, `md.<repo-relative-dir>`; a path identity is rendered repo-relative to the git toplevel / scan root) |
| struct | `parent.name` — `parent` is the rooted module/scope FQN |
| function (unique in scope) | `parent.name` |
| function (overloaded) | `parent.name(T1,T2,...)` — erased, comma-separated params |
| Go `init` | `parent.init#<file-basename>` |
| TypeScript | `<language-id>.<pkg.relpath.name>` — the rooted npm package + dot-path prefix per ES-module file (`ts.@co/ui.src.components.Button.Button`); TS getters/setters land as overloads (`diameter()` / `diameter(number)`) |

A `File` FQN is its repo-relative path (relative to the git toplevel, or the
scan root outside a repo) and an `UnresolvedTarget` FQN is a foreign symbol
name — neither is language-rooted.

Because the language roots are disjoint, a cross-language module/symbol FQN
collision is impossible by construction (`rust.apg` and `py.apg` both survive).

Overloads are grouped by `(parent, name)`; any group of size > 1 renders every
member with the `(params)` suffix. The ingestor fails loudly (panics) on any
residual same-kind FQN collision rather than silently overwriting.

## LadybugDB Tooling

> **LadybugDB is a real, actively-developed project** — the successor to
> [KuzuDB](https://github.com/kuzudb/kuzu) (formerly known as Kuzu; ~6k commits,
> MIT-licensed, live releases). Do **not** assume it's fake or obsolete. Before
> dismissing it, read https://github.com/LadybugDB/ladybug (README, releases,
> docs at https://docs.ladybugdb.com) for current info — installable via
> `pip install ladybug`, `npm install @ladybugdb/core`, `cargo add lbug`, or the
> Go/Java/C++/CLI binaries.

The workspace has a LadybugDB graph database at `apg/.trans/db.lbug` containing the parsed codebase. Interact via the `apg_query` tool — a Cypher-like query interface.

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

- 5 code node types:
  - `Module` — property: `fqn`
  - `File` — properties: `fqn` (the repo-relative path from the git toplevel / scan root), `start_line`, `end_line` (`1..total-lines`), `code_type`
  - `Struct` — properties: `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type`
  - `Function` — properties: `fqn`, `path`, `start`, `end`, `start_line`, `end_line`, `code_type`
  - `UnresolvedTarget` — properties: `fqn`, `category` (a call/type reference the scanner could not resolve to a project symbol; deduplicated by name)
- 5 code edge types:
  - `Contains` — Module↔Module, Module→File, File→Struct, File→Function, Struct→Struct, Struct→Function
  - `Calls` — Function→Function
  - `Uses` — Function→Struct, Struct→Struct. Rust `impl X for Y` is a `Uses`
    edge `Y → X` (the type implements the trait); a foreign trait lands as an
    `UnresolvedUse` instead. Rust impl methods (inherent and trait) hang under
    the **self type** (`Type.method`); trait declarations/defaults under the
    trait.
  - `UnresolvedCall` — Function→UnresolvedTarget; rel-table property `target_type` (function type of a func-value call, Go-only, empty otherwise)
  - `UnresolvedUse` — Function→UnresolvedTarget, Struct→UnresolvedTarget
- Authored node labels (the `apg/layers/` node files): `Requirement` (fqn,
  body, feature), `Stakeholder`, `User`, `DomainGroup` (the `domain.group`
  type — `Group` is a reserved word; `attribute` core/supporting/generic,
  optional `root`), `Entity`, `Value`, `Service`, `System`, `Container`
  (`kind` app/service/db/queue), `Component`, `Person`, `Constraint` (prose;
  local ones carry `attaches_to`), `Note`. Transient labels: `Plan`,
  `PlanPhase`, `Task` (kind/tier/status), `Feedback` (body/status/
  disposition). The four Implementation kinds may carry `status: planned` for
  not-yet-built code (see "Planned Implementation nodes" below).
- Authored edge labels: `Contains` (hierarchy), `Drives` (Requirement →
  Group/Entity/Value/Service), `RealisedBy` (Group/Entity/Service →
  System/Container/Component), `SpecImplementedBy` (the DB rel-table name for
  the authored `implemented-by` kind: System/Container/Component → code),
  `Calls` (Service→Service), `Publishes`/`Subscribes` (Service → event
  Entity), `DependsOn` (Requirement→Requirement), `Uses` (Person→System),
  `Represents` (User→Entity, Entity→Person), `Details` (Note→any).
  Transient: `Gates` (PlanPhase→PlanPhase), `Satisfies` (PlanPhase→
  Requirement), `Reviews` (Feedback→any). Dependency and `contains` trees are
  acyclic; a dangling FQN is a write-time error (see the layers section
  below).

### `UnresolvedTarget.category`

One of `builtin` (Go predeclared func/type), `stdlib`, `external`, `func-value` (call through a function-valued variable or IIFE), `interface-method` (method on a universe-scope interface, e.g. `error.Error`), or `unknown` (fallback / frontend omitted it). Go populates this exactly from the type checker; Java classifies stdlib (`java.*`/`javax.*`/`jdk.*`) vs `external`; C++ is heuristic (`external` for qualified names, `func-value` for bare identifiers). Rust classifies exactly via crate origin (sysroot `std`/`core`/`alloc` → `stdlib`, dependency crates → `external`, project `macro_rules!` → `unknown`, closure calls → `func-value`).

Type conversions in Go (`[]byte(x)`, `protoimpl.Pointer(x)`, `(*T)(nil)`) are routed to `Uses`/`UnresolvedUse` edges, not `UnresolvedCall`. The `target_type` property only carries data on `UnresolvedCall` edges whose target is `func-value`.

### The authored graph: layers, spine, plans, feedback

The **durable spec** is a node-file store, not a JSONL: one file per node under
`apg/layers/`, authored via `apg node add|rm` / `apg edge add|rm` (never by
editing files by hand — the binary validates the schema, the pairings, and the
references, and auto-commits each mutation). FQN = **`<layer>.<type>.<name>`**,
no project prefix, no `spec.<id>` vocabulary; the **file name IS the identity**
and the node-file's `layer`/`type`/`name` fields must match the path. Both
halves of every edge live in the node files (out in the source's file, the
matching in in the target's); a pairing mismatch fails at scan. The six layers:

| layer | types |
|---|---|
| `requirements` | `stakeholder`, `user`, `requirement`, `note`, `constraint` |
| `domain` | `group` (attribute core/supporting/generic, optional `root`), `entity` (kind `entity`\|`event`), `value`, `service`, `note`, `constraint` |
| `solution` | `system`, `container` (kind app/service/db/queue), `component`, `person`, `note`, `constraint` |
| `implementation` | `note`, `constraint` (attach-only — the real nodes are scanned code) |
| `global` | `constraint`, `note` (the laws) |
| `plans` (transient) | `PlanPhase`, `Task`, planned Implementation nodes — `.trans/plans/` only |

**Constraints are prose**: the binary validates structure + references at write
time; whether the prose holds is assessed by review, never executed. A
`domain-rule`-style law is a `constraint` node, not an invariant.

The **spine** threads the tiers end to end:
`Stakeholder ⊃ Requirement —drives→ Domain —realised-by→ Solution
—implemented-by→ code`. Authored edge kinds: `contains`, `drives`,
`realised-by`, `implemented-by` (target is a code FQN, validated against the
scanned graph: resolves → real; planned → pending, not an error; gone →
drift, an error), `calls`, `publishes`/`subscribes` (Service → event Entity),
`depends-on`, `uses` (Person → System), `represents` (User → Entity, Entity →
Person), `details` (Note → any). Any requirement traces down to the code that
implements it; any code traces up to the why. A requirement is `delivered`
when review concludes the spine reaches it — satisfaction is by review, not
asserted by the binary.

### Planned Implementation nodes

- Code a plan will build exists in the graph *before* it is written: the
  plan-writer declares it as a **planned Implementation node** — a
  `Module`/`File`/`Struct`/`Function` record at its **real code FQN** carrying
  `status: planned` (`apg plan add <project> planned <kind> <fqn>`). A planned
  node has no location and is the target of a `creates` task.
- When the code actually exists, the next **branch scan replaces the planned
  node**: the scanner finds the FQN, clears `status`, fills in the location,
  and re-points incident edges. On `main` there are no planned nodes — every
  Implementation node is real code and the spine resolves straight through to
  it.
- The plan-writer never authors spec tiers 1–3 and the spec-writer never
  authors planned code.

### Plans, tasks & feedback (transient)

- `Task {fqn, title, kind, tier, status}` carries a two-axis classification:
  `kind` ∈ source/test/gate/docs is the **owning role** (orthogonal,
  `source` default); `tier` ∈ unit/int/e2e is the verification depth,
  required iff `kind = test`, rejected otherwise. Every task is
  implementer-workable — the human's decision point is plan end: the verify
  gate + merge act are the single delivery moment.
- `Feedback {fqn, body, status, disposition}` is a review item;
  `status` ∈ open/actioned/resolved. An artifact is done only when every
  `Feedback` on it is `resolved` — `apg plan complete` and the verify gate
  refuse otherwise. Feedback (and the plan itself) lives in the `.trans`
  mirrors and dies with the branch.
- Query patterns: the spine `MATCH (r:Requirement)-[:Drives]->(:Entity)-[:RealisedBy]->(:Container)-[:SpecImplementedBy]->(c) RETURN r.fqn, c.fqn`; plan health via the suite tools (`apg_plan`, `apg_plan_phases`, `apg_plan_tasks`, `apg_review`).
- The six distributed agents (installed by `apg init`): `codebase-navigator`
  (orchestrates the flow — `apg project start` from main, per-branch DB
  build, coordinator-mediated feedback routing (dispatch one open item to its
  owning writer, take the writer's ACTIONED/WONT-FIX claim, run the shallow
  claim-vs-change check, then action it), human-gate summary, and the merge act
  (verify gate → merge → rebuild) on approval), `spec-writer` / `plan-writer`
  (author through the `apg_node`/`apg_edge`/`apg_plan_*` tools, **no file
  writes**; the spec-writer authors the layer tiers + spine + reconciliation
  mode, the plan-writer the tier-4 delta with structural/holistic gates),
  `spec-review` / `plan-review` (attach/resolve/reject feedback, **no authoring
  tools**; approval-only wont-fix — the coordinator actions the writer's
  `--wont-fix` claim and only the reviewer makes it terminal), and
  `agent-builder` (`mode: primary`, the only write grant
  `.opencode/agents/**`, scaffolds a repo's code-writer agents).
  Repo-defined implementer / implementation-phase-reviewer agents are generated
  by `agent-builder` (assertion-only `plan done`, task notes, branch commits;
  phase review on branch scans + the final implementation review discovering
  divergence — fix code or reconcile the spec).
- `agent-builder` scaffolds agents **into the repo it is run against**. When
  that repo is the apg repo itself, its `.opencode/agents/**` are consumer
  artifacts refreshed out-of-band by the maintainer with the **installed
  (released)** binary — never as a task in a feature change-set, and never by
  pointing a candidate build at the apg repo. A change-set that changes the
  agent-builder template edits `opencode-suite/agents/**` (product source,
  embedded via `include_str!`); it does not regenerate the repo's own agents.

### `Struct.code_type` / `Function.code_type`

Classifies what kind of code a node lives in: `src` (default), `test`, `generated`, `external`, `lib`, or a user-defined value. All code is included in the graph; this column is how you filter it. Example: `MATCH (n:Function) WHERE n.code_type = 'test' RETURN n.fqn`.

Built-in defaults (per language):
- **Go**: `test` = `_test.go` or `test`/`tests` path segment; `generated` = `*.pb.go` or `gen`/`generated` segment; `external` = `vendor` segment.
- **Java**: `test` = `*Test.java`/`*Tests.java` or `test`/`tests` segment; `generated` = `gen`/`generated` segment; `external` = `vendor`/`third_party`.
- **C++**: `test` = `*_test.cpp`/`test_*.cpp` or `test`/`tests` segment; `generated` = `*.pb.cc`/`*.pb.h` or `gen`/`generated` segment; `external` = `vendor`/`third_party`/`external`.
- **Rust**: `test` = `*_test.rs` or `test`/`tests` segment; `generated` = `gen`/`generated` segment; `external` = `vendor`; else `src`.

An `apg/config.json` at the project root **replaces** the defaults. Shape:

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

`globs` match the full path; `names` match the node simple name or FQN. First matching type (list order) wins, else `default`.
- Nodes without locations (Modules, UnresolvedTargets) have no `path`/`start`/`end` (and no `code_type`).
- `start` and `end` are **0-based byte indices**, not line numbers. Every
  located node also carries `start_line`/`end_line`, **1-based inclusive line
  numbers**; use them (not byte offsets) when joining against diffs, hunks, or
  anything line-oriented.
- `path` is the **repo-relative source-file identity** — the `/`-separated path from the git toplevel (or the scan root outside a repo), the same identity as a `File` FQN, never an absolute checkout path (the absolute path is reconstructed at the suite-tool boundary). Read those files with `read`, `grep`, or `bash` (resolving the relative path against the project directory) to **confirm and anchor** a graph result (open the file at its `start_line`/`end_line`) or to read artifacts the graph does not model — never to discover a fact the graph carries.

### Fidelity & noise

- **Java, Go, Rust, TypeScript, and C# edges are exact** — resolved via the
  compiler's type checker (javac attribution / `types.Info`), rust-analyzer,
  the official TypeScript compiler API, or Roslyn. A `Calls` edge always points
  at the real declared method.
- **C++ edges are heuristic** (tree-sitter + scope/type tracking). Unresolvable calls/types are recorded as `UnresolvedCall`/`UnresolvedUse` rather than guessed.
- **The scanner never guesses**: if a call/type can't be resolved to a project symbol, it becomes an `UnresolvedTarget` edge, never a fabricated FQN.
- **All code is included** — tests, generated, and vendored code are scanned like everything else (the only exclusions are user `--exclude-path` patterns, `node_modules`, and files the compiler/frontend can't process). Filter by `code_type` instead.
- **Multi-module repos**: Go workspaces (`go.work`), C++ monorepos, Cargo
  workspaces, and npm workspaces are supported. Each module is a top-level
  `Module` node; FQNs are module-prefixed so they stay unique across modules.
  Pass `modules: "dir1,dir2"` to `apg_scan` to restrict scanning to specific
  modules (Go/C++/Rust/TS/C#).
- **Multi-language repos** (a Go backend + TS frontend, say): `apg scan`
  auto-detects every language present and merges their graphs into one
  database. Opaque ids are namespaced per language (`--id-prefix`, `g1`/`t1`/…);
  a `lang_switch` control record before each stream drives per-record
  `code_type` classification and FQN rendering.
- To see what the scanner couldn't resolve: `MATCH (f)-[:UnresolvedCall]->(u) RETURN u.fqn, count(f) ORDER BY 2 DESC LIMIT 20`

### Other tools

- `read`, `grep`, `glob`, `bash` — standard file operations. They **confirm and
  anchor** a graph result (open the returned `path` at its
  `start_line`/`end_line`) or read artifacts the graph does not model — they
  do not discover graph facts. For ANY code or structure question — discovery
  and enumeration included ("what is in this file/module?", "what does this
  unit depend on?") — the first tool call is a graph query (the `apg_*` suite,
  or `apg_query`); reach for a file tool second.
- Extract byte ranges with: `dd if=<file> bs=1 skip=<start> count=<end-start> 2>/dev/null`
- Use `rg` (ripgrep) in bash for fast content search **of artifacts the graph
  does not model** — for in-graph facts, a graph query comes first.
- `task` — spawn sub-agents for complex multi-file exploration.
