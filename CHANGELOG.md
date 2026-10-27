# Changelog

All notable changes to apg are recorded here, newest version first. Versions are
`X.Y.Z`.

A release that changes the durable-spec model adds an **`### Migration`**
subsection to its version: a bullet list of the tasks a layout must perform to
move from the previous model to the new one. `apg init` slices those sections for
the `(old layout version, running binary version]` range and writes them into the
repo's `apg/migration-guide.md`, which the **migration-agent** reads and works.

## v0.27.0

Minor release: `apg scan` now graphs the agent definitions apg distributes, the
plan task record is edge-modelled, planned nodes are realised on every build
path, `apg scan --force` is a true rebuild, and `apg init` installs a ninth
(researcher) agent with a gitignored `apg/.scratch/` workspace.

**Agent frontend and `Agent` nodes.** A new bundled frontend (`agentfrontend`,
`src/agentlib` — stable Rust + `serde`/`serde_json`, nothing needed on `PATH` at
scan time) graphs each definition file under `.opencode/agents/` as exactly one
`Agent` node per file, keyed by the extension-stripped file stem at its
repo-relative path and line span (`agent.<file-stem>`). It joins the scan driver
as the `agent` language id with its own non-fallback `--id-prefix`, ships in the
base `scanner` package beside `structfrontend` (formula, Linux tarball and
installer alike), and is carried through ingest, the DB projection, the
`graph.jsonl` export, the fact cache and the incremental splice fast path; the
structural scanner no longer claims `.opencode/agents/`, so the tree is claimed
exactly once. `Agent` is the identity a plan task's `assigned-to` edge names.

**Edge-modelled plan tasks.** A `Task` now carries only `title` and `status`; its
model is its edges — exactly one `assigned-to` `Agent`, an optional `implements`
`Component`, and one per-kind operation edge per operation
(`creates`/`modifies`/`deletes`/`renames`/`moves`, rendered
`module_`/`file_`/`struct_`/`function_`-prefixed) with `--target` naming a
renames/moves destination. The task-only `kind`/`tier`/`verb`/`target`/`new_fqn`
fields are retired from the wire schema, ingest, projection and export.
`apg plan add`/`update` author the edges, `apg plan render` groups tasks by their
assigned agent and prints one line per operation edge, the plan overview derives
its targets column from them, `apg_plan_tasks` reports an assigned agent missing
from the branch graph as drift, and the verify gate's coverage check reads every
operation-edge destination.

**Planned-node realisation is total.** A planned node is replaced by the real
declaration at its FQN on both build paths — full scan and incremental splice —
for all four implementation kinds, across crate boundaries, with its incident
edges re-pointed and its kind preserved; a node that is still planned survives
both paths unchanged, and the verify gate now judges realisation.

**`apg scan --force`.** A true rebuild: it discards `db.lbug` (with its
`wal`/`shm` sidecars) and `graph.jsonl` before scanning, and bypasses both reuse
paths — the unchanged-tree fast path and the shared fact store's warm cache and
incremental fact reuse — so every frontend re-runs. The store itself and
`apg/layers/**` are left untouched, and without the flag both reuse paths behave
exactly as before. The `apg_scan` suite tool exposes the same flag.

**Ninth agent, research escalation and the scratch surface.** `apg init` now
installs nine distributed agents — the read-only `researcher` joins the roster —
and scaffolds a gitignored `apg/.scratch/` (created by `apg init` and
`apg project start`, its `.gitignore` entry added exactly once) where the
researcher writes its result markdown. Spec-writer, plan-writer, spec-review and
plan-review return a RESEARCH REQUEST when research blocks them; the navigator
triages it and re-dispatches that stage fresh rather than guessing.

**Guards, locking and schema documentation.** Node/edge, plan and review
mutations run their project-context guard before resolving the session, plan
store or target, and every refusal names the current branch and the fix;
`apg init` refuses a target inside a linked worktree; `apg session start`
requires a project context; a scan holds the extended write lock across the whole
pipeline; spec freshness is judged under that lock; and `apg session end`/`abort`
wait for the server to remove its socket. A new normative `SCHEMA.md` states the
edge-kind matrix, the whole-model cardinalities, the tier-4 `Agent` class and the
edge-modelled task record; the scan `Language` vocabulary covers every structural
stream id; an implementation-layer constraint may now carry one `applies-to` edge
per Language it binds (without one it binds all of tier 4), round-tripped through
merge and the export reader; and the `apg spec lint` documentation now states it
blocks R3/R4 (R2 is enforced at write time, not linted).

No durable-spec model change.

## v0.26.0

Minor release: the distributed navigator can now dispatch the `migration-agent`,
and every distributed agent template states the shell rule explicitly.

The navigator prompt gains one `subagent` allow plus the roster/purpose prose at
three body sites, so the eight-agent roster `apg init` installs is fully
dispatchable from the navigator — including `migration-agent` for
`apg/migration-guide.md` work. Every distributed agent template now states the
shell rule: one command per call, and a chained (`&&`, `;`, `|`) or redirected
command is refused; the agent-builder template also states that the
implementer/test-implementer bodies it generates carry that rule, and
`opencode-suite/PERMISSIONS.md` records the whole-value consequence. The spec
records the corresponding requirements: the navigator prompt is a third roster
documentation surface alongside the `apg init` roster and the README, exactly
two frontmatter deltas (the `subagent` allow and the prose) are sanctioned, and
three requirements cover the shell rule and its propagation.

No durable-spec model change.

## v0.25.0

Minor release: filesystem-operation permission scoping now works from a code
writer's worktree. `fsScopeDecision` asserts every fs-operation resource — and
every `save` always-allow pattern — in opencode's main-anchored frame through one
injectable seam, so `apg_rm` / `apg_mv` / `apg_cp` match the code-writer's
worktree globs instead of being refused by the broad `*` deny. A single denial
still refuses the whole operation, and `save` patterns canonicalise their literal
prefix while preserving a glob tail. Adds unit/int/e2e regression coverage, a
real linked-worktree e2e fixture/scenario, and a repaired stale `apg_session`
e2e; `scripts/gate.sh --e2e` now also runs the opencode-suite boundary e2e (the
only agent-grant-reachable entry point). No durable-spec model change.

## v0.24.1

Patch release: the structural scanner no longer graphs the repo's own top-level
`apg/` tool-owned layout. The layout is pruned in the discovery walk, excluded
from the structural scope, and ingest drops any residual records, so a self-scan
stops treating the tool's own state as project source. Adds structlib e2e and
ingest unit coverage. No durable-spec model change.

## v0.24.0

Re-worked the durable-spec spine direction and its cardinalities. The `drives`
edge (Requirement → Service) is replaced by `driven-by` (Service → Requirement),
so spine edges point from the concrete tier toward the abstract one;
`driven-by` must now reach both ends (every Service is driven by at least one
Requirement, and every Requirement is driven by at least one Service). A
Component may now claim many code nodes, but each code node is claimed by
exactly one Component. Every Service must be realised by at least one Component;
every `role` requires `applies-to` (`container`|`component`|`both`), must be
`is-role`-ed by at least one Container/Component, and that user's level must
match; and a Person's `uses` Component must realise a Service the Actor it
represents reaches through `calls`. Group `attribute`/`root` and Container
`kind` are now unvalidated metadata, requiring no layout change.

### Migration

- **`drives` becomes `driven-by` (renamed and reversed).** Rewrite every
  `drives` node-file edge (Requirement → Service) as `driven-by`
  (Service → Requirement): rename the edge kind and swap `from`/`to`. `drives`
  is no longer an accepted edge kind.
- **Every Requirement needs a `driven-by` Service.** A Requirement with no
  Service driving it fails `save`/`scan` (requirement coverage).
- **Every Service needs a `realises` Component.** Point at least one
  `solution.component` at each realised Service.
- **Every `solution.role` needs `applies-to`.** Set it to
  `container`|`component`|`both`; each Role must be `is-role`-ed by at least one
  Container/Component, and the value must match that user's level.
- **Each code node has exactly one `implemented-by` Component.** A Component may
  claim many code nodes, but a code node claimed by two Components must be
  reduced to a single claimant.
- **Person → Actor → Service coherence.** Where a Person `uses` a Component, the
  Actor it represents must `call` — directly or through `calls` — a Service that
  Component realises.

## v0.23.2

Re-cut of v0.23.1 (same fix) with correctly-versioned bottles; the v0.23.1 bottles
were accidentally built at v0.23.0. No durable-spec model change.

## v0.23.1

Patch release: `apg init` writes the migration guide only when the layout version
actually advances, so a same-version re-init no longer overwrites a still-pending
migration guide with "no migration required". No durable-spec model change.

## v0.23.0

Re-worked the durable-spec model: the domain → solution spine runs through
`realises` (Component → Service), constraints are tier-scoped, and an event is an
`Entity` kind rather than a type of its own. A layout written by an earlier apg
does not satisfy the new cardinalities and must be migrated before `apg scan`
accepts it.

### Migration

- **`realised-by` becomes `realises` (reversed direction).** The old
  `realised-by` edge (Domain Group/Entity/Service → Solution
  System/Container/Component) is gone; the new edge is `realises`, directed
  **Component → Service**. For each realised Service, invent a
  `solution.component`, invent the `solution.container` and `solution.system`
  that contain it, and re-point every `implemented-by` edge that hung off the old
  Group/Entity/Service target onto that Component.
- **`depends-on` is removed.** A Requirement's dependency on another Requirement
  becomes `contains` decomposition (a Requirement contained by the Requirement it
  depended on).
- **`drives` targets `Service` only.** A `drives` edge whose target is a Group,
  Entity, or Value must be re-pointed at the Service that owns or represents that
  node.
- **`represents` is re-shaped; invent `domain.actor`.** `User —represents→ Entity`
  becomes `Actor —represents→ User` (invent the `domain.actor`); `Entity
  —represents→ Person` becomes `Person —represents→ Actor`.
- **`Person uses System` becomes `Person uses Component`.**
- **`Group contains Value` becomes `Entity contains Value`.**
- **An event is an `Entity`.** A separate event type becomes an `Entity` with
  `kind = event`.
- **Constraints are tier-scoped.** An `attaches-to` constraint becomes a
  tier-scoped constraint: the constraint's **layer is its scope**
  (`requirements`/`domain`/`solution`/`implementation`/`global`), `attaches-to`
  is gone, and a constraint names no node.
- **New node types `domain.actor` and `solution.role`; new edges `requires`,
  `reads`, `writes`, `is-role`, and `realises`.**
- **The whole model must satisfy the new cardinalities** before `save`/`scan`
  accepts it — for example a leaf Group needs at least one Service, an Entity
  needs at least one Value, and a Component needs exactly one `is-role` Role and
  at least one `realises`.

## v0.22.2

Patch release: no durable-spec model change.
