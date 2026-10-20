# apg migration guide

Written by `apg init`; read and worked by the **migration-agent**, which finds it here in the repo's `apg/` dir.

The layout was at apg 0.22.2; this binary is apg 0.24.0. Run the **migration-agent** to work the tasks below, then re-run `apg scan`.

## Migration: 0.22.2 → 0.24.0

### v0.23.0

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
