# Changelog

All notable changes to apg are recorded here, newest version first. Versions are
`X.Y.Z`.

A release that changes the durable-spec model adds an **`### Migration`**
subsection to its version: a bullet list of the tasks a layout must perform to
move from the previous model to the new one. `apg init` slices those sections for
the `(old layout version, running binary version]` range and writes them into the
repo's `apg/migration-guide.md`, which the **migration-agent** reads and works.

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
