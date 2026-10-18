# migration-agent — changelog-driven migration guide + generic migration agent

Plain-code change-set (no APG tooling, same discipline as schema-rework).
Ships with the 0.23 release; essential, since the schema-rework change makes the
apg repo's own durable spec un-ingestible until it is migrated.

## Goal

A **generic migration agent** reads a **migration guide** that `apg init` writes
into the repo's `apg/` dir, and executes (or reports) the migration tasks needed
to move the layout from the version it was at to the version it is now at. The
migration knowledge is **data** (a `### Migration` section per version in the
changelog), not a bespoke agent per schema change.

## Design (the flow)

```
CHANGELOG.md  (repo root, committed, embedded via include_str!)
   ## v0.23.0
   ... release notes ...
   ### Migration
   - realised-by → realises  (Component→Service, invent Component + Container/System)
   ...

apg init (cmd_init in src/install.rs)
   old = version_gate::config_version(&apg_dir)   // BEFORE ensure_config_version overwrites it
   new = env!("CARGO_PKG_VERSION")
   slice = changelog entries with version ∈ (old, new]      // semver ordering
   write apg/migration-guide.md = concat of those entries' ### Migration sections
      (no entries / no Migration sections → write "no migration required")

migration-agent  (opencode-suite/agents/migration-agent.md, installed by apg init)
   reads apg/migration-guide.md → reports the tasks (and, where it can, executes them)
```

## Pieces

### 1. `CHANGELOG.md` (new) + the `### Migration` convention

- New committed `CHANGELOG.md` at the repo root, markdown.
- Convention: `## vX.Y.Z` headers (strictly) + an optional `### Migration`
  subsection per version, holding a bullet list of migration tasks.
- `apg init` parses it by convention: split on `## vX.Y.Z`, order versions
  semver, select the `(old, new]` range, take each selected entry's
  `### Migration` content.
- Embedded via `include_str!("../CHANGELOG.md")` (resolved from `src/`, matching
  the existing `opencode-suite/` include pattern).

### 2. The slicer (in `src/install.rs`, called from `cmd_init`)

- `cmd_init` reads the old version via `version_gate::config_version(&apg_dir)`
  **before** `ensure_config_version` rewrites it; captures it into a local.
- New `fn write_migration_guide(apg_dir, old_version)` (or similar):
  - slice the embedded changelog for `(old, new]` using the existing
    `version_gate::parse_major_minor` (extended to full semver for ordering),
  - concatenate the selected `### Migration` sections in version order, with a
    small header naming the from→to range,
  - write to `apg/migration-guide.md` (or write "no migration required").
- Idempotent: re-running `apg init` at the same version rewrites the same guide
  (no-op).

### 3. `opencode-suite/agents/migration-agent.md` (new distributed agent)

- Generic: "read `apg/migration-guide.md` in the repo's `apg/` dir; if it says
  no migration is required, report that and stop; otherwise work the listed
  tasks."
- Registered in `install.rs`'s `AGENTS` list (so `apg init` installs it like the
  other seven); add it to the `KNOWN_AGENT_FILES` prune set.
- Its "specialist subject" is supplied *by the guide* (the data), not baked into
  the agent — so the next schema change only ships a new `### Migration` section.

### 4. The `0.22 → 0.23` Migration section (the first real payload)

Content = the schema-rework §10 mapping, phrased as actionable tasks:
- `realised-by` (Domain Group/Entity/Service → Solution System/Container/Component)
  → `realises` (Component → Service), inventing a Component + its Container/System
  containment; re-point `implemented-by` onto the Component.
- `depends-on` (Requirement→Requirement) → `contains` decomposition.
- `drives` → Service only (a Group/Entity/Value target becomes the owning/representing Service).
- `represents` (User→Entity, Entity→Person) → Actor —represents→ User, Person —represents→ Actor (invent the Actor).
- `Person uses System` → `Person uses Component`.
- `Group contains Value` → `Entity contains Value`.
- event as a separate type → Entity `kind=event`.
- `attaches-to` constraints → tier-scoped constraints (layer is the scope).
- Note the whole-model save boundary (§8.1): the migrated model must satisfy the
  new cardinalities before `save`/`scan` accept it.

## Design decisions (pinned)

- **Markdown** changelog (human-first); `apg init` slices by `## vX.Y.Z` convention.
- **Slice range** `(old, new]`, full-semver ordering; concatenate in order so a
  skipped release composes its intermediate sections.
- **Guide location** `apg/migration-guide.md` in the `apg/` dir (committed, like
  `config.json`); written by `apg init`, read by the agent.
- **No-op** when the range has no `### Migration` sections.

## Resolved: the bootstrap is a documented hand-edit exception

**Decision (resolved).** Migration **is** a documented exception to "never
hand-edit": the new binary refuses to ingest the old spec, so the agent's
execute path cannot go through the normal authoring flow (`apg node`/`apg edge`
+ session) — that flow is itself refused while the old spec is on disk, and the
whole-model save boundary (§8.1) would reject every intermediate state. The
migration-agent therefore holds a **scoped `apg/layers/**` edit grant** and
rewrites the node files directly into the new format the guide specifies, then
`apg scan` (and `apg spec lint`) validate.

Concretely, the agent's grants are:

- **write** — `apg/layers/**` only (the one exception), and **no** commit grant.
- **no `apg_node`/`apg_edge`/`apg_session`** — those are refused during the
  bootstrap and would mislead; the mechanism is direct file rewrite.
- **read/glob/grep** — `apg/layers/**` included (the node files are the source
  of truth during migration, since the graph index is stale); `apg/.trans/**`
  still denied.
- **validate** — `apg_scan` + `apg_spec_lint`, plus the read/query tools to
  cross-check code FQNs.

The human reviews the agent's `apg/layers/**` diff and commits — the one-time
irreversible rewrite is human-gated, while the agent absorbs the mechanical,
error-prone part.

## Phases

1. **Changelog + convention** — add `CHANGELOG.md` with the `### Migration`
   convention and the `0.22→0.23` section; embed it.
2. **The slicer** — `write_migration_guide` in `install.rs`, wired into
   `cmd_init` before the version overwrite; unit + e2e (scratch repo: init at
   two versions, assert the guide contents, incl. the no-op case).
3. **The agent** — `opencode-suite/agents/migration-agent.md` + register in
   `install.rs` `AGENTS` + `KNOWN_AGENT_FILES`.
4. **Gate + review** — `scripts/gate.sh --e2e`; implementation-phase review.

## Out of scope

- Actually executing the 0.22→0.23 migration of the apg repo's `apg/layers/**`
  (a post-release activity done *by* the migration agent using the guide).
- README/AGENTS doc updates (post-release, as in schema-rework).
