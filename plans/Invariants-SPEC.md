# Invariants-SPEC.md — graph-wide invariant mechanism

Status: working spec (authoring in progress; materialized into the graph as a spec project later).
Graph model: **GraphModel-SPEC.md** (4-tier active-knowledge graph; present-ness = branch membership).

## Purpose

A graph-native mechanism to store **invariants** — rules that artifacts must respect — in the
program graph, where they are discoverable, citable, and explicitly linked. Invariants are
**graph-wide**: they guard **Specs**, **Plans**, and **code** alike. They are the discipline
layer that turns recurring rules into first-class graph citizens instead of prose scattered
across the CLI, agent files, and docs.

## Principles

- **Optional and emergent.** The flow works identically with zero invariants. There is no seed
  set and no precondition — invariants appear as patterns in review feedback become clear.
- **Correctness never depends on them.** CLI envelope validation + reviewer judgment carry the
  load. Invariants make recurring rules explicit, discoverable, and linkable; they never
  replace the enforcement layers.
- **Linking is selective.** A Feedback → Invariant link (`Checks`) is optional (0..n). Most
  feedback is *not* an invariant violation — internal consistency, ambiguity, coverage, and
  clarity live outside the invariant model.

## Node schema

```json
{"type":"invariant",
 "fqn":"invariant/<name>" | "<project>/invariant/<name>",
 "title":"...",
 "body":"...",
 "category":"process|product|graph-integrity",
 "scope":"spec|plan|review|code|...",
 "status":"active|retired"}
```

- **Roots**: `invariant/...` for universal rules; `<project>/invariant/...` for
  repo/project-specific rules (project-scoped and stable; present-ness is branch-determined,
  not an FQN property — see GraphModel-SPEC.md).
- **Categories**:
  - `process` — rules about spec/plan/review structure and workflow, e.g. "every plan task
    carries exactly one kind from {source, test, gate, docs}; a test task requires a tier
    (unit/int/e2e), a non-test task rejects one".
  - `product` — domain rules about the codebase/artifacts, e.g. "release records' version
    equals the release tag".
  - `graph-integrity` — data rules about the graph itself, e.g. "Details edges target only
    the 14 allowable labels", "no orphan Note nodes".

## Edges

- `GuardedBy` (artifact → Invariant) — an artifact (Spec, Plan, Requirement, Task, or a code
  node) is guarded by the invariants it must respect. Makes a reviewer's in-scope checklist
  **queryable and verifiable**.
- `Checks` (Feedback → Invariant) — a review feedback cites the rule it enforces, where
  relevant. Optional.

## Tools

- `apg invariant add [<project>] <name> --title … --body … --category … --scope …
  [--guard <fqn>]*` — materialize a new invariant, optionally linking `GuardedBy` to the
  artifacts it guards. Suite tool `apg_invariant_add.ts`.
- `apg invariants` (read) — list invariant nodes, filter by scope/project. Suite tool
  `apg_invariants.ts`. The awareness tool for writers and reviewers.
- Lifecycle: `--guard` at add time; retire/amend via status flip (`apg invariant rm` /
  `apg invariant link` as the lifecycle stabilizes).

## Lifecycle (emergent)

1. Reviewers attach feedback; where a comment enforces a known rule, they link
   `Checks` → Invariant.
2. As patterns recur across feedback cycles and artifacts, the **codebase-navigator** proposes
   a new invariant to the user.
3. On user confirmation, the invariant is materialized via `apg invariant add`.
4. The navigator injects the active invariant set into writer/reviewer delegation prompts;
   writers and reviewers also query `apg invariants` themselves.
5. Invariants can be amended or retired (status) as the system evolves.

## Roles

- **Writers** (spec-writer, plan-writer): aware of the invariants in scope (prompt injection +
  tool); author so known invariants hold from the start.
- **Reviewers** (spec-review, plan-review, implementation-phase-reviewer): check artifacts
  against their `GuardedBy` invariants; cite a violated invariant via `Checks` where relevant.
- **codebase-navigator**: proposes new invariants as patterns emerge; injects the active set
  into prompts; routes the review loop.
- **User**: approves invariant materialization.

## Query patterns

```cypher
MATCH (i:Invariant) RETURN i.fqn, i.category, i.status
MATCH (a)-[:GuardedBy]->(i:Invariant) WHERE a.fqn = '<project>/spec' RETURN i.fqn
MATCH (f:Feedback)-[:Checks]->(i:Invariant) RETURN f.fqn, i.fqn
```

## Domain specs

The mechanism is graph-wide; each domain defines its own spec and invariants. The graph model
and node taxonomy live in **GraphModel-SPEC.md**; the first descendant flow spec is
**SpecCreation-SPEC.md** (the spec capture + writing flow).