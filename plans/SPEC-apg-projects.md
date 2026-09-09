# SPEC — apg projects model + node-file serialization

The apg change-set that ships the first-class project concept (the mutation guard) and the
node-file serialization model ("serialize nodes, not specs"). Change-set: `apg-projects`,
standalone, bootstrap-spec'd with the v0.10.4 binary in the old model.

## 1. Governing principles

1. **A project is a container for a change-set over the graph** (and the code that underpins
   the implementation graph). A project ≠ a spec and ≠ a plan — it *contains* those things.
2. **It is not possible to mutate the graph without a project — at all.** "There is no spoon":
   projects are ephemeral change-set containers; the nodes are the reality. A spec/plan/review
   JSONL on main means only that there is a spec in the graph — it is not a project and carries
   no project semantics.
3. **Serialize nodes, not specs.** One file per node; no project segment anywhere in the path.
   Present-ness is branch membership of the file: on main = present, on a branch = proposed.
4. **Merge-conflict localization is the goal:** different change-sets touching different nodes
   never collide on a file; the same node edited by two change-sets conflicts *at that node* —
   a meaningful conflict.
5. **The endstate is the thing we care about.** Decisions are not modeled — the code/structure
   is the record. Explanations the structure doesn't carry become notes.

## 2. The projects model

### 2.1 Project start

`apg project start <name>` — creates the project context: worktree + branch, off the repo's
DEFAULT branch (not a literal `main`; symbolic HEAD resolved). Branch name == project name is
the membership mechanism. Worktree location is fixed: `<main>/apg/.worktrees/<project>` (a
gitignored path inside the main checkout — tested and verified safe; gitignored paths are not
part of the worktree).

- Auto-scans as part of start: one command → worktree + branch + branch DB.
- **Idempotent only when `<name>` matches the current project context** (already in that
  project's worktree/branch → no-op, print the path). Otherwise hard fail.
- **Always a branch — no escape hatch.** No `--no-branch`, no degradation.
- **Hard refuse:** non-git dirs (suggest `apg init`), dirty main at start, unborn HEAD
  ("commit an initial state first"), invalid branch names (never sanitize — project names
  inherit git refname constraints), and every collision ("project already exists" — branch
  checked out elsewhere, branch without worktree, worktree dir present but not a worktree:
  case-specific messages naming the actual state and the fix command).
- **`project start` is a main-checkout operation** — run from inside a worktree → hard fail
  ("you work one project at a time").

### 2.2 The project membership guard

- **Guards writes only; reads are always allowed.** Writes refuse outside a project context.
  The durable mutation surface is `apg node` / `apg edge` — the spec/invariant/note writes of
  the old model are node/edge mutations here (a requirement is a node; an invariant is a
  constraint node; a note is a note node). Plan and review mutations are the transient
  surfaces, over `.trans`. Reads, queries, and scans work from
  anywhere. `project start` itself is unguarded (the entry point).
- **Membership = "the project's worktree, on the project's branch"**: current branch ==
  project name AND current checkout is the project's worktree (path under
  `<main>/apg/.worktrees/<project>`). Failure messages name which half failed.
- **Main is never a mutation place** — delivered or not. Post-merge changes are new
  change-sets. No "adoption" of legacy JSONLs: a spec on main just is; starting a project
  brings it along in the worktree.
- **Universal-scope artifacts (e.g. global constraints) are authored from inside a project**
  like any mutation; scope is orthogonal to mutation context.
- **Error UX:** exit code 1; stderr message = (1) which membership half failed, (2) one
  actionable fix line (`run apg project start <name> from the main checkout` / switch to the
  worktree at `apg/.worktrees/<project>`).
- **`apg plan apply` is renamed `apg plan verify`** — the binary applies nothing; verify is
  the pre-merge coherence gate (no remaining planned/dangling nodes + all feedback resolved),
  guarded (verdict only meaningful against the branch's graph).

### 2.3 Binary implementation

- **git2 crate, `default-features = false`** (no https/ssh → no OpenSSL). The git CLI is
  never shelled out to; push/tag remain human acts.
- **Root resolution split:** keep the existing walk-up for checkout-local `apg/` (correct in
  worktrees by construction); git2 for identity (branch, checkout path, main path, worktree
  existence). Invariant: the git checkout root contains the `apg/` the walk-up found — verify
  cheaply, error on divergence.
- **The guard lives in the central mutation funnel** — `write_jsonl_and_reingest`
  (src/artifacts.rs:72), beside the existing staleness gate. One check covers all mutations.
  Consequence: non-git test fixtures gain a real project context (real git fixtures with
  branch + worktree).
- **No `APG_NO_PROJECT_GUARD` escape hatch — of any kind.** Reads are unguarded, so tooling
  never needs to mutate outside a project.
- **Graph mutations auto-commit** on the project branch via git2; codebase changes are
  committed by the implementer (unchanged agent flow). After an auto-commit the staleness
  gate's recorded scan_meta is re-anchored (DB and tree in sync by construction).
- **`apg project merge`** = verify gate → merge → main rebuild (binary-operated via git2,
  from the main checkout — the project's terminal lifecycle act). The rebuild is a plain
  unguarded scan on main.

### 2.4 Init & version gate

- `apg init` scaffolds `apg/.worktrees/` + its gitignore entry, and writes the binary version
  into `apg/config.json` as a binary-managed `version` field (user code_type rules untouched).
  `project start` self-heals the dir/gitignore if absent.
- **Version gate (blocks, not warns):** same major.minor → proceed (patch diff fine); missing
  `version` (pre-versioning) → block; major or minor mismatch in EITHER direction → block with
  upgrade guidance. Applies to `apg scan` and `apg project start` (the layout-touching ops);
  `apg init` is the upgrade act. Upgrade instructions ship as a suite doc
  (`~/.opencode/lib/apg-upgrade.md`).

## 3. The tier model

One model per layer. Requirements = stakeholder/requirement hierarchy; Domain = DDD semantics
with plain names (DDD nomenclature is cryptic); Solution = C4 only; Plans = the bridge;
Implementation = the code; Global = the laws.

### 3.1 Layers and node types

| tier | layer dir | node types |
|---|---|---|
| 1 Requirements | `layers/requirements/` | `Stakeholder`, `User`, `Requirement`, `Note`, `Constraint` |
| 2 Domain | `layers/domain/` | `Group`, `Entity`, `Value`, `Service`, `Note`, `Constraint` |
| 3 Solution | `layers/solution/` | `System`, `Container`, `Component`, `Person`, `Note`, `Constraint` |
| 4 Plans | `apg/.trans/plans/` (transient) | `PlanPhase`, `Task`, planned Implementation nodes |
| 5 Implementation | `layers/implementation/` | only things that attach to code: `Note`, `Constraint` |
| — Global | `layers/global/` | `Constraint` (the laws), `Note` (attached to them) |

The catalog is **six logical layers** (requirements, domain, solution, plans, implementation,
global). Storage policy is separate from the catalog: plans serialize only under
`apg/.trans/plans/` (transient, per branch); implementation's actual nodes ARE the code in the
branch (scanned, never serialized) — only its attach-only Note/Constraint files exist durably
under `layers/implementation/`; the other four layers serialize durably under `apg/layers/`.

**Requirements:** `Stakeholder` = anyone with an interest ("a thing that has an opinion");
`User` ⊂ Stakeholder = "a thing that uses the system"; `Requirement` tree all the way down
(theme → epic → feature → story as one node type at different depths; the spec-writer agent
carries explicit decomposition guidance — decompose until each requirement is atomic/testable).

**Domain (DDD semantics, plain names):**
- `Group` — hierarchical (groups in groups); attributes `core/supporting/generic` and `root`
  (for aggregate-groups). BoundedContext/Subdomain/Aggregate/DomainRule all collapse into it —
  aggregate specialness is behavioral and lives in implementation, not structure.
- `Entity` — kind `entity` | `event` (events are ephemeral entities with motion, not a type).
- `Value` — immutable (ValueObject).
- `Service` — stateless behaviour (DomainService; home of what was DomainProcess).
- Group coupling is **derived, never stored**: A and B are coupled iff a service/event edge
  chain connects them. The DDD context-map flavors (direct / published / translated / shared /
  coevolving) are **edge attributes**, never node types or Group→Group edges.

**Solution (C4 only):** `System`, `Container` (kind app/service/db/queue), `Component`,
`Person` (the C4 view of User/Stakeholder — same individual, solution-side vocabulary).

**Plans (the bridge):** plan nodes exist only in `.trans` (see §5).

**Implementation:** the code in the branch — scanned, never serialized.

**Global:** `Constraint` — declarative ("X must hold"), guards the whole graph; the binary
validates a constraint's *structure and references* at write time; whether the prose statement
actually holds is assessed by review (non-deterministic), never executed. Local constraints
attach to any tier-1–3 node (requirement AC, domain law, design bound). Constraints declare
what must hold about something that **exists** — never a non-thing.

### 3.2 The spine

Strictly sequential — no tier skips:

```
Stakeholder ⊃ Requirement —Drives→ Domain —RealisedBy→ Solution —ImplementedBy→ code
```

- `User` → domain role (an `Entity`, e.g. Customer) → `Person` is the same individual
  expressed through the chain (`represents` edges; one User may represent many Entities).
- `Person --uses--> System` is a same-tier C4 relationship, not a spine hop.

### 3.3 Edge kinds — validation matrix (enforced at write time)

| kind | source | target |
|---|---|---|
| `contains` | Stakeholder/User/Requirement | Requirement |
| `contains` | Requirement | Requirement |
| `contains` | Group | Group/Entity/Value/Service |
| `contains` | System | Container |
| `contains` | Container | Component |
| `drives` | Requirement | Group/Entity/Value/Service (Domain) |
| `realised-by` | Group/Entity/Service (Domain) | System/Container/Component |
| `implemented-by` | System/Container/Component | code FQN (validated vs scanned graph) |
| `calls` | Service | Service |
| `publishes` | Service | Entity (kind: event) |
| `subscribes` | Service | Entity (kind: event) |
| `depends-on` | Requirement | Requirement |
| `uses` | Person | System |
| `represents` | User | Entity (domain role) |
| `represents` | Entity | Person |
| `details` | Note | any node |

Coupling flavor (direct/published/translated/shared/coevolving) is an edge attribute on
calls/publishes/subscribes — never a stored Group→Group edge.

Node rules: name allowlist `[a-z0-9][a-z0-9-]*` (refuse, never sanitize); type must exist in
its layer; `Entity` requires kind entity/event; `Group` takes core/supporting/generic +
optional root; `Container` takes app/service/db/queue; **FQN = `<layer>.<type>.<name>`** for
authored nodes (code nodes keep their language-native FQNs) — names unique per (layer, type);
contains/depends-on trees acyclic. Spine is sequential (lint). Dangling FQN references are
write-time errors.

## 4. Serialization

### 4.1 Layout

```
apg/layers/
  requirements/{stakeholder,user,requirement,note,constraint}/<name>.json
  domain/{group,entity,value,service,note,constraint}/<name>.json
  solution/{system,container,component,person,note,constraint}/<name>.json
  implementation/{note,constraint}/<name>.json
  global/{constraint,note}/<name>.json
apg/.trans/          (transient — mirrors the structure)
  plans/             (the plan, per branch; also the tier dir of plan nodes)
  requirements/ domain/ solution/ implementation/ global/   (feedback, in the tier dir of the attached node)
```

- One file per node. The file name IS the identity: FQN = `<layer>.<type>.<name>` (global
  per-layer namespace, no project prefix). Short ids (R1) may exist as metadata only, never
  as identity.
- **Node-file schema** (the binary checks that `layer`/`type`/`name` match the path and
  derives the FQN):

  ```json
  {
    "name": "place-order",
    "type": "requirement",
    "layer": "requirements",
    "body": "A customer can place an order.",
    "properties": {},
    "out": [{ "kind": "drives", "target": "domain.service.checkout", "properties": {} }],
    "in":  [{ "kind": "contains", "source": "requirements.user.customer", "properties": {} }]
  }
  ```
- **Both in and out edges live in the node file.** An edge appears in both endpoint files
  (out in the source's, in in the target's). **Validation: an in/out edge in one file without
  the matching out/in edge in the other endpoint's file is an error** (caught at ingestion);
  a match means the same source, kind, target, AND edge properties — not merely endpoint
  existence. Outgoing edges are canonical for building the graph.
- **Transient-to-durable relationships stay entirely in `.trans`**: feedback (and plan)
  edges are recorded BOTH halves in the transient store, referencing durable node FQNs —
  committed node files never contain transient references. Ingestion combines the durable
  nodes with their transient relationships and validates the pairs.
- **Code endpoints are exempt from the pairwise rule** — code nodes have no files. The
  `implemented-by` edge is recorded on the spec side only, as a code FQN, validated against
  the scanned graph: resolves → real; planned (in `.trans`) → pending, not an error; **gone
  from the scanned graph → error** (spec drift). The scanned graph is the stronger check.
- **Renames / deletions are atomic write-throughs:** rename = file move + FQN change +
  rewrite of every referencing file; delete = file removal + rewrite of incident edges out of
  referencing files. **One logical mutation updates ALL affected files and commits once** —
  the complete proposed change is validated before anything is written; if applying fails,
  the previous state is restored (never leave mismatched endpoint files).
- **Constraints are prose**: the binary validates a constraint's structure and references at
  write time; satisfaction is assessed by review, not executed (no constraint-expression
  language in this change-set).
- **No migration:** legacy `apg/specs/*.jsonl` and `apg/notes/` are not read (version gate
  blocks old layouts; re-materialize instead). A migration would institutionalize the wrong
  model — the lossy mapping is the spec-writer's judgement, not a converter's. The old
  `apg spec` / `apg invariant` command surfaces and the old-model graph vocabulary are
  removed from the binary — node kinds are exactly the §3.1 catalog plus the code kinds and
  the transient plan/review kinds; edge kinds are exactly the §3.3 matrix plus the §5
  plan/feedback edges; nothing else remains.

### 4.2 Auto-commit

Each node/edge mutation writes its file(s) and auto-commits (one commit per logical
mutation — all affected files together, single-file diffs when the mutation touches one
file); staleness re-anchoring moves with each auto-commit. Plan mutations never
commit — `.trans` is gitignored and transient; branch commits carry only code + node files.

## 5. Plans & transient data

- **Plans are tier 4 — the bridge.** Plan nodes (PlanPhase, Task, planned Implementation
  nodes) exist only in `.trans/plans/` (per branch), never durable.
- **The plan is the HOW for the whole solution — coverage is derived and enforced:** every
  solution node's `implemented-by` FQN must be touched by at least one plan task; the bridge
  is complete iff coverage holds.
- Plan edges: `contains` (Plan ⊃ PlanPhase ⊃ Task), `gates` (PlanPhase→PlanPhase),
  `satisfies` (PlanPhase→Requirement), and Task→Implementation verbs:
  - `creates` — builds a *planned* node (FQN declared in the plan, not in the scanned graph)
  - `modifies` — changes existing code (FQN must resolve in the scanned graph)
  - `deletes` — removes code (FQN must resolve in the scanned graph)
  - `renames` / `moves` — FQN changes
  - further verbs may reveal themselves through dogfooding
- **Feedback is transient** — branch-lifecycle data, never committed; `.trans` mirrors the
  layers structure (feedback sits in the tier dir of its attached node — **all six tiers**:
  requirements/domain/solution/implementation/global, and plan nodes under `.trans/plans`).
  Both halves of the relationship live in `.trans` (see §4.1). Feedback links to
  durable nodes via `Reviews` edges without polluting node files. Review state dies with the
  branch; the reviewed nodes persist.
- Verification items are the plan's test tier (`unit/int/e2e`), not graph content.

## 6. Bootstrap & rollout

- **This change-set is standalone (`apg-projects`)** — the cosanima 1.0 spec is deleted and,
  when it returns, re-materializes on its own branch as its own project.
- **Bootstrap:** this spec is authored with the current binary (v0.10.4) in the old model,
  within its boundaries — it will be re-materialized in the new model later. The new model is
  the 2.0 shape the binary ships toward (the version gate blocks the layout switch).
- The apg repo dogfoods the model: this session is the bootstrap dogfood (branch/worktree
  created manually because `apg project start` doesn't exist yet); from the next feature
  onward the binary handles it.
- **DOGFOODING RULE: the developing binary is NEVER run against this codebase.** Only the
  released binary (`/opt/homebrew/bin/apg`, 0.10.4) runs against this repo's graph until the
  0.11.0 release. The new binary's behavior is exercised via cargo tests (fixture-based) and,
  when end-to-end runs are needed, in a scratch test project (never this repo). The repo's own
  config stays unversioned during development; the version field lands at release on main.
- **Agent flow (operational):** the navigator runs `apg project start <name>` from the main
  checkout; the binary prints the worktree path; the navigator operates with cwd inside the
  worktree. Suite tools work unchanged — walk-up discovery finds the worktree's own `apg/`;
  opencode sessions are rooted at the repo, so the project session/tool workdir points at the
  worktree, not the main checkout.
- Test worktree `apg/.worktrees/test` stays until before shipping (the agent can see the
  verified pattern); cleanup deletes no branch.