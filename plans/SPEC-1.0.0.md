# SPEC 1.0.0 — notes (prose capture, to review later)

> Working notes from the design conversation. Nothing here is final; it is the raw
> material to be turned into a graph-native spec (parent + child specs) once 0.9.1
> lands and we can dogfood `carto` against its own graph. Treat every line as a
> candidate decision, not a commitment.

## Positioning

`apg` (a program graph) no longer describes what the project does. We now graph the
**active knowledge that drives a program** — not just the code, but the intent (why)
and the trajectory (what's next), as native nodes in the same graph as the code they
produce. A one-hop query answers "why does this function exist," not a prose search.

The map/cartographer metaphor is the one to lead with: it's a *map* you navigate (the
agents already "navigate" the graph), not a "graph" data structure. "Cartographer" was
independently chosen for an inspired-by service in the `~/platform` test bed, which
signals the metaphor already resonates.

## Name

- Leading candidate: **anima** (brand and binary — `anima scan`, `anima query`).
- Etymology: Latin **anima** — "breath, vital principle, soul"; the source of
  "animate," "inanimate," "animal." The layer that turns inert source into *living
  knowledge* — the "active knowledge" positioning in one word.
  - A quiet nod to the lineage: a-n-i-m-a traces back through "Alex's [Program] Graph"
    without carrying the dead "graph" framing.
- Alternates captured so the decision is reviewable: **telos** (purpose/why), **sophia**
  / **gnosis** (wisdom/knowledge), **lore** (accumulated knowing), **atlas** (map).
- Availability sweep required before commit: domain, crates.io/npm/brew binary name,
  GitHub org. "Anima" is soft-collision (some art/software projects use it) — much
  softer than `carto`'s clash with CartoDB.
- Etymology note: originally "Alex's Program Graph" (`apg`), made by an engineer called
  Alex. The "program graph" framing is exactly what the rename leaves behind.
- Rename is load-bearing: binary, package, tap `antz29/apg`, formulae (`apg-*`), `apg/`
  dir, `.trans`, `~/.opencode/tools/apg_*` suite, `apg_` prefix across six agents.
- **This is the cheapest rename moment** — pre-1.0, small install base. Post-1.0 the
  same rename becomes a migration story.

## License

- Leaning **Apache-2.0** (from MIT). Rationale: explicit patent grant + contributor
  protection clause, matters before opening to contributors.
- Sweep: `LICENSE`, `Cargo.toml` `license`, README, all `Formula/*.rb` `license "MIT"`.

## Repository structure

- **Split monorepo** into `carto` (core: Rust ingestor + CLI + tool suite + agents)
  plus six frontend repos: `carto-go`, `carto-java`, `carto-cpp`, `carto-rust`,
  `carto-ts`, `carto-csharp`.
- Each frontend is its own language/toolchain with its own build pipeline → each gets
  its own repo + native CI + own versioning + own contributors.
- Forces a **stable frontend interface**: the JSONL schema (§2) + FQN rules promoted to
  a versioned cross-repo contract.
- `build.rs` orchestration dies or changes form; local dev installs frontends (brew) or
  uses a dev harness.
- Reconcile/decide: history rehome across repos; coordinated 1.0 launch of seven repos.

## Frontend contract (versioned handshake)

Direction: "make a new frontend, validate against the latest `carto` binary."

- **`carto frontend validate`** — a test/validation mode in the published (small)
  `carto` binary that validates a frontend's JSONL output against the schema +
  invariants, with a real exit code and a record-by-record report. The binary is the
  oracle — a Go/C#/TS frontend can't depend on a Rust crate.
- Frontend-side contract: each frontend exposes a **`SUPPORTED_CARTO`** version string —
  the schema range it was validated against (not the `carto` CLI release).
- Runtime compatibility: `carto scan` reads `SUPPORTED_CARTO`, compares to its own
  schema revision; compatible → run (warn if older-but-backward-compatible),
  incompatible → refuse with a clear "re-validate with `carto frontend validate`".
- **Version string is a range, not one revision** — e.g. `1.x`. Do **not** hand-roll a
  matcher ("Option B is a disaster"); adopt the **`semver` crate** (`Version` +
  `VersionReq`) for full range grammar.
  - Expressiveness therefore depends on available library support (the `semver` dep).
  - Decide the *bump policy* (what's additive=minor vs breaking=major) as the wall the
    range bounces off: additive record/edge types + optional fields = patch/minor;
    field rename / FQN-rule change / removed type / semantic change = major.
  - Likely still *restrict what frontends may declare* (reject bare `*`/`>1` as sole
    bound) even though the parser can match anything.
  - Keep the **schema revision** and the **`carto` CLI semver** conceptually distinct.
- Two halves of one contract: conformance mode (validate) + runtime assessment.

## Frontend CI

- Per-frontend CI in each native toolkit: build → run on a golden fixture → pipe stdout
  to `carto frontend validate` (schema drift caught in the frontend repo, not at first
  real scan).
- Golden-fixture diff mode (`--update` style).

## CI / quality for 1.0

- **Rust core test gate** before any tag release (currently tests run only locally).
- **Per-frontend CI lanes** (above), independent of the Rust core gate.
- **PR-check + contributor onboarding**: PR-level CI pays off once there are active
  contributors — candidate to defer to post-1.0, behind license + contrib-agreement
  decision.
- Contributor agreement is its own decision, coupled to the Apache-2.0 choice.

## Residual gaps (unruled — candidate open questions)

- **Windows**: currently macOS + Linux only; 1.0 should state unsupported/support.
- **C++ fidelity**: Go/Java/Rust/TS/C# edges are exact (compiler type-check); C++ is
  heuristic. Formally declare C++ best-effort in the contract.
- **Stable/semver surface**: name the stable surface (JSONL schema, FQN rules, DB
  schema, CLI flags, tool names).
- **Committed SPEC.md + CHANGELOG**: both absent today; SPEC.md is constantly referenced
  by AGENTS.md but not checked in.

## Spec decomposition (parent + children)

Parent **release** spec (`carto-1.0`?) holding cross-cutting gates (1.0.0 version,
launch flag-day, changelog, license sweep coordination) with `SpecDependsOn` → children,
added as we work:

| child | scope |
|---|---|
| `carto-rename` | brand/binary rename; `[[bin]]`, `Cargo.toml`, dir layout (`apg/`→`.carto/`), `apg_*`→`carto_*` prefixes, tap/formulae, installers, README |
| `carto-license` | Apache-2.0 migration sweep |
| `carto-repo-split` | monorepo → `carto` + six frontend repos |
| `carto-frontend-interface` | validate mode + `SUPPORTED_CARTO` handshake + schema revision + bump policy |
| `carto-frontend-ci` | per-frontend CI over the validate oracle |

Dependency DAG: rename / license / repo-split are mutually independent (parallel);
`frontend-interface` depends on `repo-split`; `frontend-ci` depends on both `repo-split`
and `frontend-interface`.

## Sequence

1. **0.9.1** — get `apg scan` working on this repo (see `SPEC-0.9.1.md`); proof = a
   successful, non-empty scan. Prerequisite: nothing can be dogfooded until this.
2. **Then 1.0.0** — rename, license, repo split, frontend contract, CI — as the
   coordinated flag-day, spec'd graph-natively once the graph exists.
