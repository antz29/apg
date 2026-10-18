# Changelog

All notable changes to apg are recorded here, newest version first. Versions are
`X.Y.Z`.

A release that changes the durable-spec model adds an **`### Migration`**
subsection to its version: a bullet list of the tasks a layout must perform to
move from the previous model to the new one. `apg init` slices those sections for
the `(old layout version, running binary version]` range and writes them into the
repo's `apg/migration-guide.md`, which the **migration-agent** reads and works.

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
