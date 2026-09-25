//! The layer catalog and tree layout of the node-file model (apg-projects
//! SPEC §3.1/§4.1/§5): the six logical layers — requirements, domain,
//! solution, plans, implementation, global — the node types each layer may
//! hold, and where each layer's node files live.
//!
//! Storage policy is separate from the catalog: requirements, domain,
//! solution, and global serialize durably under `apg/layers/`; plans
//! serialize **only** under the gitignored `apg/.trans/plans/` (transient,
//! per branch); implementation's real nodes ARE the scanned code (never
//! serialized) and only its attach-only note/constraint files exist durably
//! under `apg/layers/implementation/`. `apg/.trans/` mirrors every layer dir
//! (all six tiers, incl. global): feedback lands in the tier dir of its
//! attached node; plan nodes live under `.trans/plans/`.
//!
//! This module ships the catalog + layout as **data/constants**, plus the
//! SPEC §3.3 node-rule validation ([`validate_node`], [`validate_trees_acyclic`]).
//! The write/ingest and path/identity helpers are later phase-3/4 tasks. Every
//! layout constant is a
//! directory name relative to the layout root (the `apg/` dir — `specs::LAYOUT`
//! from the repo root); later consumers join them onto the root they resolve:
//!
//! ```text
//! apg/layers/                         (durable — one file per node)
//!   requirements/{stakeholder,user,requirement,note,constraint}/
//!   domain/{group,entity,value,service,note,constraint}/
//!   solution/{system,container,component,person,note,constraint}/
//!   implementation/{note,constraint}/ (attach-only — the real nodes are scanned code)
//!   global/{constraint,note}/
//! apg/.trans/                         (gitignored — mirrors the structure)
//!   plans/                            (the plan, per branch; tier dir of plan nodes)
//!   requirements/ domain/ solution/ implementation/ global/   (feedback mirrors)
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifacts;
use crate::git;
use crate::schema::Record;

/// The durable node-file root: `layers/` under the layout root (SPEC §4.1) —
/// one file per node at `<layer>/<type>/<name>.json`, the file name IS the
/// identity.
// (Unused until the phase-3 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const LAYERS_DIR: &str = "layers";

/// The gitignored transient root: `.trans/` under the layout root (SPEC
/// §4.1/§5) — the per-branch plan store plus the transient mirrors of the
/// layer tree.
// (Unused until the phase-3 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const TRANS_DIR: &str = ".trans";

/// Where a layer's authored nodes serialize (SPEC §3.1, storage policy —
/// separate from the catalog itself).
// (Unused until the phase-3 write/ingest tasks consult the catalog.)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePolicy {
    /// Node files under the committed `apg/layers/<layer>/` tree — durable;
    /// a file's present-ness on a branch is the node's present-ness.
    Durable,
    /// Only under the gitignored `apg/.trans/plans/` — transient, per
    /// branch, never committed. The plans layer's whole serialization.
    TransientPlans,
    /// The layer's real nodes are the scanned code — never serialized; only
    /// its attach-only node types (note, constraint) have durable files under
    /// `apg/layers/<layer>/`.
    ScannedCode,
}

/// The six logical layers of the catalog (SPEC §3.1). The catalog is
/// layer-scoped; [`Layer::storage`] carries the storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// Tier 1 — the stakeholder/requirement hierarchy (why).
    Requirements,
    /// Tier 2 — DDD semantics with plain names (what).
    Domain,
    /// Tier 3 — C4 only (how).
    Solution,
    /// Tier 4 — the bridge: plan phases, tasks, planned Implementation nodes.
    Plans,
    /// Tier 5 — the code in the branch (scanned, never serialized).
    Implementation,
    /// The laws (constraints over the whole graph) and notes on them.
    Global,
}

// (Unused until the phase-3/4 write/ingest tasks consult the catalog.)
#[allow(dead_code)]
impl Layer {
    /// All six layers in catalog order: requirements, domain, solution,
    /// plans, implementation, global.
    pub const ALL: [Layer; 6] = [
        Layer::Requirements,
        Layer::Domain,
        Layer::Solution,
        Layer::Plans,
        Layer::Implementation,
        Layer::Global,
    ];

    /// The layer's directory name — the dir under `apg/layers/` for the
    /// file-backed layers, under `apg/.trans/` for plans; also the name of
    /// the layer's `.trans` mirror tier dir.
    pub const fn layer_dir(self) -> &'static str {
        match self {
            Layer::Requirements => "requirements",
            Layer::Domain => "domain",
            Layer::Solution => "solution",
            Layer::Plans => "plans",
            Layer::Implementation => "implementation",
            Layer::Global => "global",
        }
    }

    /// The node types the layer may hold (SPEC §3.1 table): lowercase type
    /// strings — the per-type dir names under the layer and the node-file
    /// schema's `type` field. Plans: `plan-phase` and `task`, plus the four
    /// code kinds (`module`/`file`/`struct`/`function`) a **planned
    /// Implementation node** can declare (schema.rs `planned_node`; they live
    /// as records under `.trans/plans/`, never as node files).
    /// Implementation: only the attach-only `note`/`constraint` files — the
    /// layer's real nodes are the scanned code.
    pub const fn node_types(self) -> &'static [&'static str] {
        match self {
            Layer::Requirements => &["stakeholder", "user", "requirement", "note", "constraint"],
            Layer::Domain => &["group", "entity", "value", "service", "note", "constraint"],
            Layer::Solution => &[
                "system",
                "container",
                "component",
                "person",
                "note",
                "constraint",
            ],
            Layer::Plans => &["plan-phase", "task", "module", "file", "struct", "function"],
            Layer::Implementation => &["note", "constraint"],
            Layer::Global => &["constraint", "note"],
        }
    }

    /// Where the layer's authored nodes serialize (SPEC §3.1): the four
    /// file-backed layers are [`StoragePolicy::Durable`]; plans is
    /// [`StoragePolicy::TransientPlans`] (`.trans/plans/` only, per branch);
    /// implementation is [`StoragePolicy::ScannedCode`] — scanned code, never
    /// serialized, only its attach-only types have durable files.
    pub const fn storage(self) -> StoragePolicy {
        match self {
            Layer::Requirements | Layer::Domain | Layer::Solution | Layer::Global => {
                StoragePolicy::Durable
            }
            Layer::Plans => StoragePolicy::TransientPlans,
            Layer::Implementation => StoragePolicy::ScannedCode,
        }
    }
}

/// The durable `apg/layers/` tree (SPEC §4.1): the file-backed layer dirs in
/// layout order — requirements, domain, solution, implementation, global
/// (plans has no durable dir) — each with its per-type node dirs.
// (Unused until the phase-3 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const LAYERS_TREE: &[(&str, &[&str])] = &[
    (
        "requirements",
        &["stakeholder", "user", "requirement", "note", "constraint"],
    ),
    (
        "domain",
        &["group", "entity", "value", "service", "note", "constraint"],
    ),
    (
        "solution",
        &[
            "system",
            "container",
            "component",
            "person",
            "note",
            "constraint",
        ],
    ),
    ("implementation", &["note", "constraint"]),
    ("global", &["constraint", "note"]),
];

/// The `.trans` tree (SPEC §4.1/§5): every layer dir under the transient
/// root — `plans/` first (the per-branch plan store; also the tier dir of
/// plan nodes), then the five feedback mirrors in the tier dir of the
/// attached node. All six tiers, incl. implementation and global.
// (Unused until the phase-3/4 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const TRANS_MIRRORS: [Layer; 6] = [
    Layer::Plans,
    Layer::Requirements,
    Layer::Domain,
    Layer::Solution,
    Layer::Implementation,
    Layer::Global,
];

// ---------------------------------------------------------------------------
// SPEC §3.3 node rules — write-time validation (phase-3 task-2)
// ---------------------------------------------------------------------------

/// Property keys the tier model validates (SPEC §3.3). A key is validated
/// only for the types that take it; any other key — known or not — is
/// metadata (SPEC §4.1: short ids "may exist as metadata only") and is never
/// refused, never treated as identity.
pub const PROP_KIND: &str = "kind";
pub const PROP_ATTRIBUTE: &str = "attribute";
pub const PROP_ROOT: &str = "root";

/// `Entity` `kind` values (SPEC §3.1: "Entity — kind `entity` | `event`").
pub const ENTITY_KINDS: [&str; 2] = ["entity", "event"];
/// `Group` `attribute` values (SPEC §3.1: "attributes
/// `core`/`supporting`/`generic`").
pub const GROUP_ATTRIBUTES: [&str; 3] = ["core", "supporting", "generic"];
/// `Container` `kind` values (SPEC §3.1: "kind `app`/`service`/`db`/`queue`").
pub const CONTAINER_KINDS: [&str; 4] = ["app", "service", "db", "queue"];

/// The node-file schema's properties map (SPEC §4.1). Known keys
/// (kind/attribute/root) are validated per type by [`validate_node`]; unknown
/// keys are metadata — allowed, never identity.
pub type NodeProperties = BTreeMap<String, String>;

/// The SPEC §3.3 name allowlist `[a-z0-9][a-z0-9-]*` — refuse, never
/// sanitize. Lowercase ASCII letters and digits anywhere, hyphens after the
/// first character; a leading digit and a trailing hyphen are fine; the empty
/// name is not.
pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate one proposed node against the SPEC §3.3 node rules before it is
/// written — write-time enforcement. Pure: no I/O; `existing` is the current
/// (layer, type, name) identity universe, used only for the uniqueness rule.
///
/// Enforced (the binary can check these):
/// - the name matches the allowlist `[a-z0-9][a-z0-9-]*` — refused, never
///   sanitized;
/// - the type exists in its layer — case-sensitive exact match against the
///   catalog's lowercase type spellings ([`Layer::node_types`]);
/// - kind attributes when the property is present: `Entity` **requires**
///   `kind` ∈ {entity, event} (the spec's only "requires"); `Group` takes
///   `attribute` ∈ {core, supporting, generic} and an optional `root`
///   (format-validated against the name allowlist, independent of the
///   attribute); `Container` takes `kind` ∈ {app, service, db, queue};
/// - names unique per (layer, type).
///
/// Not enforced (prose/agent guidance the binary cannot check): requirement
/// decomposition atomicity, "Value immutable", "Service stateless", the
/// Stakeholder/User and Person (C4 view) semantics. Tree acyclicity for
/// contains/depends-on edges is [`validate_trees_acyclic`].
// (Unused until write_project, phase-3 task-16, calls it.)
#[allow(dead_code)]
pub fn validate_node(
    layer: Layer,
    node_type: &str,
    name: &str,
    properties: &NodeProperties,
    existing: &BTreeSet<(Layer, String, String)>,
) -> anyhow::Result<()> {
    if !valid_name(name) {
        anyhow::bail!(
            "node name `{name}` is invalid — the name allowlist is [a-z0-9][a-z0-9-]* (refused, never sanitized)"
        );
    }
    let types = layer.node_types();
    if !types.contains(&node_type) {
        anyhow::bail!(
            "type `{node_type}` does not exist in layer `{}` — allowed types: {}",
            layer.layer_dir(),
            types.join(", ")
        );
    }
    match node_type {
        "entity" => match properties.get(PROP_KIND) {
            Some(kind) if ENTITY_KINDS.contains(&kind.as_str()) => {}
            Some(kind) => anyhow::bail!(
                "Entity `kind` `{kind}` is invalid — expected {}",
                ENTITY_KINDS.join("|")
            ),
            None => anyhow::bail!("Entity requires `kind` ({})", ENTITY_KINDS.join("|")),
        },
        "group" => {
            if let Some(attribute) = properties.get(PROP_ATTRIBUTE)
                && !GROUP_ATTRIBUTES.contains(&attribute.as_str())
            {
                anyhow::bail!(
                    "Group `attribute` `{attribute}` is invalid — expected {}",
                    GROUP_ATTRIBUTES.join("|")
                );
            }
            if let Some(root) = properties.get(PROP_ROOT)
                && !valid_name(root)
            {
                anyhow::bail!(
                    "Group `root` `{root}` is invalid — must match the name allowlist [a-z0-9][a-z0-9-]*"
                );
            }
        }
        "container" => {
            if let Some(kind) = properties.get(PROP_KIND)
                && !CONTAINER_KINDS.contains(&kind.as_str())
            {
                anyhow::bail!(
                    "Container `kind` `{kind}` is invalid — expected {}",
                    CONTAINER_KINDS.join("|")
                );
            }
        }
        _ => {}
    }
    if existing.contains(&(layer, node_type.to_string(), name.to_string())) {
        anyhow::bail!(
            "duplicate node `{}.{node_type}.{name}` — names are unique per (layer, type)",
            layer.layer_dir()
        );
    }
    Ok(())
}

/// Parse an authored-node FQN (`<layer>.<type>.<name>`) back into its
/// (layer, type, name) identity. Used only to resolve edge endpoints against
/// the change universe; the FQN *builder* is [`fqn`].
pub fn parse_fqn(fqn: &str) -> anyhow::Result<(Layer, String, String)> {
    let mut parts = fqn.split('.');
    let (layer_s, node_type, name) = match (parts.next(), parts.next(), parts.next(), parts.next())
    {
        (Some(l), Some(t), Some(n), None) => (l, t, n),
        _ => anyhow::bail!("`{fqn}` is not a node FQN of the form <layer>.<type>.<name>"),
    };
    let Some(layer) = Layer::ALL
        .iter()
        .find(|l| l.layer_dir() == layer_s)
        .copied()
    else {
        anyhow::bail!("`{fqn}` names unknown layer `{layer_s}`");
    };
    Ok((layer, node_type.to_string(), name.to_string()))
}

/// Refuse a cycle in a contains/depends-on edge set — the SPEC §3.3 node rule
/// "contains/depends-on trees acyclic". `edges` is the per-change set of
/// (source FQN, target FQN) pairs — a proposed node's `out` list. `existing`
/// is the (layer, type, name) identity universe **of the change**: it must
/// contain every node of the change (existing nodes plus the node being
/// validated and any co-proposed nodes), so a dangling endpoint is refused
/// here too. Pure structure check — the edge-kind matrix is
/// [`validate_edges`]'s job (phase-3 task-4).
///
/// Run over the assembled post-mutation edge set by
/// [`validate_assembled_rules`]: the write path (`validate_change`, behind
/// [`write_project`]) and the scan path ([`ingest_tree`]) both enforce it.
pub fn validate_trees_acyclic(
    edges: &[(&str, &str)],
    existing: &BTreeSet<(Layer, String, String)>,
) -> anyhow::Result<()> {
    // Dangling FQN references are write-time errors (SPEC §3.3).
    for &(src, dst) in edges {
        let src_node = parse_fqn(src)?;
        if !existing.contains(&src_node) {
            anyhow::bail!("edge endpoint `{src}` is not a node of the change");
        }
        let dst_node = parse_fqn(dst)?;
        if !existing.contains(&dst_node) {
            anyhow::bail!("edge endpoint `{dst}` is not a node of the change");
        }
    }
    // Directed graph of the edge set; a three-color DFS refuses any cycle (a
    // contains/depends-on tree must stay acyclic — a self-loop is a cycle).
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(src, dst) in edges {
        out.entry(src).or_default().push(dst);
    }
    const UNVISITED: u8 = 0;
    const ON_STACK: u8 = 1;
    const DONE: u8 = 2;
    fn visit<'a>(
        node: &'a str,
        out: &BTreeMap<&'a str, Vec<&'a str>>,
        state: &mut BTreeMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> anyhow::Result<()> {
        state.insert(node, ON_STACK);
        stack.push(node);
        if let Some(nexts) = out.get(node) {
            for &next in nexts {
                match state.get(next).copied().unwrap_or(UNVISITED) {
                    ON_STACK => {
                        let start = stack.iter().position(|&n| n == next).unwrap();
                        let cycle: Vec<&str> =
                            stack[start..].iter().copied().chain([next]).collect();
                        anyhow::bail!("contains/depends-on cycle: {}", cycle.join(" -> "));
                    }
                    UNVISITED => visit(next, out, state, stack)?,
                    DONE => {}
                    _ => unreachable!(),
                }
            }
        }
        stack.pop();
        state.insert(node, DONE);
        Ok(())
    }
    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    let mut stack: Vec<&str> = Vec::new();
    for node in out.keys() {
        if state.get(node).copied().unwrap_or(UNVISITED) == UNVISITED {
            visit(node, &out, &mut state, &mut stack)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SPEC §3.3 edge-kind matrix — write-time validation (phase-3 task-4)
// ---------------------------------------------------------------------------

/// The SPEC §3.3 durable edge kinds — the complete list. Plan edges
/// (`contains` plan tiers, `gates`, `satisfies`, the Task→Implementation
/// verbs) and `reviews` are §5 **transient** kinds — not here (phase 4). The
/// matrix rows cover the nine two-authored-endpoint kinds; `implemented-by`
/// (code-FQN target) and `details` (any-node target) have a code-exempt
/// endpoint and are special-cased in [`validate_edge`].
const EDGE_KINDS: [&str; 11] = [
    "contains",
    "drives",
    "realised-by",
    "implemented-by",
    "calls",
    "publishes",
    "subscribes",
    "depends-on",
    "uses",
    "represents",
    "details",
];

/// One SPEC §3.3 matrix row: `(kind, source-layer, source-types,
/// target-layer, target-types)`. Each endpoint is a (layer, type) pair: a type
/// maps to a unique layer except `note`/`constraint` (which span the authoring
/// layers — but neither is a matrix endpoint, so the layer+type granularity
/// here is unambiguous). Source/target type sets are subsets of
/// [`Layer::node_types`].
type MatrixRow = (
    &'static str,
    Layer,
    &'static [&'static str],
    Layer,
    &'static [&'static str],
);

/// The SPEC §3.3 edge-kind matrix — only the two-authored-endpoint kinds are
/// listed; `implemented-by` and `details` are special-cased in
/// [`validate_edge`].
const MATRIX: &[MatrixRow] = &[
    (
        "contains",
        Layer::Requirements,
        &["stakeholder", "user", "requirement"],
        Layer::Requirements,
        &["requirement"],
    ),
    (
        "contains",
        Layer::Domain,
        &["group"],
        Layer::Domain,
        &["group", "entity", "value", "service"],
    ),
    (
        "contains",
        Layer::Solution,
        &["system"],
        Layer::Solution,
        &["container"],
    ),
    (
        "contains",
        Layer::Solution,
        &["container"],
        Layer::Solution,
        &["component"],
    ),
    (
        "drives",
        Layer::Requirements,
        &["requirement"],
        Layer::Domain,
        &["group", "entity", "value", "service"],
    ),
    (
        "realised-by",
        Layer::Domain,
        &["group", "entity", "service"],
        Layer::Solution,
        &["system", "container", "component"],
    ),
    (
        "calls",
        Layer::Domain,
        &["service"],
        Layer::Domain,
        &["service"],
    ),
    (
        "publishes",
        Layer::Domain,
        &["service"],
        Layer::Domain,
        &["entity"],
    ),
    (
        "subscribes",
        Layer::Domain,
        &["service"],
        Layer::Domain,
        &["entity"],
    ),
    (
        "depends-on",
        Layer::Requirements,
        &["requirement"],
        Layer::Requirements,
        &["requirement"],
    ),
    (
        "uses",
        Layer::Solution,
        &["person"],
        Layer::Solution,
        &["system"],
    ),
    (
        "represents",
        Layer::Requirements,
        &["user"],
        Layer::Domain,
        &["entity"],
    ),
    (
        "represents",
        Layer::Domain,
        &["entity"],
        Layer::Solution,
        &["person"],
    ),
];

/// Format one kind's allowed source/target shapes from the matrix rows, e.g.
/// `requirements.requirement -> domain.group|entity|value|service`.
fn allowed_shapes(kind: &str) -> String {
    MATRIX
        .iter()
        .filter(|(k, ..)| *k == kind)
        .map(|(_, sl, sts, tl, tts)| {
            format!(
                "{}.{} -> {}.{}",
                sl.layer_dir(),
                sts.join("|"),
                tl.layer_dir(),
                tts.join("|")
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Validate a set of proposed edges against the SPEC §3.3 edge-kind matrix —
/// write-time enforcement, pure (no I/O; the FQN string itself encodes
/// `layer.type.name`, so the endpoint (layer, type) is derived by parsing, no
/// store needed). Each tuple is `(kind, source FQN, target FQN)`; the batch
/// loops [`validate_edge`] over every edge.
///
/// This is also the sequential-spine lint (§3.2): every spine hop is
/// tier-locked by the matrix (`drives`: Requirement → Domain; `realised-by`:
/// Domain → Solution; `implemented-by`: Solution → code), so a tier skip is
/// impossible once the matrix holds — the matrix IS the spine enforcement, and
/// there is no extra tier machinery. A dangling FQN endpoint (malformed FQN or
/// unknown layer) is a write-time error (SPEC §3.3).
// (Unused until write_project, phase-3 task-16, calls it.)
#[allow(dead_code)]
pub fn validate_edges(edges: &[(&str, &str, &str)]) -> anyhow::Result<()> {
    for &(kind, source, target) in edges {
        validate_edge(kind, source, target)?;
    }
    Ok(())
}

/// Validate one `(kind, source, target)` edge against the SPEC §3.3 matrix.
/// The source of every §3.3 edge is an authored node — parsed first, so a
/// malformed source FQN or unknown layer is a dangling-reference write-time
/// error for every kind. `implemented-by` (code-FQN target) and `details`
/// (any-node target) have a code-exempt **target**: only their source is
/// validated here; code-FQN validation against the scanned graph is task-10
/// ([`validate_code_refs`]). For every other kind, both endpoints must parse
/// as authored-node FQNs and the (layer, type) shape must match a matrix row.
fn validate_edge(kind: &str, source: &str, target: &str) -> anyhow::Result<()> {
    let (src_layer, src_type, _src_name) = parse_fqn(source)?;

    match kind {
        // implemented-by: System/Container/Component → code FQN. The target is
        // exempt — validated against the scanned graph by validate_code_refs.
        "implemented-by" => {
            if src_layer != Layer::Solution
                || !["system", "container", "component"].contains(&src_type.as_str())
            {
                anyhow::bail!(
                    "`implemented-by` source `{source}` must be a Solution System/Container/Component, not {}.{src_type}",
                    src_layer.layer_dir()
                );
            }
            Ok(())
        }
        // details: Note (any layer) → any node, authored OR code. The target is
        // exempt; only the source's note-ness is checked.
        "details" => {
            if src_type.as_str() != "note" {
                anyhow::bail!(
                    "`details` source `{source}` must be a `note` (any layer), not {}.{src_type}",
                    src_layer.layer_dir()
                );
            }
            Ok(())
        }
        // The matrix kinds: both endpoints parse, and the (layer, type) shape
        // must match a §3.3 row.
        _ => {
            if !EDGE_KINDS.contains(&kind) {
                anyhow::bail!(
                    "unknown edge kind `{kind}` — durable edge kinds are {} (plan/feedback edges are §5 transient, not here)",
                    EDGE_KINDS.join(", ")
                );
            }
            let (dst_layer, dst_type, _dst_name) = parse_fqn(target)?;
            let ok = MATRIX.iter().any(|(k, sl, sts, tl, tts)| {
                *k == kind
                    && *sl == src_layer
                    && sts.contains(&src_type.as_str())
                    && *tl == dst_layer
                    && tts.contains(&dst_type.as_str())
            });
            if !ok {
                anyhow::bail!(
                    "invalid `{kind}` edge `{source}` -> `{target}` — allowed shapes: {}",
                    allowed_shapes(kind)
                );
            }
            Ok(())
        }
    }
}

/// The property half of the SPEC §3.3 `publishes`/`subscribes` matrix rows:
/// those edges target an `Entity` with `kind: event` (a published/subscribed
/// event), not a plain entity. [`validate_edge`] is type-only — it never sees
/// a node's properties — so the caller supplies the assembled post-mutation
/// node set as FQN → node file and this function resolves each target against
/// it. A target absent from `assembled` is left to the dangling-reference
/// checks ([`validate_edges`] parses matrix endpoints; [`check_edge_pairing`]
/// resolves authored endpoints): this function only refuses a present target
/// that is not an event.
fn validate_event_targets(
    edges: &[(&str, &str, &str)],
    assembled: &BTreeMap<String, &NodeFile>,
) -> anyhow::Result<()> {
    for &(kind, _source, target) in edges {
        if !matches!(kind, "publishes" | "subscribes") {
            continue;
        }
        let Some(node) = assembled.get(target) else {
            continue;
        };
        if node.node_type != "entity"
            || node.properties.get(PROP_KIND).map(String::as_str) != Some("event")
        {
            anyhow::bail!(
                "`{kind}` target `{target}` must be an Entity with kind `event` — the §3.3 matrix qualifies publishes/subscribes targets as Entity (kind: event)"
            );
        }
    }
    Ok(())
}

/// The assembled-set property rules of SPEC §3.3 — the rules the edge-kind
/// matrix cannot express (it never sees properties, and a cycle only exists
/// across edges):
///
/// - contains/depends-on trees are acyclic ([`validate_trees_acyclic`],
///   including its dangling-endpoint refusal);
/// - `publishes`/`subscribes` targets are `Entity (kind: event)`
///   ([`validate_event_targets`]).
///
/// `assembled` maps every post-mutation FQN to its node file (a write
/// overrides the current file); `universe` is the same set's (layer, type,
/// name) identity universe, which [`validate_trees_acyclic`] resolves
/// endpoints against. Pure — no I/O.
///
/// Both the write path (`validate_change`, behind [`write_project`]) and the
/// scan path ([`ingest_tree`]) run this over their assembled post-mutation
/// node set, so neither can write or ingest a violation.
fn validate_assembled_rules(
    assembled: &BTreeMap<String, &NodeFile>,
    universe: &BTreeSet<(Layer, String, String)>,
) -> anyhow::Result<()> {
    let mut tree_edges: Vec<(String, String)> = Vec::new();
    let mut event_edges: Vec<(String, String, String)> = Vec::new();
    for (from, n) in assembled {
        for oe in &n.out {
            match oe.kind.as_str() {
                "contains" | "depends-on" => tree_edges.push((from.clone(), oe.target.clone())),
                "publishes" | "subscribes" => {
                    event_edges.push((oe.kind.clone(), from.clone(), oe.target.clone()))
                }
                _ => {}
            }
        }
    }
    let tree_refs: Vec<(&str, &str)> = tree_edges
        .iter()
        .map(|(s, t)| (s.as_str(), t.as_str()))
        .collect();
    validate_trees_acyclic(&tree_refs, universe)?;
    let event_refs: Vec<(&str, &str, &str)> = event_edges
        .iter()
        .map(|(k, s, t)| (k.as_str(), s.as_str(), t.as_str()))
        .collect();
    validate_event_targets(&event_refs, assembled)
}

// ---------------------------------------------------------------------------
// SPEC §3.1 — group coupling, derived never stored (phase-3 task-5)
// ---------------------------------------------------------------------------

/// Derive the group-coupling relation from a set of domain service/event edges
/// (SPEC §3.1: "Group coupling is derived, never stored"). A and B are coupled
/// iff a `calls`/`publishes`/`subscribes` edge chain connects them; coupling
/// is the transitive closure of that relation, computed as undirected
/// connectivity over the services and events the edges name — a `calls` chain
/// (Service→Service) connects its services, and a shared event (a `publishes`
/// Service→Entity and a `subscribes` Service→Entity to the same event) connects
/// publisher and subscriber through the event medium. The relation is
/// symmetric ("A and B are coupled"), so two services are coupled iff they sit
/// in the same connected component.
///
/// The coupled units are the GROUPS that own the services, not the services —
/// a service's FQN (`domain.service.<name>`) does not encode its owning group,
/// so ownership is an input: `owner` maps a service FQN to its owning group
/// FQN. A node absent from `owner` (an event, or an unmapped service) is never
/// a coupled unit — an event is only the medium. Each connected component's
/// distinct groups are pairwise coupled; a component with a single group (e.g.
/// a lone service or an event nobody else touches) couples nothing.
///
/// `edges` is the caller's `(kind, source FQN, target FQN)` triples; only
/// `calls`/`publishes`/`subscribes` are coupling edges (any other kind is
/// ignored — [`validate_edges`] already refuses it). The DDD context-map flavor
/// (direct/published/translated/shared/coevolving) is an edge attribute on
/// those edges, kept by the caller — it is never a node type, never a
/// Group→Group edge, and never part of this result.
///
/// Returns the derived coupled pairs as a [`BTreeSet`] of `(group, group)`
/// FQNs, each normalized so the lower FQN sorts first (a pair is unordered).
/// Purely derived data — nothing is stored and no Group→Group edge or coupling
/// node is produced.
// (Unused until a later phase surfaces the derived coupling relation.)
#[allow(dead_code)]
pub fn derive_coupling(
    edges: &[(&str, &str, &str)],
    owner: &BTreeMap<String, String>,
) -> BTreeSet<(String, String)> {
    // Undirected adjacency over every node the coupling edges name.
    let mut adj: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for &(kind, src, dst) in edges {
        if matches!(kind, "calls" | "publishes" | "subscribes") {
            adj.entry(src).or_default().insert(dst);
            adj.entry(dst).or_default().insert(src);
        }
    }

    // Connected components (DFS); each component's owned groups are pairwise
    // coupled. An isolated node never appears in `adj`, so no chain → no pair.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut coupled: BTreeSet<(String, String)> = BTreeSet::new();
    for &start in adj.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut groups: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            if let Some(group) = owner.get(node) {
                groups.insert(group.clone());
            }
            if let Some(nexts) = adj.get(node) {
                for &next in nexts {
                    if seen.insert(next) {
                        stack.push(next);
                    }
                }
            }
        }
        let list: Vec<&String> = groups.iter().collect();
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                coupled.insert((list[i].clone(), list[j].clone()));
            }
        }
    }
    coupled
}

// ---------------------------------------------------------------------------
// SPEC §3.1 — constraints are prose, review-only (phase-3 task-6)
// ---------------------------------------------------------------------------

/// Property key a **local** constraint uses to name the node it constrains
/// (SPEC §3.1: "Local constraints attach to any tier-1–3 node"). The value is
/// one authored-node FQN `<layer>.<type>.<name>` — the single node the prose
/// "must hold" about. This is the *only* constraint property the binary
/// interprets: a constraint has no expression language, so an attachment is a
/// reference, never an expression.
pub const PROP_ATTACHES_TO: &str = "attaches-to";

/// Validate a proposed `constraint` node at write time (SPEC §3.1): constraints
/// are **prose** ("X must hold") over things that **exist**. The binary
/// validates only a constraint's *structure* and *references* — never whether
/// the prose actually holds. **Satisfaction is assessed by review**
/// (non-deterministic), never executed: there is no constraint-expression
/// language in this change-set, and this function takes no prose input at all
/// (the prose `body` is the node-file schema's top-level `body`, stored
/// verbatim and read by a reviewer — not by the binary).
///
/// Structure (reuses [`validate_node`]'s shared node rules — a `constraint`
/// carries no `kind`/`attribute`/`root`): the name matches the allowlist, the
/// type exists in its layer (requirements/domain/solution/implementation/global
/// host `constraint`; plans does not, so a plans-layer constraint is refused),
/// and the name is unique per (layer, type).
///
/// References (the "never a non-thing" rule): a **local** constraint
/// (requirements/domain/solution) *may* name the one tier-1–3 node it
/// constrains via [`PROP_ATTACHES_TO`] — that FQN must parse and resolve
/// against `existing` (the caller-supplied (layer, type, name) universe), or
/// the write is refused. A **global** constraint ([`Layer::Global`]) guards the
/// whole graph and must not declare an attachment — one is refused, not
/// ignored (naming one thing contradicts whole-graph scope).
///
/// `existing` is the current identity universe, exactly as in [`validate_node`]:
/// every node that already exists (or is co-proposed in the change) — *not* the
/// constraint being validated.
// (Unused until ingest_tree, phase-3 task-15, consults it.)
#[allow(dead_code)]
pub fn eval_constraint(
    layer: Layer,
    name: &str,
    properties: &NodeProperties,
    existing: &BTreeSet<(Layer, String, String)>,
) -> anyhow::Result<()> {
    // Structure: reuse the shared node rules — name allowlist, type-in-layer,
    // uniqueness. A `constraint` takes no kind/attribute/root.
    validate_node(layer, "constraint", name, properties, existing)?;

    // References: only an `attaches-to` property is interpreted (there is no
    // expression language — the prose body is never an input here).
    let Some(target) = properties.get(PROP_ATTACHES_TO) else {
        return Ok(());
    };

    // A global constraint guards the whole graph — an attachment is refused.
    if layer == Layer::Global {
        anyhow::bail!(
            "global constraint `{name}` must not declare `{PROP_ATTACHES_TO}` — a global constraint guards the whole graph"
        );
    }

    // A local constraint attaches to a tier-1–3 node. The target must parse as
    // an authored-node FQN, live in a tier-1–3 layer, and resolve — never a
    // non-thing.
    let (target_layer, target_type, target_name) = parse_fqn(target.as_str())?;
    if !matches!(
        target_layer,
        Layer::Requirements | Layer::Domain | Layer::Solution
    ) {
        anyhow::bail!(
            "constraint `{name}` attaches to `{target}`, which is not a tier-1–3 node — local constraints attach to requirements/domain/solution"
        );
    }
    if !existing.contains(&(target_layer, target_type, target_name)) {
        anyhow::bail!(
            "constraint `{name}` attaches to `{target}`, which does not exist — a constraint declares what must hold about something that EXISTS (never a non-thing)"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Node-file schema + single-node writer (phase-3 task-8)
// ---------------------------------------------------------------------------

/// One node file's schema (SPEC §4.1). `layer`/`type`/`name` are the identity
/// — the file name IS the identity, so the FQN is derived, never stored:
/// `<layer>.<type>.<name>` (global per-layer namespace, no project prefix).
/// `body` is the prose; `properties` is metadata (short ids live here, never
/// as identity); `out`/`in` hold both edge directions in this file (out in the
/// source's file, in in the target's — the pairing check is task-9, and the
/// edge/type validation is task-16's [`validate_node`]/[`validate_edges`],
/// called by write_project **before** [`write_node`]).
// (Unused until write_project, phase-3 task-16, calls write_node.)
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeFile {
    /// The layer dir name (e.g. `requirements`) — must equal the path segment.
    pub layer: String,
    /// The node type (e.g. `requirement`) — must equal the path segment. Rust
    /// reserves `type`, so the field is `node_type` renamed to `"type"` on the
    /// wire.
    #[serde(rename = "type")]
    pub node_type: String,
    /// The node name (e.g. `place-order`) — must equal the file-name stem.
    pub name: String,
    /// The node's prose body.
    #[serde(default)]
    pub body: String,
    /// Metadata only — short ids and any other keys, never identity.
    #[serde(default)]
    pub properties: NodeProperties,
    /// Outgoing edges (this node is the source) — canonical for graph assembly.
    #[serde(default)]
    pub out: Vec<OutEdge>,
    /// Incoming edges (this node is the target). Rust reserves `in`, so the
    /// field is `in_edges` renamed to `"in"` on the wire.
    #[serde(default, rename = "in")]
    pub in_edges: Vec<InEdge>,
}

/// One out-edge in a node file (SPEC §4.1): this node is the source. Two
/// separate structs for out/in — they carry different fields (`target` vs
/// `source`).
// (Unused until write_project, phase-3 task-16, calls write_node.)
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutEdge {
    /// The edge kind (e.g. `drives`).
    pub kind: String,
    /// The target node FQN (`<layer>.<type>.<name>`, or a code FQN for
    /// `implemented-by`).
    pub target: String,
    /// Edge properties (metadata — e.g. the context-map flavor).
    #[serde(default)]
    pub properties: NodeProperties,
}

/// One in-edge in a node file (SPEC §4.1): this node is the target.
// (Unused until write_project, phase-3 task-16, calls write_node.)
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InEdge {
    /// The edge kind (e.g. `contains`).
    pub kind: String,
    /// The source node FQN (`<layer>.<type>.<name>`).
    pub source: String,
    /// Edge properties (metadata).
    #[serde(default)]
    pub properties: NodeProperties,
}

/// Build an authored-node FQN (`<layer>.<type>.<name>`) from its identity —
/// the inverse of [`parse_fqn`]. The file name IS the identity, so the FQN is
/// derived, never stored.
// (Unused until write_project, phase-3 task-16, and ingest_tree, task-15, call it.)
#[allow(dead_code)]
pub fn fqn(layer: Layer, node_type: &str, name: &str) -> String {
    format!("{}.{node_type}.{name}", layer.layer_dir())
}

/// Write one node file at `<apg_root>/layers/<layer>/<type>/<name>.json`
/// (SPEC §4.1). The path is derived from the node's own layer/type/name, and
/// those same values are what the serializer writes — so the file's
/// `layer`/`type`/`name` fields match the path by construction, and the FQN is
/// the path's segments.
///
/// This is the **single-file** writer: it creates the parent dirs and writes
/// the one file (a plain [`std::fs::write`] — the multi-file atomicity is
/// task-11's `write_through`). It does **not** validate edges or pairing
/// (task-9/11) and does **not** validate the type against its layer's catalog
/// (that is [`validate_node`], called by write_project before this) — it only
/// does the cheap identity sanity checks a file name demands:
///
/// - the layer must be a known layer, and must **not** be plans — plans is
///   transient (`.trans/plans/` only, per branch), never a durable node-file
///   layer;
/// - the name must match the allowlist `[a-z0-9][a-z0-9-]*` (refused, never
///   sanitized), so the file name stays safe.
///
/// Returns the written file's path. Nothing here treats a `properties` key as
/// identity — short ids are metadata, stored verbatim.
// (Unused until write_project, phase-3 task-16, calls it.)
#[allow(dead_code)]
pub fn write_node(apg_root: &Path, node: &NodeFile) -> anyhow::Result<PathBuf> {
    let layer = Layer::ALL
        .iter()
        .find(|l| l.layer_dir() == node.layer)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
    if layer.storage() == StoragePolicy::TransientPlans {
        anyhow::bail!(
            "layer `plans` is transient (apg/.trans/plans/) — not a durable node-file layer"
        );
    }
    if !valid_name(&node.name) {
        anyhow::bail!(
            "node name `{}` is invalid — the name allowlist is [a-z0-9][a-z0-9-]* (refused, never sanitized)",
            node.name
        );
    }
    let path = apg_root
        .join(LAYERS_DIR)
        .join(layer.layer_dir())
        .join(&node.node_type)
        .join(format!("{}.json", node.name));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(node)?)?;
    Ok(path)
}

/// The node-file path for an identity (SPEC §4.1): the same derivation
/// [`write_node`] uses. A helper for the command layer (`node_cmd`).
pub fn node_file_path(apg_root: &Path, layer: Layer, node_type: &str, name: &str) -> PathBuf {
    apg_root
        .join(LAYERS_DIR)
        .join(layer.layer_dir())
        .join(node_type)
        .join(format!("{name}.json"))
}

/// Read one node file by identity (SPEC §4.1) — errors when it does not
/// exist. A helper for the command layer (`node_cmd`).
pub fn read_node_file(
    apg_root: &Path,
    layer: Layer,
    node_type: &str,
    name: &str,
) -> anyhow::Result<NodeFile> {
    let path = node_file_path(apg_root, layer, node_type, name);
    let text = std::fs::read_to_string(&path)
        .map_err(|_| anyhow::anyhow!("no node file at {}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

/// The strict-add existence gate shared by the mutation commands (SPEC §1
/// "no implicit upsert"): refuse an `add` whose target already exists rather
/// than silently replacing the existing file (which would discard its
/// incident edges). `entity` names the existing identity; `update`/`rm` name
/// the correct follow-up commands, so the refusal tells the caller how to
/// proceed. `exists` is supplied by the caller so the same gate serves both
/// the node-FQN check and the edge-triple duplicate check.
pub fn refuse_if_present(exists: bool, entity: &str, update: &str, rm: &str) -> anyhow::Result<()> {
    if exists {
        anyhow::bail!(
            "`{entity}` already exists — use `{update}` to change it or `{rm}` to remove it"
        );
    }
    Ok(())
}

/// MERGE a property edit over `base` (SPEC §4.1 property-map): the keys in
/// `set` overwrite only what they carry, the keys in `unset` are deleted, and
/// every other key is preserved — omitting `unset` never drops a key; there is
/// no implicit unset. The shared merge helper behind [`update_node_file`] and
/// the `node update` / `edge update` command surfaces.
pub fn merge_properties(
    base: &NodeProperties,
    set: &BTreeMap<String, String>,
    unset: &BTreeSet<String>,
) -> NodeProperties {
    let mut merged = base.clone();
    for (k, v) in set {
        merged.insert(k.clone(), v.clone());
    }
    for k in unset {
        merged.remove(k);
    }
    merged
}

/// Compute the edge-preserving in-place update of a node file (SPEC §4.1):
/// read `layer.type.name`, refuse when it is absent, set `body` when supplied,
/// MERGE `set`/`unset` over its properties via [`merge_properties`], and leave
/// the node's immutable identity (`layer`/`type`/`name`) and every out/in edge
/// untouched. Pure — the caller persists the returned node through the guarded
/// [`write_project`] funnel.
pub fn update_node_file(
    apg_root: &Path,
    layer: Layer,
    node_type: &str,
    name: &str,
    body: Option<&str>,
    set: &BTreeMap<String, String>,
    unset: &BTreeSet<String>,
) -> anyhow::Result<NodeFile> {
    let f = fqn(layer, node_type, name);
    if !node_file_path(apg_root, layer, node_type, name).exists() {
        anyhow::bail!("node `{f}` does not exist — use `apg node add` to create it");
    }
    let mut node = read_node_file(apg_root, layer, node_type, name)?;
    if let Some(body) = body {
        node.body = body.to_string();
    }
    node.properties = merge_properties(&node.properties, set, unset);
    Ok(node)
}

// ---------------------------------------------------------------------------
// SPEC §4.1 — in/out edge pairing (phase-3 task-9)
// ---------------------------------------------------------------------------

/// Verify the SPEC §4.1 pairwise edge invariant over the node files the
/// caller supplies: **an edge appears in BOTH endpoint files** — out in the
/// source's file, in in the target's. An in/out edge in one file without the
/// matching out/in edge in the other endpoint's file is an ERROR (caught at
/// ingestion). A match means the **same source, kind, target, AND edge
/// properties** — not merely endpoint existence. **Outgoing edges are the
/// canonical source** for building the graph (task-15's [`ingest_tree`]);
/// this function only VERIFIES symmetry — it builds nothing.
///
/// Authored vs code endpoints (SPEC §4.1 "code endpoints are exempt"): a code
/// node has no file, so an edge to/from code has only the spec-side half. The
/// discriminator is whether the endpoint parses as an authored-node FQN
/// `<layer>.<type>.<name>` ([`parse_fqn`]) — **not** the edge kind. If it
/// parses, the endpoint is authored and MUST be present (a dangling authored
/// reference is an error) and MUST carry a matching counterpart (the target
/// node's in-edge with source = this node's FQN and equal kind + properties;
/// the symmetric rule for an in-edge's source). If it does not parse, it is a
/// code FQN and the pairing check is skipped — the `implemented-by` target and
/// a `details` target may both be code, and this parse-based rule is the
/// clean, correct discriminator that matches "code nodes have no files".
///
/// Transient edges are OUT of scope here: committed durable node files carry
/// only §3.3 edges ([`validate_edges`] refuses the §5 transient kinds
/// upstream), and a transient-to-durable relationship lives ENTIRELY in
/// `.trans` (both halves there — §4.1: committed files never hold transient
/// references). Transient pair validation is the `.trans`-side job
/// (phase-4), never this function.
///
/// Pure — no I/O; the caller supplies the node files to check. Builds the
/// FQN → node-file map with [`fqn`] (the file name IS the identity), then
/// verifies every edge half against its counterpart.
// (Unused until ingest_tree, phase-3 task-15, calls it.)
#[allow(dead_code)]
pub fn check_edge_pairing(nodes: &[NodeFile]) -> anyhow::Result<()> {
    // FQN → node file: the identity universe the pairing check resolves
    // authored endpoints against. The FQN is derived (`fqn`), never read.
    let mut by_fqn: BTreeMap<String, &NodeFile> = BTreeMap::new();
    for node in nodes {
        let layer = Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == node.layer)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
        by_fqn.insert(fqn(layer, &node.node_type, &node.name), node);
    }

    for (f, node) in &by_fqn {
        // Outgoing halves — this node is the source; the target file must hold
        // the matching in-edge (out-edges are canonical for graph assembly).
        for out_edge in &node.out {
            // Code-exempt: a non-parsing target is a code FQN, no pairing.
            if parse_fqn(&out_edge.target).is_err() {
                continue;
            }
            let Some(target) = by_fqn.get(&out_edge.target) else {
                anyhow::bail!(
                    "node `{f}` out edge `{}` -> `{}`: the target is an authored node but no node file supplies it (dangling authored reference)",
                    out_edge.kind,
                    out_edge.target
                );
            };
            let exact = target.in_edges.iter().any(|ie| {
                ie.kind.as_str() == out_edge.kind.as_str()
                    && ie.source.as_str() == f.as_str()
                    && ie.properties == out_edge.properties
            });
            if exact {
                continue;
            }
            let same_endpoints = target.in_edges.iter().any(|ie| {
                ie.kind.as_str() == out_edge.kind.as_str() && ie.source.as_str() == f.as_str()
            });
            if same_endpoints {
                anyhow::bail!(
                    "node `{f}` out edge `{}` -> `{}`: the in edge on `{}` has different properties — a match requires identical edge properties (same source, kind, target, AND properties)",
                    out_edge.kind,
                    out_edge.target,
                    out_edge.target
                );
            }
            anyhow::bail!(
                "node `{f}` out edge `{}` -> `{}` has no matching in edge on `{}` — an edge must appear in BOTH endpoint files",
                out_edge.kind,
                out_edge.target,
                out_edge.target
            );
        }

        // Incoming halves — this node is the target; the source file must hold
        // the matching out-edge.
        for in_edge in &node.in_edges {
            // Code-exempt: a non-parsing source is a code FQN, no pairing.
            if parse_fqn(&in_edge.source).is_err() {
                continue;
            }
            let Some(source) = by_fqn.get(&in_edge.source) else {
                anyhow::bail!(
                    "node `{f}` in edge `{}` <- `{}`: the source is an authored node but no node file supplies it (dangling authored reference)",
                    in_edge.kind,
                    in_edge.source
                );
            };
            let exact = source.out.iter().any(|oe| {
                oe.kind.as_str() == in_edge.kind.as_str()
                    && oe.target.as_str() == f.as_str()
                    && oe.properties == in_edge.properties
            });
            if exact {
                continue;
            }
            let same_endpoints = source.out.iter().any(|oe| {
                oe.kind.as_str() == in_edge.kind.as_str() && oe.target.as_str() == f.as_str()
            });
            if same_endpoints {
                anyhow::bail!(
                    "node `{f}` in edge `{}` <- `{}`: the out edge on `{}` has different properties — a match requires identical edge properties (same source, kind, target, AND properties)",
                    in_edge.kind,
                    in_edge.source,
                    in_edge.source
                );
            }
            anyhow::bail!(
                "node `{f}` in edge `{}` <- `{}` has no matching out edge on `{}` — an edge must appear in BOTH endpoint files",
                in_edge.kind,
                in_edge.source,
                in_edge.source
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SPEC §4.1 — code-endpoint validation (phase-3 task-10)
// ---------------------------------------------------------------------------

/// The status of one `implemented-by` code FQN against the scanned graph
/// (SPEC §4.1 "code endpoints are exempt"): a code FQN is language-native and
/// opaque — never parsed, never layer-classified. Exact string set membership
/// against the two caller-supplied universes is the whole check.
// (Unused until ingest_tree, phase-3 task-15, validates implemented-by targets.)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeRefStatus {
    /// The FQN resolves in the scanned graph — the code exists.
    Real,
    /// The FQN is declared as a planned node in `.trans` but not yet scanned —
    /// pending, not an error (it realizes once the code lands).
    Pending,
    /// The FQN is in neither universe — the code is gone from the scanned
    /// graph (spec drift).
    Drift,
}

/// The `lang_switch` scan identities a code FQN can be rooted under — the
/// complete set `available_languages`/`id_prefix_for` recognise (`ts` and `js`
/// are the unified JS/TS frontend's two ids; a JS-only repo scans under `js`).
/// Used only to resolve an authored code reference against the scanned universe
/// tolerantly (see [`resolves_in_scanned`]).
pub const LANGUAGE_ROOTS: [&str; 9] = [
    "rust", "java", "go", "cpp", "csharp", "ts", "js", "py", "md",
];

/// True when `fqn` resolves in `scanned`, tolerating the language root: an
/// authored reference may be stored **un-rooted** (`apg.cmd_scan`) while the
/// scanned universe is rooted (`rust.apg.cmd_scan`), or the reverse. This keeps
/// the durable spec language-agnostic so a binary that predates rooting (the
/// parent) and a rooted binary (the child) resolve the *same* authored target —
/// neither depends on the other. Exact membership always wins first, then a
/// `<root>.` prefix add/strip for every known language root.
fn resolves_in_scanned(fqn: &str, scanned: &BTreeSet<String>) -> bool {
    if scanned.contains(fqn) {
        return true;
    }
    for root in LANGUAGE_ROOTS {
        if scanned.contains(&format!("{root}.{fqn}")) {
            return true;
        }
        if let Some(rest) = fqn.strip_prefix(root).and_then(|s| s.strip_prefix('.'))
            && scanned.contains(rest)
        {
            return true;
        }
    }
    false
}

/// The **language-agnostic identity** of a code FQN: strip ONE leading known
/// language root (`rust.`/`java.`/`go.`/`cpp.`/`csharp.`/`ts.`/`js.`/`py.`/`md.`)
/// and return the remainder; anything else is returned unchanged. This is the
/// stable identity the durable spec is authored against, so a rooted FQN
/// (`rust.apg.cache`) and its bare counterpart (`apg.cache`) compare EQUAL.
/// [`resolves_in_scanned`] applies the same tolerance to universe membership;
/// this exposes it to the verify paths that compare two authored code FQNs to
/// each other (planned-node realization, derived coverage) rather than to a
/// scanned universe.
pub fn code_identity(fqn: &str) -> &str {
    for root in LANGUAGE_ROOTS {
        if let Some(rest) = fqn.strip_prefix(root).and_then(|s| s.strip_prefix('.')) {
            return rest;
        }
    }
    fqn
}

/// Classify one `implemented-by` code FQN (SPEC §4.1): [`CodeRefStatus::Real`]
/// if `fqn` resolves in `scanned` (see [`resolves_in_scanned`] — rooting
/// tolerant); [`CodeRefStatus::Pending`] if it is in `planned` (and not
/// `scanned`); [`CodeRefStatus::Drift`] otherwise. `scanned` is the set of code
/// FQNs the scan produced; `planned` is the set of FQNs the plan declared as
/// planned nodes (`.trans`).
///
/// **The scanned graph is the stronger check**: membership in `scanned` wins
/// over membership in `planned` — a FQN in both universes is `Real` (it has
/// landed; the plan's planned-node declaration is moot).
// (Unused until ingest_tree, phase-3 task-15, validates implemented-by targets.)
#[allow(dead_code)]
pub fn classify_code_ref(
    fqn: &str,
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> CodeRefStatus {
    if resolves_in_scanned(fqn, scanned) {
        CodeRefStatus::Real
    } else if planned.contains(fqn) {
        CodeRefStatus::Pending
    } else {
        CodeRefStatus::Drift
    }
}

/// Validate a batch of `implemented-by` code FQNs against the scanned graph
/// (SPEC §4.1): returns Ok if every ref is [`CodeRefStatus::Real`] or
/// [`CodeRefStatus::Pending`]. A Pending ref is expected until the code lands
/// (the scan later realizes it), never an error. Only a
/// [`CodeRefStatus::Drift`] ref errors — the FQN is gone from the scanned graph
/// (spec drift). Bails on the first Drift, naming the offending FQN. Pure — no
/// I/O; the caller supplies both universes.
// (Unused until ingest_tree, phase-3 task-15, validates implemented-by targets.)
#[allow(dead_code)]
pub fn validate_code_refs(
    refs: &[&str],
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<()> {
    for fqn in refs {
        if classify_code_ref(fqn, scanned, planned) == CodeRefStatus::Drift {
            anyhow::bail!("spec drift: code FQN `{fqn}` is gone from the scanned graph");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SPEC §4.1 — atomic multi-file write-through (phase-3 task-11)
// ---------------------------------------------------------------------------

/// Write a SET of node files atomically (SPEC §4.1 "renames / deletions are
/// atomic write-throughs"): one logical mutation updates ALL affected files —
/// the moved/removed file plus every referencing file whose edges/FQNs change
/// — and commits once. `writes` is the complete set of affected node files
/// (full new content for each); each path is derived from its own
/// layer/type/name, exactly as [`write_node`] derives it
/// (`<apg_root>/layers/<layer>/<type>/<name>.json`).
///
/// The complete proposed change is validated BEFORE anything is written, and
/// on any failure the previous state is restored — never leave mismatched
/// endpoint files. The pre-check here is **structural**, not semantic:
///
/// - a plans-layer node is refused (plans is transient — `.trans/plans/` only
///   — never a durable node-file layer);
/// - an allowlist-violating name is refused (reuse [`valid_name`] — never
///   sanitized);
/// - two `writes` entries colliding on the same path/FQN are refused.
///
/// The FULL semantic validation — [`validate_node`] uniqueness against the
/// whole store, [`check_edge_pairing`], the edge matrix ([`validate_edges`]) —
/// is the caller's job: `write_project` (task-16) runs validate_node/
/// validate_edges before this, and `ingest_tree` (task-15) runs pairing.
///
/// Atomicity: for every path, the pre-existing bytes are recorded
/// (`None` when the file did not exist) BEFORE any write; then all files are
/// written (parent dirs created). If ANY write fails, every path is restored —
/// write back the recorded bytes, or delete the file if it did not previously
/// exist — and the error is returned. After all writes succeed, the affected
/// paths are committed in a single commit; a commit failure rolls the writes
/// back too. Outside a git repo there is no commit — the mutation still lands
/// on disk (keeps non-git temp-dir tests working; real mutations run in a
/// project worktree).
// (Unused until write_project, phase-3 task-16, calls it.)
#[allow(dead_code)]
pub fn write_through(apg_root: &Path, writes: &[NodeFile]) -> anyhow::Result<()> {
    // --- Structural pre-check (before any filesystem touch) ---
    let mut paths: Vec<PathBuf> = Vec::with_capacity(writes.len());
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for node in writes {
        let layer = Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == node.layer)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
        if layer.storage() == StoragePolicy::TransientPlans {
            anyhow::bail!(
                "layer `plans` is transient (apg/.trans/plans/) — not a durable node-file layer"
            );
        }
        if !valid_name(&node.name) {
            anyhow::bail!(
                "node name `{}` is invalid — the name allowlist is [a-z0-9][a-z0-9-]* (refused, never sanitized)",
                node.name
            );
        }
        let path = apg_root
            .join(LAYERS_DIR)
            .join(layer.layer_dir())
            .join(&node.node_type)
            .join(format!("{}.json", node.name));
        if !seen.insert(path.clone()) {
            anyhow::bail!(
                "duplicate write: two entries target `{}` — one logical mutation touches each node file once",
                path.display()
            );
        }
        paths.push(path);
    }

    // Serialize every node file up front — a serialization failure is caught
    // before anything is written, so no rollback is needed for it.
    let serialized: Vec<String> = writes
        .iter()
        .map(serde_json::to_string_pretty)
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("serialize node file failed: {e}"))?;

    // Record the pre-existing bytes of every path BEFORE any write (None = the
    // file did not exist) — the snapshot a failure restores from.
    let mut prior: Vec<Option<Vec<u8>>> = Vec::with_capacity(paths.len());
    for path in &paths {
        prior.push(std::fs::read(path).ok());
    }

    // Write every file; on the first failure restore the whole set and return.
    for (i, path) in paths.iter().enumerate() {
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!(
                "create_dir_all {} failed: {e}",
                parent.display()
            ));
        }
        if let Err(e) = std::fs::write(path, &serialized[i]) {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!("write {} failed: {e}", path.display()));
        }
    }

    // Commit once — all affected paths together. Outside a git repo there is
    // no commit (the mutation still lands on disk); a commit failure rolls the
    // writes back so a failed mutation never leaves mismatched files.
    if git::in_repo(apg_root) {
        let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        let msg = git::graph_mutation_message(apg_root, &refs);
        match git::commit_files(apg_root, &refs, &[], &msg) {
            Ok(Some(_)) => {
                // Re-anchor the staleness gate's recorded scan_meta (mirrors
                // the JSONL funnel's auto-commit — DB and tree in sync by
                // construction). A re-anchor failure degrades to a warning.
                if let Err(e) = git::reanchor_scan_meta(apg_root, &git::git_state(apg_root)) {
                    eprintln!(
                        "apg: warning: could not re-anchor scan_meta after node-file commit: {e:#}"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                rollback(&paths, &prior);
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Restore the previous state of every path after a failed write: write back
/// the recorded bytes, or delete the file when it did not previously exist.
/// Best-effort — the caller returns the primary failure; a rollback step that
/// cannot be applied (e.g. a path blocked by a directory) is left as-is.
// (Unused until write_project, phase-3 task-16, wires write_through.)
#[allow(dead_code)]
fn rollback(paths: &[PathBuf], prior: &[Option<Vec<u8>>]) {
    for (path, prev) in paths.iter().zip(prior.iter()) {
        match prev {
            Some(bytes) => {
                let _ = std::fs::write(path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SPEC §4.1 — tree ingestion (phase-3 task-15)
// ---------------------------------------------------------------------------

/// Ingest the durable `apg/layers/` tree into the new-model graph records
/// (apg-projects SPEC §4.1): walk [`LAYERS_TREE`], deserialize every
/// `<layer>/<type>/<name>.json` into a [`NodeFile`], validate the assembled
/// set, and convert it into the unified-JSONL `Record` stream `cmd_scan`
/// chains after the scanner records.
///
/// Validation (all ERRORs, never silent):
/// - [`check_edge_pairing`] over every node file — an in/out edge in one file
///   without the matching out/in edge (same source, kind, target, AND
///   properties) in the other endpoint's file is refused (R16 AC).
/// - [`validate_assembled_rules`] over the assembled set — contains/depends-on
///   trees acyclic ([`validate_trees_acyclic`]) and `publishes`/`subscribes`
///   targets are `Entity (kind: event)` ([`validate_event_targets`]) (SPEC
///   §3.3 property rules).
/// - [`validate_code_refs`] on every `implemented-by` target against
///   `scanned_code` (the code FQNs the just-run scan produced) and `planned`
///   (the plan's planned-node FQNs, from `.trans`): resolves → real; planned →
///   pending (not an error); gone from both → spec-drift error.
/// - [`eval_constraint`] on every `constraint` node (structure + reference
///   validation; satisfaction is review-only, R14).
///
/// The FQN of each node is derived from its **path** (`fqn(layer, type,
/// name)`), never read — the file name IS the identity, and the file's own
/// `layer`/`type`/`name` fields must match the path (SPEC §4.1). Out-edges
/// are the canonical source for the emitted edge records; in-edges are
/// verified (pairing) but never emitted.
pub fn ingest_tree(
    apg_root: &Path,
    scanned_code: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<Vec<Record>> {
    // Walk the durable tree, deserializing one NodeFile per `<name>.json`.
    let mut nodes: Vec<NodeFile> = Vec::new();
    for (layer_dir, types) in LAYERS_TREE {
        for node_type in *types {
            let dir = apg_root.join(LAYERS_DIR).join(layer_dir).join(node_type);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "json") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                let nf: NodeFile = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("{}: bad node file: {e}", path.display()))?;
                // The file name IS the identity: the layer/type/name fields
                // must match the path segments (SPEC §4.1).
                if nf.layer != *layer_dir || nf.node_type != *node_type || nf.name != name {
                    anyhow::bail!(
                        "node file {}: layer/type/name fields must match the path segments",
                        path.display()
                    );
                }
                nodes.push(nf);
            }
        }
    }
    // Deterministic record order (one file per node, so paths are unique).
    nodes.sort_by(|a, b| (&a.layer, &a.node_type, &a.name).cmp(&(&b.layer, &b.node_type, &b.name)));

    // The assembled identity universe + FQN → node map: the constraint check
    // and the property-aware §3.3 rules both resolve against them.
    let universe: BTreeSet<(Layer, String, String)> = nodes
        .iter()
        .map(|n| (layer_of(&n.layer), n.node_type.clone(), n.name.clone()))
        .collect();
    let assembled: BTreeMap<String, &NodeFile> = nodes
        .iter()
        .map(|n| (fqn(layer_of(&n.layer), &n.node_type, &n.name), n))
        .collect();

    // 1. Pairwise in/out symmetry across ALL node files (R16 AC).
    check_edge_pairing(&nodes)?;

    // 2. Property-aware §3.3 rules over the assembled set: contains/depends-on
    // trees acyclic, and publishes/subscribes targets are Entity (kind: event).
    validate_assembled_rules(&assembled, &universe)?;

    // 3. `implemented-by` code refs against the scanned graph + planned nodes.
    let refs: Vec<&str> = nodes
        .iter()
        .flat_map(|n| n.out.iter())
        .filter(|oe| oe.kind == "implemented-by")
        .map(|oe| oe.target.as_str())
        .collect();
    validate_code_refs(&refs, scanned_code, planned)?;

    // 4. Constraint structure/reference validation over the assembled graph.
    for n in &nodes {
        if n.node_type != "constraint" {
            continue;
        }
        // The constraint under validation is not part of its own existence
        // universe: `eval_constraint` reuses `validate_node`'s uniqueness
        // rule, which compares against everything EXCEPT the node being
        // validated (the same exclusion `validate_change` applies). Duplicates
        // among constraints are still caught — each one validates against the
        // other's presence.
        let mut own_universe = universe.clone();
        own_universe.remove(&(layer_of(&n.layer), n.node_type.clone(), n.name.clone()));
        eval_constraint(layer_of(&n.layer), &n.name, &n.properties, &own_universe)?;
    }

    // Convert node files + out-edges into records (out is canonical).
    let mut records = Vec::new();
    for n in &nodes {
        let layer = layer_of(&n.layer);
        let f = fqn(layer, &n.node_type, &n.name);
        records.push(node_record(&f, &n.node_type, n)?);
        for oe in &n.out {
            records.push(edge_record(&f, &oe.kind, &oe.target)?);
        }
    }
    Ok(records)
}

/// Resolve a node file's `layer` string to its [`Layer`] — a hard error on an
/// unknown layer (the catalog is closed).
fn layer_of(layer_dir: &str) -> Layer {
    match Layer::ALL.iter().find(|l| l.layer_dir() == layer_dir) {
        Some(l) => *l,
        None => panic!("unknown layer `{layer_dir}`"),
    }
}

/// Convert one node file into its new-model node record. `f` is the derived
/// FQN (`<layer>.<type>.<name>`); `properties` carries the §3.1 attributes
/// (entity/container `kind`, group `attribute`/`root`, constraint
/// `attaches-to`, a requirement's metadata `id`/`feature`) — short ids are
/// metadata, never identity.
fn node_record(f: &str, node_type: &str, n: &NodeFile) -> anyhow::Result<Record> {
    let name = n.name.clone();
    let body = n.body.clone();
    let prop = |k: &str| n.properties.get(k).cloned().unwrap_or_default();
    Ok(match node_type {
        "stakeholder" => Record::Stakeholder {
            fqn: f.to_string(),
            name,
            body,
        },
        "user" => Record::User {
            fqn: f.to_string(),
            name,
            body,
        },
        "requirement" => Record::Requirement {
            fqn: f.to_string(),
            id: prop("id"),
            title: name,
            body,
            feature: prop("feature"),
        },
        "note" => Record::Note {
            fqn: f.to_string(),
            body,
            kind: prop("kind"),
        },
        "constraint" => Record::Constraint {
            fqn: f.to_string(),
            name,
            body,
            attaches_to: prop("attaches-to"),
        },
        "group" => Record::Group {
            fqn: f.to_string(),
            name,
            attribute: prop("attribute"),
            root: prop("root"),
            body,
        },
        "entity" => Record::Entity {
            fqn: f.to_string(),
            name,
            body,
        },
        "value" => Record::Value {
            fqn: f.to_string(),
            name,
            body,
        },
        "service" => Record::Service {
            fqn: f.to_string(),
            name,
            body,
        },
        "system" => Record::System {
            fqn: f.to_string(),
            name,
            body,
        },
        "container" => Record::Container {
            fqn: f.to_string(),
            name,
            kind: prop("kind"),
            body,
        },
        "component" => Record::Component {
            fqn: f.to_string(),
            name,
            body,
        },
        "person" => Record::Person {
            fqn: f.to_string(),
            name,
            body,
        },
        other => anyhow::bail!("unknown node type `{other}` in layer `{}`", n.layer),
    })
}

/// Convert one out-edge into its new-model edge record. `from` is the source
/// node's derived FQN. The §3.3 kebab kinds map to the new-model records
/// (`realised-by` → `RealisedBy`, `implemented-by` → `SpecImplementedBy`);
/// `contains`/`drives`/`calls`/`uses`/`represents`/`details`/`depends-on` map
/// to the shared records the ingestor routes by endpoint node-kind.
fn edge_record(from: &str, kind: &str, to: &str) -> anyhow::Result<Record> {
    let from = from.to_string();
    let to = to.to_string();
    Ok(match kind {
        "contains" => Record::Contains { from, to },
        "drives" => Record::Drives { from, to },
        "realised-by" => Record::RealisedBy { from, to },
        "implemented-by" => Record::SpecImplementedBy { from, to },
        "calls" => Record::Calls { from, to },
        "publishes" => Record::Publishes { from, to },
        "subscribes" => Record::Subscribes { from, to },
        "depends-on" => Record::DependsOn { from, to },
        "uses" => Record::Uses { from, to },
        "represents" => Record::Represents { from, to },
        "details" => Record::Details { from, to },
        other => anyhow::bail!("unknown edge kind `{other}`"),
    })
}

// ---------------------------------------------------------------------------
// SPEC §4.1/§4.2 — the durable mutation orchestrator (phase-3 task-16)
// ---------------------------------------------------------------------------

/// Read every durable node file under `apg/layers/` into a [`NodeFile`] vector
/// (no validation) — the identity universe `validate_change` and `write_project`
/// resolve writes against. The FQN is derived from the path, never read.
///
/// Public so the relocated e2e crates reach it as
/// `apg::layers::read_existing_nodes` (e.g. the `testutil` crash-durability
/// test pairs the read store with [`check_edge_pairing`]).
pub fn read_existing_nodes(apg_root: &Path) -> anyhow::Result<Vec<NodeFile>> {
    let mut nodes: Vec<NodeFile> = Vec::new();
    for (layer_dir, types) in LAYERS_TREE {
        for node_type in *types {
            let dir = apg_root.join(LAYERS_DIR).join(layer_dir).join(node_type);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "json") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                let nf: NodeFile = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("{}: bad node file: {e}", path.display()))?;
                if nf.layer != *layer_dir || nf.node_type != *node_type || nf.name != name {
                    anyhow::bail!(
                        "node file {}: layer/type/name fields must match the path segments",
                        path.display()
                    );
                }
                nodes.push(nf);
            }
        }
    }
    nodes.sort_by(|a, b| (&a.layer, &a.node_type, &a.name).cmp(&(&b.layer, &b.node_type, &b.name)));
    Ok(nodes)
}

/// Derive the (layer, type, name) identity of a node file path under
/// `apg/layers/` (the inverse of [`write_node`]'s path derivation).
fn identity_from_path(apg_root: &Path, path: &Path) -> Option<(Layer, String, String)> {
    let rel = path.strip_prefix(apg_root.join(LAYERS_DIR)).ok()?;
    let mut comps = rel.components();
    let layer_dir = comps.next()?.as_os_str().to_str()?.to_string();
    let node_type = comps.next()?.as_os_str().to_str()?.to_string();
    let name = comps.next()?.as_os_str().to_str()?.to_string();
    let name = name.strip_suffix(".json")?.to_string();
    let layer = Layer::ALL.iter().find(|l| l.layer_dir() == layer_dir)?;
    Some((*layer, node_type, name))
}

/// Validate a complete proposed mutation BEFORE anything is written (SPEC
/// §4.1 "the complete proposed change is validated before anything is
/// written"): every written node passes [`validate_node`], every written node's
/// out-edge passes [`validate_edges`], the assembled post-mutation set
/// (existing nodes minus deleted/overwritten, plus the writes) satisfies the
/// property-aware §3.3 rules ([`validate_assembled_rules`]: acyclic
/// contains/depends-on trees; `Entity (kind: event)` publishes/subscribes
/// targets) and [`check_edge_pairing`], every written constraint's
/// `attaches-to` reference resolves against the post-mutation universe
/// ([`eval_constraint`], R14), and — when the `graph.jsonl` export exists —
/// every assembled `implemented-by` target is Real or Pending against the
/// exported scanned graph ([`validate_code_refs`]; a Drift target aborts before
/// the write). Validation never opens `db.lbug`. Pure read — no write.
pub fn validate_change(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> anyhow::Result<()> {
    let mut existing: BTreeMap<String, NodeFile> = BTreeMap::new();
    for n in read_existing_nodes(apg_root)? {
        let f = fqn(layer_of(&n.layer), &n.node_type, &n.name);
        existing.insert(f, n);
    }

    // Remove the deleted files and the overwritten files from the current set.
    let mut deleted_fqns: BTreeSet<String> = BTreeSet::new();
    for path in deletes {
        if let Some((layer, node_type, name)) = identity_from_path(apg_root, path) {
            deleted_fqns.insert(fqn(layer, &node_type, &name));
        }
    }
    let written_fqns: BTreeSet<String> = writes
        .iter()
        .map(|n| fqn(layer_of(&n.layer), &n.node_type, &n.name))
        .collect();
    existing.retain(|f, _| !deleted_fqns.contains(f) && !written_fqns.contains(f));

    // The uniqueness universe: everything currently present EXCEPT the nodes
    // this change (re)writes — so an overwrite does not trip its own uniqueness.
    let mut universe: BTreeSet<(Layer, String, String)> = existing
        .keys()
        .map(|f| {
            let (l, t, n) = parse_fqn(f).expect("existing FQN must parse");
            (l, t, n)
        })
        .collect();

    // Per-node + per-edge validation over the written set (a write that also
    // appears among the deletes is contradictory — refuse).
    for n in writes {
        let layer = layer_of(&n.layer);
        validate_node(layer, &n.node_type, &n.name, &n.properties, &universe)?;
        universe.insert((layer, n.node_type.clone(), n.name.clone()));
    }
    for n in writes {
        let from = fqn(layer_of(&n.layer), &n.node_type, &n.name);
        let edges: Vec<(&str, &str, &str)> = n
            .out
            .iter()
            .map(|oe| (oe.kind.as_str(), from.as_str(), oe.target.as_str()))
            .collect();
        validate_edges(&edges)?;
    }

    // The assembled post-mutation node set, FQN → node file (a write overrides
    // the current file) — the property-aware §3.3 rules and the code-ref drift
    // check need the full set, not just the writes.
    let mut assembled: BTreeMap<String, &NodeFile> =
        existing.iter().map(|(f, n)| (f.clone(), n)).collect();
    for n in writes {
        assembled.insert(fqn(layer_of(&n.layer), &n.node_type, &n.name), n);
    }

    // SPEC §3.3 property rules over the assembled set: contains/depends-on
    // trees acyclic, and publishes/subscribes targets are Entity (kind: event).
    // Both abort before anything is written.
    validate_assembled_rules(&assembled, &universe)?;

    // Code-ref drift (SPEC §4.1): an `implemented-by` target gone from the
    // scanned graph must abort BEFORE anything is written — otherwise the
    // file lands and commits and only the step-5 re-merge fails (a committed
    // partial mutation). Decoupled from the DB: when `graph.jsonl` exists it is
    // the sole code-identity source (real code FQNs UNION the plan store's
    // planned FQNs), resolved WITHOUT opening `db.lbug`. When `graph.jsonl` is
    // absent, code-FQN refs are recorded **unvalidated** even when `db.lbug`
    // exists (deliberate: `db.lbug` is a derived projection and is never opened
    // for validation); the next scan re-validates them.
    if apg_root.join(TRANS_DIR).join("graph.jsonl").exists() {
        let (scanned, planned) = artifacts::code_universes_from_export(apg_root)?;
        let refs: Vec<&str> = assembled
            .values()
            .flat_map(|n| n.out.iter())
            .filter(|oe| oe.kind == "implemented-by")
            .map(|oe| oe.target.as_str())
            .collect();
        validate_code_refs(&refs, &scanned, &planned)?;
    }

    // Constraint reference validation (R14): a written constraint's
    // `attaches-to` must resolve against the post-mutation universe — a
    // non-thing reference is refused BEFORE anything is written. (Without
    // this, the files would land and the step-5 re-merge would fail
    // afterwards, leaving a committed partial mutation.)
    for n in writes {
        if n.node_type != "constraint" {
            continue;
        }
        let layer = layer_of(&n.layer);
        let mut own_universe = universe.clone();
        own_universe.remove(&(layer, n.node_type.clone(), n.name.clone()));
        eval_constraint(layer, &n.name, &n.properties, &own_universe)?;
    }

    // Pairwise symmetry over the assembled post-mutation set.
    let mut merged: Vec<NodeFile> = existing.into_values().collect();
    merged.extend(writes.iter().cloned());
    check_edge_pairing(&merged)?;
    Ok(())
}

/// Write a SET of node files atomically AND delete a set of node-file paths,
/// in one logical mutation (SPEC §4.1 "renames / deletions are atomic
/// write-throughs"): snapshot the prior state of every affected path, write
/// the writes, remove the deletes, and on any failure restore every path —
/// never leave mismatched endpoint files. Commits once. `write_through` (the
/// write-only primitive, task-11) delegates here with an empty delete list.
pub fn write_through_with_deletes(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> anyhow::Result<()> {
    // Structural pre-check: no plans layer, valid names, no duplicate write
    // path, and a path may not be both written and deleted.
    let mut write_paths: Vec<PathBuf> = Vec::with_capacity(writes.len());
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for node in writes {
        let layer = Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == node.layer)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
        if layer.storage() == StoragePolicy::TransientPlans {
            anyhow::bail!(
                "layer `plans` is transient (apg/.trans/plans/) — not a durable node-file layer"
            );
        }
        if !valid_name(&node.name) {
            anyhow::bail!(
                "node name `{}` is invalid — the name allowlist is [a-z0-9][a-z0-9-]* (refused, never sanitized)",
                node.name
            );
        }
        let path = apg_root
            .join(LAYERS_DIR)
            .join(layer.layer_dir())
            .join(&node.node_type)
            .join(format!("{}.json", node.name));
        if !seen.insert(path.clone()) {
            anyhow::bail!(
                "duplicate write: two entries target `{}` — one logical mutation touches each node file once",
                path.display()
            );
        }
        write_paths.push(path);
    }
    for d in deletes {
        if write_paths.contains(d) {
            anyhow::bail!(
                "a path cannot be both written and deleted in one mutation: {}",
                d.display()
            );
        }
    }

    // Serialize up front — a serialization failure is caught before any write.
    let serialized: Vec<String> = writes
        .iter()
        .map(serde_json::to_string_pretty)
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("serialize node file failed: {e}"))?;

    // Snapshot the prior state of every affected path (writes and deletes).
    let mut paths: Vec<PathBuf> = write_paths.clone();
    paths.extend(deletes.iter().cloned());
    let mut prior: Vec<Option<Vec<u8>>> = Vec::with_capacity(paths.len());
    for path in &paths {
        prior.push(std::fs::read(path).ok());
    }

    // Apply: write the writes, remove the deletes; restore everything on the
    // first failure.
    for (i, path) in write_paths.iter().enumerate() {
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!(
                "create_dir_all {} failed: {e}",
                parent.display()
            ));
        }
        if let Err(e) = std::fs::write(path, &serialized[i]) {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!("write {} failed: {e}", path.display()));
        }
    }
    for path in deletes {
        if let Err(e) = std::fs::remove_file(path) {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!("delete {} failed: {e}", path.display()));
        }
    }

    // Commit once. Outside a git repo there is no commit; a commit failure
    // rolls back so a failed mutation never leaves mismatched files.
    if git::in_repo(apg_root) {
        let write_refs: Vec<&Path> = write_paths.iter().map(|p| p.as_path()).collect();
        let delete_refs: Vec<&Path> = deletes.iter().map(|p| p.as_path()).collect();
        let all_refs: Vec<&Path> = write_refs
            .iter()
            .chain(delete_refs.iter())
            .copied()
            .collect();
        let msg = git::graph_mutation_message(apg_root, &all_refs);
        match git::commit_files(apg_root, &write_refs, &delete_refs, &msg) {
            // The durable write is now committed — the system-of-record
            // durability point. The staleness re-anchor and the projection are
            // the caller's (`write_project`), applied only AFTER this commit.
            Ok(Some(_)) | Ok(None) => {}
            Err(e) => {
                rollback(&paths, &prior);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// A test-only failure-injection point on the durable mutation path, at the
/// commit→project boundary (phase-05 task-14). The durable file write + its
/// single commit land FIRST; the projection apply follows. A test installs a
/// one-shot hook to force a failure at exactly one boundary — not a normal
/// projection error path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MutationBoundary {
    /// After validation, BEFORE the durable file write/commit: a failure here
    /// leaves the mutation in neither the durable store nor the projection.
    BeforeCommit,
    /// AFTER the durable file write + commit, BEFORE the projection apply: a
    /// failure here leaves the committed state durable while the projection
    /// stays prior — the next rebuild reproduces the committed state.
    BeforeProject,
}

type MutationHook = (MutationBoundary, Box<dyn FnOnce() -> anyhow::Result<()>>);

thread_local! {
    static MUTATION_HOOK: std::cell::RefCell<Option<MutationHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Install a one-shot hook that fires at `point` on the next `write_project`
/// in THIS thread (tests run one per thread, so a hook never leaks across
/// tests). A returned `Err` stands in for a crash at the boundary.
pub fn install_mutation_hook(
    point: MutationBoundary,
    hook: impl FnOnce() -> anyhow::Result<()> + 'static,
) {
    MUTATION_HOOK.with(|cell| *cell.borrow_mut() = Some((point, Box::new(hook))));
}

fn fire_mutation_hook(point: MutationBoundary) -> anyhow::Result<()> {
    let hook = MUTATION_HOOK.with(|cell| {
        let mut slot = cell.borrow_mut();
        match slot.as_ref() {
            Some((p, _)) if *p == point => slot.take().map(|(_, hook)| hook),
            _ => None,
        }
    });
    match hook {
        Some(hook) => hook(),
        None => Ok(()),
    }
}

/// The exact FQN delete set a durable node/edge mutation applies to the
/// projection (phase-05 tasks 4/6): every written FQN (removed ∪ changed —
/// detaching a written node drops its old incident edges, so the MERGE-only
/// re-merge cannot leave a vanished edge) plus every deleted FQN. Never a
/// whole layer-dir prefix.
fn projection_deletes(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for n in writes {
        out.insert(fqn(layer_of(&n.layer), &n.node_type, &n.name));
    }
    for path in deletes {
        if let Some((layer, node_type, name)) = identity_from_path(apg_root, path) {
            out.insert(fqn(layer, &node_type, &name));
        }
    }
    out
}

/// The injectable projection apply `write_project_with` threads through: given
/// the exact FQN delete set (removed ∪ changed) and the source records, apply
/// them to `db.lbug` in one transaction.
pub type ProjectionApply<'a> = &'a dyn Fn(&BTreeSet<String>, &[Record]) -> anyhow::Result<()>;

/// The durable-mutation orchestrator (SPEC §2.2/§4.1/§4.2): one logical
/// node/edge mutation — the full set of affected node files `writes` plus the
/// paths `deletes` to remove — guarded, validated, written atomically, and
/// re-merged into the live DB. Mirrors `artifacts::write_jsonl_and_reingest`'s
/// envelope for node files:
///
/// 1. **Membership guard** — node files have no project segment, so any
///    project context satisfies the guard (`git::require_project_context`).
/// 2. **Staleness gate** — refuse-on-stale when a DB exists, before any write.
/// 3. **Validate the complete change** ([`validate_change`]) before writing.
/// 4. **Atomic write + delete + single commit** ([`write_through_with_deletes`])
///    — the system-of-record durability point, and the ONLY step that controls
///    the flock-guaranteed one-commit-per-mutation. No durable write is ever
///    buffered: the files hit disk (and git) before anything is projected.
/// 5. **Re-anchor the staleness gate's `scan_meta`** after the commit
///    (graph.jsonl only — never opens `db.lbug`).
/// 6. **Projection delta** — apply the exact durable delta
///    ([`projection_deletes`] = removed ∪ changed FQNs, guarded for planned
///    code FQNs) to the live DB, in one transaction, AFTER the durable commit
///    (commit-then-project). The same transaction re-MERGEs the worktree's
///    transient record set ([`append_transient_records`]: the plan store + the
///    five feedback tier mirrors) so a changed-FQN DETACH cannot drop a
///    pre-existing transient pairing (`Feedback -[:Reviews]-> <node>`).
///    Skipped when no DB exists (the files are the durable form); a re-ingest
///    failure leaves the committed durable state authoritative.
pub fn write_project(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> anyhow::Result<()> {
    write_project_with(apg_root, writes, deletes, &|deletes, records| {
        artifacts::reingest_layers(apg_root, deletes, records)
    })
}

/// [`write_project`] with an injectable projection apply. The direct path uses
/// the default (open `db.lbug`, apply the delta, close); the phase-03 session
/// coordinator passes a closure that applies the delta through the DB handle it
/// already owns, so the session amortizes ONE open/parse across N mutations
/// while every mutation's projection delta still lands synchronously as the
/// mutation completes (the open is amortized, visibility never is).
pub fn write_project_with(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
    project: ProjectionApply<'_>,
) -> anyhow::Result<()> {
    // 1. Membership guard (writes only happen inside a project context).
    git::require_project_context(apg_root)?;

    // 2. Staleness gate (refuse-on-stale when a DB exists).
    if apg_root.join(TRANS_DIR).join("db.lbug").exists()
        && let Some(msg) = git::refusal_message(apg_root)
    {
        anyhow::bail!("{msg}");
    }

    // 3. Validate the complete change before anything is written.
    validate_change(apg_root, writes, deletes)?;

    // Test-only injection at the BEFORE-COMMIT boundary (phase-05 task-14,
    // window 2): a failure here must leave the mutation in neither the durable
    // store nor the projection.
    fire_mutation_hook(MutationBoundary::BeforeCommit)?;

    // 4. Atomic write + delete + single commit — the durability point. This is
    //    the whole flock-held sequence's controlled commit.
    write_through_with_deletes(apg_root, writes, deletes)?;

    // 5. Re-anchor the staleness gate's recorded scan_meta AFTER the commit
    //    (mirrors the JSONL funnel's auto-commit: DB and tree in sync by
    //    construction, so consecutive node/edge mutations never trip the
    //    refuse-on-stale gate). graph.jsonl only — no db.lbug open. A re-anchor
    //    failure degrades to a warning — the mutation already landed.
    if let Err(e) = git::reanchor_scan_meta(apg_root, &git::git_state(apg_root)) {
        eprintln!("apg: warning: could not re-anchor scan_meta after node-file commit: {e:#}");
    }

    // Test-only injection at the commit→project boundary (phase-05 task-14,
    // window 1): the durable write is committed by now; a failure here leaves
    // the projection prior and the committed state reproducible by a rebuild.
    fire_mutation_hook(MutationBoundary::BeforeProject)?;

    // 6. Projection delta — applied only AFTER the durable commit
    //    (commit-then-project). Skipped when there is no query index yet.
    if apg_root.join(TRANS_DIR).join("db.lbug").exists() {
        let graph_jsonl = apg_root.join(TRANS_DIR).join("graph.jsonl");
        let (scanned, mut planned) = artifacts::code_universes_from_export(apg_root)?;
        if !graph_jsonl.exists() {
            // No code-identity source: the mutation still lands (the durable
            // node files are authoritative), but its code-FQN refs are
            // recorded UNVALIDATED. Treat every implemented-by target as
            // pending so the projection re-merge records them instead of
            // rejecting them as drift. The next scan re-validates.
            for n in read_existing_nodes(apg_root)? {
                for oe in &n.out {
                    if oe.kind == "implemented-by" {
                        planned.insert(oe.target.clone());
                    }
                }
            }
            for n in writes {
                for oe in &n.out {
                    if oe.kind == "implemented-by" {
                        planned.insert(oe.target.clone());
                    }
                }
            }
        }
        let mut records = ingest_tree(apg_root, &scanned, &planned)?;
        // The durable tree is the only source of DURABLE records, but a durable
        // mutation detaches every changed FQN — and with it any incident
        // TRANSIENT edge. Here that is precisely `Feedback -[:Reviews]-> <node>`:
        // `detach_delete_project` DETACH-deletes the changed node, taking the
        // Reviews edge with it, and a MERGE of the durable records alone cannot
        // put it back. Append the worktree's transient record set (the plan
        // store + the five feedback tier mirrors) so the same transaction also
        // re-MERGEs it — a durable mutation then leaves any pre-existing
        // Feedback/Reviews pairing intact, with no later transient write needed.
        //
        // `.trans` is branch-local, so every file here is this project's
        // transient state; this is an idempotent MERGE (the DETACH set above is
        // durable-FQN-only), never a delete. It cannot resurrect an edge to a
        // node removed by a `node rm`: `merge_edge`'s dangling-endpoint guard
        // resolves each endpoint through `known` (this record set), the code
        // graph, then the LIVE DB (`node_label`), and skips the MERGE when
        // either endpoint is absent — a node already detached by this apply is
        // gone from the DB, so its transient edges stay gone.
        append_transient_records(apg_root, &mut records)?;
        let deletes = projection_deletes(apg_root, writes, deletes);
        project(&deletes, &records)?;
    }
    Ok(())
}

/// Appends the worktree's transient record set to `records`: the plan store
/// (`.trans/plans/*.jsonl`) plus the five feedback tier mirrors
/// (`.trans/<tier>/*.jsonl`), via the public enumerators
/// [`specs::plan_files`](crate::specs::plan_files) /
/// [`specs::trans_mirror_files`](crate::specs::trans_mirror_files). Used by
/// [`write_project_with`]'s projection apply so a durable mutation re-MERGEs
/// the transient nodes/edges (notably `Feedback -[:Reviews]-> <durable node>`)
/// that its changed-FQN DETACH would otherwise drop. Errors loudly on a
/// malformed transient file (never a silent skip).
fn append_transient_records(apg_root: &Path, records: &mut Vec<Record>) -> anyhow::Result<()> {
    for path in crate::specs::plan_files(apg_root)
        .into_iter()
        .chain(crate::specs::trans_mirror_files(apg_root))
    {
        records.extend(crate::specs::read_jsonl(&path)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
