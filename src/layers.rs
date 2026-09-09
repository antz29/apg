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
/// the change universe; the FQN *builder* is phase-3 task-8.
fn parse_fqn(fqn: &str) -> anyhow::Result<(Layer, String, String)> {
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
// (Unused until write_project, phase-3 task-16, calls it.)
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is the SPEC §3.1 table: per-layer dirs and node types,
    /// verbatim (global holds constraint before note; implementation holds
    /// only the attach-only pair).
    #[test]
    fn catalog_matches_the_spec_table() {
        let rows: Vec<(&str, &[&str])> = Layer::ALL
            .into_iter()
            .map(|l| (l.layer_dir(), l.node_types()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "requirements",
                    &["stakeholder", "user", "requirement", "note", "constraint"][..]
                ),
                (
                    "domain",
                    &["group", "entity", "value", "service", "note", "constraint"][..]
                ),
                (
                    "solution",
                    &[
                        "system",
                        "container",
                        "component",
                        "person",
                        "note",
                        "constraint"
                    ][..]
                ),
                (
                    "plans",
                    &["plan-phase", "task", "module", "file", "struct", "function"][..]
                ),
                ("implementation", &["note", "constraint"][..]),
                ("global", &["constraint", "note"][..]),
            ]
        );
    }

    /// Storage policy (SPEC §3.1): the four file-backed layers are durable,
    /// plans is the only transient layer (`.trans/plans/` only, per branch),
    /// implementation is scanned code.
    #[test]
    fn storage_policy_marks_only_plans_transient() {
        for l in Layer::ALL {
            match l.storage() {
                StoragePolicy::Durable => assert!(
                    matches!(
                        l,
                        Layer::Requirements | Layer::Domain | Layer::Solution | Layer::Global
                    ),
                    "{l:?} must not be durable"
                ),
                StoragePolicy::TransientPlans => {
                    assert_eq!(l, Layer::Plans, "only plans is transient")
                }
                StoragePolicy::ScannedCode => {
                    assert_eq!(
                        l,
                        Layer::Implementation,
                        "only implementation is scanned code"
                    )
                }
            }
        }
    }

    /// Implementation holds exactly the attach-only types; global holds the
    /// laws (constraint) and the notes on them.
    #[test]
    fn implementation_is_attach_only() {
        assert_eq!(Layer::Implementation.storage(), StoragePolicy::ScannedCode);
        assert_eq!(Layer::Implementation.node_types(), &["note", "constraint"]);
        assert_eq!(Layer::Global.node_types(), &["constraint", "note"]);
    }

    /// The durable tree constant is exactly the catalog projection over the
    /// non-transient layers (plans has no durable dir; implementation's
    /// attach-only dirs are durable) — the durable dir list matches the
    /// layout constants, in layout order.
    #[test]
    fn layers_tree_rows_match_the_catalog() {
        let durable: Vec<(&str, &[&str])> = Layer::ALL
            .into_iter()
            .filter(|l| l.storage() != StoragePolicy::TransientPlans)
            .map(|l| (l.layer_dir(), l.node_types()))
            .collect();
        assert_eq!(LAYERS_TREE, durable.as_slice());
    }

    /// The `.trans` mirrors are complete: all six tiers in the §4.1 diagram
    /// order — plans first, then the five feedback mirrors incl. global —
    /// with unique dirs.
    #[test]
    fn trans_mirrors_cover_all_six_layers() {
        assert_eq!(
            TRANS_MIRRORS,
            [
                Layer::Plans,
                Layer::Requirements,
                Layer::Domain,
                Layer::Solution,
                Layer::Implementation,
                Layer::Global,
            ]
        );
        let dirs: Vec<&str> = TRANS_MIRRORS.iter().map(|l| l.layer_dir()).collect();
        let mut unique = dirs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), dirs.len(), "mirror tier dirs must be unique");
    }

    /// The SPEC §3.3 name allowlist `[a-z0-9][a-z0-9-]*`: lowercase letters,
    /// digits, and hyphens (not first); a leading digit and a trailing hyphen
    /// are fine. Refuse — never sanitize.
    #[test]
    fn name_allowlist_refuses_invalid_names() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for name in [
            "customer",
            "a",
            "0",
            "order-line",
            "a-b-c",
            "123",
            "line-",
            "x0",
        ] {
            assert!(
                validate_node(Layer::Domain, "value", name, &props, &empty).is_ok(),
                "{name} must pass the allowlist"
            );
        }
        for name in [
            "CamelCase",
            "with.dot",
            "with space",
            "with_underscore",
            "-lead",
            "",
            "UPPER",
        ] {
            assert!(
                validate_node(Layer::Domain, "value", name, &props, &empty).is_err(),
                "{name} must be refused (never sanitized)"
            );
        }
    }

    /// Type must exist in its layer (SPEC §3.3) — case-sensitive exact match
    /// against the catalog's lowercase type spellings. The cryptic DDD names
    /// are not types (SPEC §3.1 collapses them into Group).
    #[test]
    fn type_must_exist_in_its_layer() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for bad in [
            "aggregate",
            "bounded-context",
            "subdomain",
            "domain-rule",
            "domain-process",
        ] {
            let err = validate_node(Layer::Domain, bad, "x", &props, &empty).unwrap_err();
            assert!(
                err.to_string().contains("does not exist in layer"),
                "{bad}: {err}"
            );
        }
        // Case-sensitive: the catalog spells types lowercase.
        assert!(validate_node(Layer::Domain, "Entity", "x", &props, &empty).is_err());
        // A type of another layer is refused here.
        assert!(validate_node(Layer::Domain, "stakeholder", "x", &props, &empty).is_err());
        // The exact lowercase spelling passes the type check; the Entity kind
        // rule is then what fires.
        let err = validate_node(Layer::Domain, "entity", "customer", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("requires"), "{err}");
    }

    /// Entity requires `kind` ∈ {entity, event} — the spec's only "requires".
    #[test]
    fn entity_kind_is_required_and_validated() {
        let empty = BTreeSet::new();
        let err = validate_node(
            Layer::Domain,
            "entity",
            "customer",
            &BTreeMap::new(),
            &empty,
        )
        .unwrap_err();
        assert!(err.to_string().contains("requires"), "{err}");
        for kind in ["entity", "event"] {
            let props = BTreeMap::from([("kind".to_string(), kind.to_string())]);
            assert!(
                validate_node(Layer::Domain, "entity", "customer", &props, &empty).is_ok(),
                "kind {kind} must be accepted"
            );
        }
        let props = BTreeMap::from([("kind".to_string(), "thing".to_string())]);
        let err = validate_node(Layer::Domain, "entity", "customer", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("entity|event"), "{err}");
    }

    /// Group: `attribute` ∈ {core, supporting, generic} and `root` are both
    /// optional; when present they are validated — root against the name
    /// allowlist, independent of the attribute.
    #[test]
    fn group_attribute_and_root_optional_but_validated() {
        let empty = BTreeSet::new();
        // No properties at all — valid.
        assert!(validate_node(Layer::Domain, "group", "sales", &BTreeMap::new(), &empty).is_ok());
        for attr in ["core", "supporting", "generic"] {
            let props = BTreeMap::from([("attribute".to_string(), attr.to_string())]);
            assert!(
                validate_node(Layer::Domain, "group", "sales", &props, &empty).is_ok(),
                "attribute {attr} must be accepted"
            );
        }
        let props = BTreeMap::from([("attribute".to_string(), "dubious".to_string())]);
        let err = validate_node(Layer::Domain, "group", "sales", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("core|supporting|generic"), "{err}");
        // root is independent of attribute and format-validated.
        let props = BTreeMap::from([("root".to_string(), "sales-root".to_string())]);
        assert!(validate_node(Layer::Domain, "group", "sales", &props, &empty).is_ok());
        let props = BTreeMap::from([("root".to_string(), "SalesRoot".to_string())]);
        let err = validate_node(Layer::Domain, "group", "sales", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("allowlist"), "{err}");
    }

    /// Container `kind` ∈ {app, service, db, queue} — validated when present,
    /// optional otherwise (SPEC §3.1 "takes").
    #[test]
    fn container_kind_validated_when_present() {
        let empty = BTreeSet::new();
        assert!(
            validate_node(
                Layer::Solution,
                "container",
                "api",
                &BTreeMap::new(),
                &empty
            )
            .is_ok()
        );
        for kind in ["app", "service", "db", "queue"] {
            let props = BTreeMap::from([("kind".to_string(), kind.to_string())]);
            assert!(
                validate_node(Layer::Solution, "container", "api", &props, &empty).is_ok(),
                "kind {kind} must be accepted"
            );
        }
        let props = BTreeMap::from([("kind".to_string(), "web".to_string())]);
        let err = validate_node(Layer::Solution, "container", "api", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("app|service|db|queue"), "{err}");
    }

    /// Unknown property keys are metadata — allowed, never identity (SPEC
    /// §4.1: short ids "may exist as metadata only"); a known key on a type
    /// that does not take it is left alone too.
    #[test]
    fn unknown_metadata_keys_are_allowed() {
        let empty = BTreeSet::new();
        let props = BTreeMap::from([
            ("id".to_string(), "R1".to_string()),
            ("kind".to_string(), "thing".to_string()),
        ]);
        assert!(validate_node(Layer::Requirements, "requirement", "login", &props, &empty).is_ok());
    }

    /// Names are unique per (layer, type) — the same name in two different
    /// types (or layers) is fine; a duplicate in the same (layer, type) is
    /// refused.
    #[test]
    fn names_unique_per_layer_and_type() {
        let existing: BTreeSet<(Layer, String, String)> =
            BTreeSet::from([(Layer::Domain, "entity".to_string(), "customer".to_string())]);
        let props = BTreeMap::from([("kind".to_string(), "entity".to_string())]);
        // Same name, different type in the same layer — fine.
        assert!(validate_node(Layer::Domain, "value", "customer", &props, &existing).is_ok());
        // Same name, different layer — fine.
        assert!(validate_node(Layer::Solution, "component", "customer", &props, &existing).is_ok());
        // Same (layer, type, name) — refused.
        let err =
            validate_node(Layer::Domain, "entity", "customer", &props, &existing).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    /// contains/depends-on trees must be acyclic (SPEC §3.3). The helper
    /// validates the per-change edge set; every endpoint must resolve into
    /// the change universe (existing nodes plus co-proposed nodes).
    #[test]
    fn tree_acyclicity_refuses_contains_and_depends_on_cycles() {
        let universe: BTreeSet<(Layer, String, String)> = BTreeSet::from([
            (
                Layer::Requirements,
                "requirement".to_string(),
                "r1".to_string(),
            ),
            (
                Layer::Requirements,
                "requirement".to_string(),
                "r2".to_string(),
            ),
            (
                Layer::Requirements,
                "requirement".to_string(),
                "r3".to_string(),
            ),
            (Layer::Domain, "group".to_string(), "g1".to_string()),
            (Layer::Domain, "group".to_string(), "g2".to_string()),
        ]);
        // A contains cycle (Group nesting) is refused.
        let contains_cycle: &[(&str, &str)] = &[
            ("domain.group.g1", "domain.group.g2"),
            ("domain.group.g2", "domain.group.g1"),
        ];
        let err = validate_trees_acyclic(contains_cycle, &universe).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        // A depends-on cycle (Requirement) is refused.
        let depends_cycle: &[(&str, &str)] = &[
            ("requirements.requirement.r1", "requirements.requirement.r2"),
            ("requirements.requirement.r2", "requirements.requirement.r3"),
            ("requirements.requirement.r3", "requirements.requirement.r1"),
        ];
        let err = validate_trees_acyclic(depends_cycle, &universe).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        // A self-loop is a cycle too ("a node cannot contain itself").
        let self_loop: &[(&str, &str)] =
            &[("requirements.requirement.r1", "requirements.requirement.r1")];
        assert!(validate_trees_acyclic(self_loop, &universe).is_err());
        // A DAG passes.
        let dag: &[(&str, &str)] = &[
            ("requirements.requirement.r1", "requirements.requirement.r2"),
            ("requirements.requirement.r2", "requirements.requirement.r3"),
        ];
        assert!(validate_trees_acyclic(dag, &universe).is_ok());
        // A dangling endpoint is refused (write-time error, SPEC §3.3).
        let dangling: &[(&str, &str)] = &[(
            "requirements.requirement.r1",
            "requirements.requirement.nope",
        )];
        let err = validate_trees_acyclic(dangling, &universe).unwrap_err();
        assert!(
            err.to_string().contains("not a node of the change"),
            "{err}"
        );
    }

    /// Every §3.3 edge kind accepts at least one valid (source, target) shape:
    /// each matrix row, both code-exempt kinds (`implemented-by` with a code
    /// FQN target, `details` from a note in every authoring layer to any
    /// target), and the `represents` pair.
    #[test]
    fn every_edge_kind_accepts_a_valid_shape() {
        let ok: &[(&str, &str, &str)] = &[
            (
                "contains",
                "requirements.stakeholder.s1",
                "requirements.requirement.r1",
            ),
            (
                "contains",
                "requirements.user.u1",
                "requirements.requirement.r1",
            ),
            (
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("contains", "domain.group.g1", "domain.group.g2"),
            ("contains", "domain.group.g1", "domain.entity.e1"),
            ("contains", "domain.group.g1", "domain.value.v1"),
            ("contains", "domain.group.g1", "domain.service.svc1"),
            ("contains", "solution.system.sys1", "solution.container.c1"),
            (
                "contains",
                "solution.container.c1",
                "solution.component.cmp1",
            ),
            ("drives", "requirements.requirement.r1", "domain.group.g1"),
            ("drives", "requirements.requirement.r1", "domain.entity.e1"),
            ("drives", "requirements.requirement.r1", "domain.value.v1"),
            (
                "drives",
                "requirements.requirement.r1",
                "domain.service.svc1",
            ),
            ("realised-by", "domain.group.g1", "solution.system.sys1"),
            ("realised-by", "domain.entity.e1", "solution.container.c1"),
            (
                "realised-by",
                "domain.service.svc1",
                "solution.component.cmp1",
            ),
            (
                "implemented-by",
                "solution.system.sys1",
                "apg.artifacts.write_jsonl_and_reingest",
            ),
            (
                "implemented-by",
                "solution.container.c1",
                "apg.layers.validate_edges",
            ),
            ("implemented-by", "solution.component.cmp1", "apg.main"),
            ("calls", "domain.service.svc1", "domain.service.svc2"),
            ("publishes", "domain.service.svc1", "domain.entity.e1"),
            ("subscribes", "domain.service.svc1", "domain.entity.e1"),
            (
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("uses", "solution.person.p1", "solution.system.sys1"),
            ("represents", "requirements.user.u1", "domain.entity.e1"),
            ("represents", "domain.entity.e1", "solution.person.p1"),
            (
                "details",
                "requirements.note.n1",
                "requirements.requirement.r1",
            ),
            ("details", "domain.note.n1", "domain.entity.e1"),
            ("details", "solution.note.n1", "solution.system.sys1"),
            (
                "details",
                "implementation.note.n1",
                "requirements.requirement.r1",
            ),
            ("details", "global.note.n1", "requirements.requirement.r1"),
        ];
        for &(kind, src, dst) in ok {
            assert!(
                validate_edge(kind, src, dst).is_ok(),
                "{kind} {src} -> {dst} must be accepted"
            );
        }
    }

    /// A rejected (kind, source, target) shape errors naming the kind, the
    /// source, and the target — tier skips and wrong-type shapes included.
    #[test]
    fn rejected_shapes_name_kind_source_and_target() {
        let bad: &[(&str, &str, &str)] = &[
            // calls from an Entity (not Service).
            ("calls", "domain.entity.e1", "domain.service.svc1"),
            // drives from a Group — a tier skip (Domain → Solution).
            ("drives", "domain.group.g1", "solution.system.sys1"),
            // contains from a Container to a Requirement — a tier skip.
            (
                "contains",
                "solution.container.c1",
                "requirements.requirement.r1",
            ),
            // depends-on from a Domain entity.
            (
                "depends-on",
                "domain.entity.e1",
                "requirements.requirement.r1",
            ),
            // realised-by from a Requirement — a tier skip.
            (
                "realised-by",
                "requirements.requirement.r1",
                "solution.system.sys1",
            ),
            // uses from a Service (must be Person).
            ("uses", "domain.service.svc1", "solution.system.sys1"),
            // publishes from a Group (must be Service).
            ("publishes", "domain.group.g1", "domain.entity.e1"),
            // subscribes to a Group (target must be an Entity).
            ("subscribes", "domain.service.svc1", "domain.group.g1"),
            // represents Entity → System (must be Entity → Person).
            ("represents", "domain.entity.e1", "solution.system.sys1"),
        ];
        for &(kind, src, dst) in bad {
            let msg = validate_edge(kind, src, dst).unwrap_err().to_string();
            assert!(msg.contains(kind), "must name kind: {msg}");
            assert!(msg.contains(src), "must name source: {msg}");
            assert!(msg.contains(dst), "must name target: {msg}");
        }
    }

    /// `implemented-by` accepts a code-FQN target (exempt — `validate_code_refs`
    /// owns it, task-10) and refuses any non-Solution source.
    #[test]
    fn implemented_by_accepts_code_fqn_target_and_refuses_non_solution_source() {
        for src in [
            "solution.system.payments",
            "solution.container.api",
            "solution.component.checkout",
        ] {
            assert!(
                validate_edge(
                    "implemented-by",
                    src,
                    "apg.artifacts.write_jsonl_and_reingest"
                )
                .is_ok(),
                "{src} must be a valid implemented-by source"
            );
        }
        for src in [
            "domain.service.checkout",
            "requirements.requirement.r1",
            "domain.entity.customer",
            "solution.person.p1",
        ] {
            let msg = validate_edge("implemented-by", src, "apg.main")
                .unwrap_err()
                .to_string();
            assert!(msg.contains("implemented-by"), "{msg}");
            assert!(msg.contains(src), "{msg}");
        }
    }

    /// `details` accepts any target — authored OR code — and enforces that the
    /// source is a `note` (spanning every authoring layer).
    #[test]
    fn details_accepts_any_target_and_enforces_note_source() {
        for target in [
            "requirements.requirement.r1",
            "domain.entity.e1",
            "solution.system.sys1",
            // A code FQN target is exempt (not parsed).
            "apg.artifacts.write_jsonl_and_reingest",
        ] {
            assert!(
                validate_edge("details", "requirements.note.n1", target).is_ok(),
                "details target {target} must be accepted"
            );
        }
        for src in [
            "requirements.note.n1",
            "domain.note.n1",
            "solution.note.n1",
            "implementation.note.n1",
            "global.note.n1",
        ] {
            assert!(
                validate_edge("details", src, "requirements.requirement.r1").is_ok(),
                "{src} must be a valid details source"
            );
        }
        for src in [
            "requirements.requirement.r1",
            "domain.entity.e1",
            "solution.system.sys1",
        ] {
            let msg = validate_edge("details", src, "requirements.requirement.r1")
                .unwrap_err()
                .to_string();
            assert!(msg.contains("details"), "{msg}");
            assert!(msg.contains(src), "{msg}");
        }
    }

    /// A dangling authored endpoint — malformed FQN or unknown layer — is a
    /// write-time error for every matrix kind (SPEC §3.3). Code FQNs only pass
    /// through the exempt endpoints (`implemented-by`/`details` targets).
    #[test]
    fn dangling_authored_endpoint_is_a_write_time_error() {
        // Unknown layer in the source.
        let msg = validate_edge("drives", "banana.type.name", "domain.group.g1")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("banana"), "{msg}");
        // Unknown layer in the target.
        let msg = validate_edge("drives", "requirements.requirement.r1", "banana.type.name")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("banana"), "{msg}");
        // Malformed FQN (not <layer>.<type>.<name>) on a matrix-kind endpoint.
        let msg = validate_edge(
            "contains",
            "requirements.requirement",
            "requirements.requirement.r1",
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("FQN"), "{msg}");
        // A code FQN in a non-exempt endpoint (drives target) is a dangling ref.
        let msg = validate_edge("drives", "requirements.requirement.r1", "apg.main")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("apg.main"), "{msg}");
    }

    /// Anything outside the §3.3 durable list — §5 plan/feedback edges
    /// (`reviews`, `gates`, `satisfies`, task verbs) and gibberish — is refused
    /// as an unknown edge kind.
    #[test]
    fn unknown_edge_kind_is_refused() {
        for kind in [
            "reviews",
            "gates",
            "satisfies",
            "creates",
            "modifies",
            "deletes",
            "banana",
        ] {
            let msg = validate_edge(
                kind,
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            )
            .unwrap_err()
            .to_string();
            assert!(msg.contains("unknown edge kind"), "{msg}");
            assert!(msg.contains(kind), "{msg}");
        }
    }

    /// `represents` has exactly the two matrix rows — User → Entity and Entity
    /// → Person; an Entity → System hop is refused (that would be a tier skip).
    #[test]
    fn represents_both_rows_and_refuses_tier_skips() {
        assert!(validate_edge("represents", "requirements.user.u1", "domain.entity.e1").is_ok());
        assert!(validate_edge("represents", "domain.entity.e1", "solution.person.p1").is_ok());
        let msg = validate_edge("represents", "domain.entity.e1", "solution.system.sys1")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("represents"), "{msg}");
        assert!(msg.contains("domain.entity.e1"), "{msg}");
        assert!(msg.contains("solution.system.sys1"), "{msg}");
    }

    /// The batch entry point loops [`validate_edge`] over every edge; any bad
    /// edge in the set fails the whole batch.
    #[test]
    fn validate_edges_batch_loops_validate_edge() {
        let ok: &[(&str, &str, &str)] = &[
            (
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("drives", "requirements.requirement.r1", "domain.group.g1"),
        ];
        assert!(validate_edges(ok).is_ok());
        let bad: &[(&str, &str, &str)] = &[
            (
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("drives", "domain.group.g1", "solution.system.sys1"),
        ];
        let msg = validate_edges(bad).unwrap_err().to_string();
        assert!(msg.contains("drives"), "{msg}");
    }

    /// The plain-named domain types are accepted: the catalog's `group`,
    /// `entity`, `value`, `service` (SPEC §3.1 — the cryptic DDD type names
    /// collapse into these; their refusal is covered in
    /// [`type_must_exist_in_its_layer`]). A plain-named node validates
    /// end-to-end with its own type's kind/attribute rule applied.
    #[test]
    fn plain_domain_types_accepted_with_plain_names() {
        let empty = BTreeSet::new();
        // group/value/service need no properties and pass untouched.
        for (node_type, name) in [
            ("group", "order-fulfilment"),
            ("value", "money"),
            ("service", "checkout"),
        ] {
            assert!(
                validate_node(Layer::Domain, node_type, name, &BTreeMap::new(), &empty).is_ok(),
                "plain {node_type} `{name}` must be accepted"
            );
        }
        // Entity is accepted once its required kind is present.
        let props = BTreeMap::from([("kind".to_string(), "entity".to_string())]);
        assert!(validate_node(Layer::Domain, "entity", "customer", &props, &empty).is_ok());
    }

    /// A `requirement` node is the ONE requirement type at every depth (SPEC
    /// §3.1: theme/epic/story/feature are not types — everything is
    /// `requirement`); a four-level requirement tree validates acyclically,
    /// and `contains`/`depends-on` between requirement nodes are both
    /// matrix-legal at any depth.
    #[test]
    fn requirement_tree_at_all_depths_is_one_node_type() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for pseudo in ["theme", "epic", "story", "feature"] {
            let err = validate_node(Layer::Requirements, pseudo, "x", &props, &empty).unwrap_err();
            assert!(
                err.to_string().contains("does not exist in layer"),
                "{pseudo}: {err}"
            );
        }
        let universe: BTreeSet<(Layer, String, String)> = ["r1", "r2", "r3", "r4"]
            .into_iter()
            .map(|n| {
                (
                    Layer::Requirements,
                    "requirement".to_string(),
                    n.to_string(),
                )
            })
            .collect();
        // Four levels of contains, plus a depends-on cross-link — still a DAG.
        let contains: &[(&str, &str)] = &[
            ("requirements.requirement.r1", "requirements.requirement.r2"),
            ("requirements.requirement.r2", "requirements.requirement.r3"),
            ("requirements.requirement.r3", "requirements.requirement.r4"),
        ];
        let depends: &[(&str, &str)] =
            &[("requirements.requirement.r1", "requirements.requirement.r4")];
        let all: Vec<(&str, &str)> = contains
            .iter()
            .copied()
            .chain(depends.iter().copied())
            .collect();
        assert!(
            validate_trees_acyclic(&all, &universe).is_ok(),
            "a four-level requirement tree must stay acyclic"
        );
        for &(src, dst) in contains {
            assert!(
                validate_edge("contains", src, dst).is_ok(),
                "contains {src} -> {dst} must be matrix-legal"
            );
        }
        for &(src, dst) in depends {
            assert!(
                validate_edge("depends-on", src, dst).is_ok(),
                "depends-on {src} -> {dst} must be matrix-legal"
            );
        }
    }

    /// The sequential spine (SPEC §3.2) is tier-locked by the matrix: the
    /// full legal chain Requirement --drives--> Domain --realised-by-->
    /// Solution --implemented-by--> code passes hop-by-hop, and a tier skip
    /// (Requirement directly realised-by a Solution — no Domain hop) is
    /// refused.
    #[test]
    fn sequential_spine_is_tier_locked() {
        // The full, legal spine from Requirement to code.
        assert!(validate_edge("drives", "requirements.requirement.r1", "domain.group.g1").is_ok());
        assert!(validate_edge("realised-by", "domain.group.g1", "solution.system.sys1").is_ok());
        assert!(validate_edge("implemented-by", "solution.system.sys1", "apg.main").is_ok());
        // A tier skip: Requirement realised-by Solution, skipping the Domain hop.
        let msg = validate_edge(
            "realised-by",
            "requirements.requirement.r1",
            "solution.system.sys1",
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("realised-by"), "{msg}");
        assert!(msg.contains("requirements.requirement.r1"), "{msg}");
        assert!(msg.contains("solution.system.sys1"), "{msg}");
    }

    /// The §3.3 edge-kind matrix is complete and internally consistent: its
    /// rows cover exactly the two-authored-endpoint kinds (all of
    /// [`EDGE_KINDS`] except the code-exempt `implemented-by`/`details`), and
    /// every row's source/target type is a real type of its layer's catalog.
    #[test]
    fn edge_kind_matrix_is_complete_and_consistent() {
        let exempt = ["implemented-by", "details"];
        let kinds: BTreeSet<&str> = MATRIX.iter().map(|(k, ..)| *k).collect();
        let expected: BTreeSet<&str> = EDGE_KINDS
            .iter()
            .copied()
            .filter(|k| !exempt.contains(k))
            .collect();
        assert_eq!(
            kinds, expected,
            "matrix rows must cover exactly the non-exempt edge kinds"
        );
        for &(kind, sl, sts, tl, tts) in MATRIX {
            for t in sts {
                assert!(
                    sl.node_types().contains(t),
                    "{kind}: source type `{t}` not in layer {}",
                    sl.layer_dir()
                );
            }
            for t in tts {
                assert!(
                    tl.node_types().contains(t),
                    "{kind}: target type `{t}` not in layer {}",
                    tl.layer_dir()
                );
            }
        }
    }

    /// Two services that call each other are coupled: their owning groups are
    /// one derived pair — symmetric, nothing stored (SPEC §3.1).
    #[test]
    fn mutual_calls_couple_their_groups() {
        let edges: &[(&str, &str, &str)] = &[
            ("calls", "domain.service.orders", "domain.service.billing"),
            ("calls", "domain.service.billing", "domain.service.orders"),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.billing".to_string(),
                "domain.group.billing".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        assert_eq!(
            coupled,
            BTreeSet::from([(
                "domain.group.billing".to_string(),
                "domain.group.sales".to_string()
            )])
        );
    }

    /// A publisher and a subscriber sharing one event are coupled through the
    /// event medium; the event itself names no coupled group.
    #[test]
    fn publish_subscribe_via_shared_event_couples() {
        let edges: &[(&str, &str, &str)] = &[
            (
                "publishes",
                "domain.service.orders",
                "domain.entity.order-placed",
            ),
            (
                "subscribes",
                "domain.service.shipping",
                "domain.entity.order-placed",
            ),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.shipping".to_string(),
                "domain.group.fulfilment".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        assert_eq!(
            coupled,
            BTreeSet::from([(
                "domain.group.fulfilment".to_string(),
                "domain.group.sales".to_string()
            )])
        );
    }

    /// A calls chain A→B→C couples A and C — coupling is the transitive
    /// closure, every pair in the component.
    #[test]
    fn transitive_call_chain_couples_endpoints() {
        let edges: &[(&str, &str, &str)] = &[
            (
                "calls",
                "domain.service.checkout",
                "domain.service.inventory",
            ),
            (
                "calls",
                "domain.service.inventory",
                "domain.service.payments",
            ),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.checkout".to_string(),
                "domain.group.storefront".to_string(),
            ),
            (
                "domain.service.inventory".to_string(),
                "domain.group.stock".to_string(),
            ),
            (
                "domain.service.payments".to_string(),
                "domain.group.money".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        // All three groups are pairwise coupled (3 unordered pairs).
        assert_eq!(
            coupled,
            BTreeSet::from([
                (
                    "domain.group.money".to_string(),
                    "domain.group.stock".to_string()
                ),
                (
                    "domain.group.money".to_string(),
                    "domain.group.storefront".to_string()
                ),
                (
                    "domain.group.stock".to_string(),
                    "domain.group.storefront".to_string()
                ),
            ])
        );
    }

    /// Two services with no connecting edge chain are NOT coupled —
    /// disconnected components yield no cross-component pair.
    #[test]
    fn unconnected_services_are_not_coupled() {
        let edges: &[(&str, &str, &str)] = &[
            ("calls", "domain.service.orders", "domain.service.billing"),
            ("calls", "domain.service.catalog", "domain.service.search"),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.billing".to_string(),
                "domain.group.billing".to_string(),
            ),
            (
                "domain.service.catalog".to_string(),
                "domain.group.catalog".to_string(),
            ),
            (
                "domain.service.search".to_string(),
                "domain.group.search".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        assert_eq!(
            coupled,
            BTreeSet::from([
                (
                    "domain.group.billing".to_string(),
                    "domain.group.sales".to_string()
                ),
                (
                    "domain.group.catalog".to_string(),
                    "domain.group.search".to_string()
                ),
            ])
        );
    }

    /// Coupling is DERIVED: the result is only group-FQN pairs — no edge
    /// kinds, no flavor values, no coupling node. The context-map flavor rides
    /// on the edge (an attribute the caller keeps), never in the derived
    /// structure.
    #[test]
    fn coupling_is_derived_and_flavor_is_not_structure() {
        let edges: &[(&str, &str, &str)] = &[
            ("calls", "domain.service.orders", "domain.service.billing"),
            (
                "publishes",
                "domain.service.orders",
                "domain.entity.order-placed",
            ),
            (
                "subscribes",
                "domain.service.shipping",
                "domain.entity.order-placed",
            ),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.billing".to_string(),
                "domain.group.billing".to_string(),
            ),
            (
                "domain.service.shipping".to_string(),
                "domain.group.fulfilment".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        // Three groups, all pairwise coupled through the call + the shared
        // event — but as DERIVED pairs, nothing stored.
        assert_eq!(coupled.len(), 3);
        for (a, b) in &coupled {
            // Every endpoint is a group FQN.
            assert!(a.starts_with("domain.group."), "{a}");
            assert!(b.starts_with("domain.group."), "{b}");
            // No flavor (edge attribute) leaks into the derived pair.
            for flavor in ["direct", "published", "translated", "shared", "coevolving"] {
                assert!(!a.contains(flavor), "flavor {flavor} leaked into {a}");
                assert!(!b.contains(flavor), "flavor {flavor} leaked into {b}");
            }
        }
    }

    /// Coupling is derived, never stored as a Group->Group edge: the §3.3
    /// matrix has no coupling row between groups — no coupling verb is an edge
    /// kind, the coupling edges (`calls`/`publishes`/`subscribes`) touch only
    /// Service/Entity, and the one Group->Group row (`contains`) is containment
    /// (nesting), not coupling.
    #[test]
    fn coupling_is_never_a_group_to_group_edge() {
        // No coupling verb is a durable edge kind — coupling is derived, not an
        // edge kind and not a stored artifact.
        for verb in ["couples", "coupled", "coupling", "coupled-to"] {
            assert!(
                !EDGE_KINDS.contains(&verb),
                "`{verb}` must not be an edge kind — coupling is derived, never stored"
            );
        }
        for &(kind, _, sts, _, tts) in MATRIX {
            let src_is_group = sts.contains(&"group");
            let dst_is_group = tts.contains(&"group");
            // The coupling edges never touch a Group endpoint (they are
            // Service->Service / Service->Entity).
            if matches!(kind, "calls" | "publishes" | "subscribes") {
                assert!(
                    !src_is_group && !dst_is_group,
                    "coupling edge `{kind}` must have no Group endpoint"
                );
            }
            // The only Group->Group row is `contains` (nesting).
            if src_is_group && dst_is_group {
                assert_eq!(
                    kind, "contains",
                    "Group->Group must be containment, never `{kind}`"
                );
            }
        }
    }

    /// The context-map flavor (direct/published/translated/shared/coevolving)
    /// is an EDGE attribute — never a node type, never an edge kind. A type
    /// named after a flavor is refused (it is not a catalog type); a flavor
    /// used as an edge kind is refused (unknown kind); the edge verb is
    /// `publishes`, not the flavor `published`.
    #[test]
    fn flavors_are_edge_attributes_never_types_or_kinds() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for flavor in ["direct", "published", "translated", "shared", "coevolving"] {
            // Not a node type — validate_node refuses a type named after a flavor.
            let err = validate_node(Layer::Domain, flavor, "x", &props, &empty).unwrap_err();
            assert!(
                err.to_string().contains("does not exist in layer"),
                "flavor `{flavor}` as a node type: {err}"
            );
            // Not an edge kind — validate_edge refuses a flavor used as a kind.
            let err = validate_edge(flavor, "domain.service.a", "domain.service.b")
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("unknown edge kind"),
                "flavor `{flavor}` as an edge kind: {err}"
            );
        }
        // The edge verb is `publishes`, not the flavor `published`.
        assert!(validate_edge("publishes", "domain.service.a", "domain.entity.e").is_ok());
    }

    /// A global constraint (layer Global) is well-formed with no attachment: it
    /// guards the whole graph, so it carries nothing but prose.
    #[test]
    fn global_constraint_needs_no_attachment() {
        let empty = BTreeSet::new();
        assert!(eval_constraint(Layer::Global, "g1", &BTreeMap::new(), &empty).is_ok());
    }

    /// A local constraint (requirements/domain/solution) is well-formed when
    /// its `attaches-to` resolves to an existing tier-1–3 node; a local
    /// constraint without an attachment is also fine ("may attach").
    #[test]
    fn local_constraint_with_resolving_attachment_is_well_formed() {
        let existing: BTreeSet<(Layer, String, String)> = BTreeSet::from([
            (
                Layer::Requirements,
                "requirement".to_string(),
                "place-order".to_string(),
            ),
            (Layer::Domain, "entity".to_string(), "customer".to_string()),
            (
                Layer::Solution,
                "system".to_string(),
                "payments".to_string(),
            ),
        ]);
        for (layer, target) in [
            (
                Layer::Requirements,
                "requirements.requirement.place-order".to_string(),
            ),
            (Layer::Domain, "domain.entity.customer".to_string()),
            (Layer::Solution, "solution.system.payments".to_string()),
        ] {
            let props = BTreeMap::from([(PROP_ATTACHES_TO.to_string(), target.clone())]);
            assert!(
                eval_constraint(layer, "law", &props, &existing).is_ok(),
                "{layer:?} attaching to {target} must validate"
            );
        }
        // A local constraint without an attachment is fine ("may attach").
        assert!(eval_constraint(Layer::Domain, "law", &BTreeMap::new(), &existing).is_ok());
    }

    /// A local constraint referencing a non-existent node is refused — "never a
    /// non-thing": the reference must resolve, not merely parse.
    #[test]
    fn local_constraint_attaching_to_a_non_thing_bails() {
        let existing: BTreeSet<(Layer, String, String)> =
            BTreeSet::from([(Layer::Domain, "entity".to_string(), "customer".to_string())]);
        let props = BTreeMap::from([(
            PROP_ATTACHES_TO.to_string(),
            "domain.entity.ghost".to_string(),
        )]);
        let err = eval_constraint(Layer::Domain, "law", &props, &existing).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("never a non-thing"), "{msg}");
        assert!(msg.contains("domain.entity.ghost"), "{msg}");
    }

    /// A local constraint attaches to a tier-1–3 node: a non-tier target (a
    /// global node) is refused, and a malformed FQN does not even parse.
    #[test]
    fn local_constraint_attachment_must_be_a_tier_1_3_node() {
        let existing: BTreeSet<(Layer, String, String)> = BTreeSet::from([
            (Layer::Global, "constraint".to_string(), "g1".to_string()),
            (Layer::Domain, "entity".to_string(), "customer".to_string()),
        ]);
        // A global node is not a tier-1–3 node.
        let props = BTreeMap::from([(
            PROP_ATTACHES_TO.to_string(),
            "global.constraint.g1".to_string(),
        )]);
        let err = eval_constraint(Layer::Domain, "law", &props, &existing).unwrap_err();
        assert!(err.to_string().contains("tier-1–3"), "{err}");
        // A malformed FQN does not parse.
        let props = BTreeMap::from([(PROP_ATTACHES_TO.to_string(), "not-an-fqn".to_string())]);
        let err = eval_constraint(Layer::Domain, "law", &props, &existing).unwrap_err();
        assert!(err.to_string().contains("FQN"), "{err}");
        // A global constraint must not declare an attachment at all.
        let props = BTreeMap::from([(
            PROP_ATTACHES_TO.to_string(),
            "domain.entity.customer".to_string(),
        )]);
        let err = eval_constraint(Layer::Global, "g2", &props, &existing).unwrap_err();
        assert!(err.to_string().contains("guards the whole graph"), "{err}");
    }

    /// A local constraint attaches to a tier-1–3 node — an EXISTING node
    /// outside tiers 1–3 (an implementation note, a plans task) is still
    /// refused: "never a non-thing" means the target must be a tier-1–3 thing,
    /// not merely resolve. The contrast — an existing tier-1–3 thing — passes.
    #[test]
    fn constraint_attachment_to_existing_non_tier_node_refused() {
        let existing: BTreeSet<(Layer, String, String)> = BTreeSet::from([
            (Layer::Implementation, "note".to_string(), "n1".to_string()),
            (Layer::Plans, "task".to_string(), "t1".to_string()),
            (Layer::Domain, "entity".to_string(), "customer".to_string()),
        ]);
        for target in ["implementation.note.n1", "plans.task.t1"] {
            let props = BTreeMap::from([(PROP_ATTACHES_TO.to_string(), target.to_string())]);
            let err = eval_constraint(Layer::Domain, "law", &props, &existing).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("not a tier"), "{target}: {msg}");
            assert!(msg.contains(target), "{target}: {msg}");
        }
        // An existing tier-1–3 thing validates (the contrast).
        let props = BTreeMap::from([(
            PROP_ATTACHES_TO.to_string(),
            "domain.entity.customer".to_string(),
        )]);
        assert!(eval_constraint(Layer::Domain, "law", &props, &existing).is_ok());
    }

    /// A `constraint` type is refused in a layer that does not host constraints
    /// (plans); implementation hosts the attach-only pair, so it is NOT refused.
    #[test]
    fn constraint_type_refused_in_a_layer_without_constraints() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        let err = eval_constraint(Layer::Plans, "x", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("does not exist in layer"), "{err}");
        // Implementation hosts note/constraint (attach-only) — a constraint
        // there is a real type.
        assert!(eval_constraint(Layer::Implementation, "x", &props, &empty).is_ok());
    }

    /// Satisfaction is NEVER evaluated: there is no constraint-expression
    /// language, and this function takes no prose input (the prose `body` is
    /// the node-file's top-level field, never read here). A constraint whose
    /// prose "would fail" — contradictory, or even expression-looking — still
    /// passes structure/reference validation, because satisfaction is assessed
    /// by review only.
    #[test]
    fn satisfaction_is_review_only_never_evaluated() {
        let existing: BTreeSet<(Layer, String, String)> =
            BTreeSet::from([(Layer::Domain, "entity".to_string(), "customer".to_string())]);
        // Contradictory prose (stands in for the top-level `body`) — the binary
        // never reads or evaluates it; only the reference is checked.
        let props = BTreeMap::from([
            (
                PROP_ATTACHES_TO.to_string(),
                "domain.entity.customer".to_string(),
            ),
            (
                "body".to_string(),
                "every order has a customer AND every order has no customer".to_string(),
            ),
        ]);
        assert!(eval_constraint(Layer::Domain, "law", &props, &existing).is_ok());
        // A global constraint with expression-looking prose is equally
        // unevaluated.
        let props = BTreeMap::from([(
            "body".to_string(),
            "count(entities) == 0 AND count(entities) > 0".to_string(),
        )]);
        assert!(eval_constraint(Layer::Global, "law", &props, &existing).is_ok());
    }

    // --- Node-file schema + single-node writer (phase-3 task-8) ---

    /// A unique temp dir for one test (removed on cleanup) — the node-file
    /// writer is the first I/O in this module, so tests stage under
    /// `std::env::temp_dir()` like specs.rs/git.rs do.
    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apg-layers-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A sample requirement node file mirroring the §4.1 example.
    fn sample_node() -> NodeFile {
        NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: "place-order".to_string(),
            body: "A customer can place an order.".to_string(),
            properties: BTreeMap::new(),
            out: vec![OutEdge {
                kind: "drives".to_string(),
                target: "domain.service.checkout".to_string(),
                properties: BTreeMap::new(),
            }],
            in_edges: vec![InEdge {
                kind: "contains".to_string(),
                source: "requirements.user.customer".to_string(),
                properties: BTreeMap::new(),
            }],
        }
    }

    /// write_node writes one file at
    /// `<root>/layers/<layer>/<type>/<name>.json`; the layer/type/name/body/
    /// properties/out/in round-trip (write → read → deserialize == original),
    /// and the FQN derived from the path's segments equals layer.type.name.
    #[test]
    fn write_node_writes_file_with_round_tripping_identity() {
        let root = temp_root("roundtrip");
        let node = sample_node();
        let path = write_node(&root, &node).unwrap();
        assert_eq!(
            path,
            root.join("layers")
                .join("requirements")
                .join("requirement")
                .join("place-order.json")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let back: NodeFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, node);
        // The file name IS the identity: the FQN is the path's segments.
        assert_eq!(
            fqn(Layer::Requirements, &back.node_type, &back.name),
            "requirements.requirement.place-order"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The FQN builder is the inverse of [`parse_fqn`]: `<layer>.<type>.<name>`,
    /// no project prefix.
    #[test]
    fn fqn_builder_returns_layer_type_name() {
        assert_eq!(
            fqn(Layer::Requirements, "requirement", "place-order"),
            "requirements.requirement.place-order"
        );
        assert_eq!(
            fqn(Layer::Domain, "service", "checkout"),
            "domain.service.checkout"
        );
        assert_eq!(
            fqn(Layer::Global, "constraint", "law"),
            "global.constraint.law"
        );
        // And it is the inverse of parse_fqn.
        let (layer, node_type, name) = parse_fqn("domain.entity.customer").unwrap();
        assert_eq!(fqn(layer, &node_type, &name), "domain.entity.customer");
    }

    /// A plans-layer node is refused — plans is transient (apg/.trans/plans/),
    /// never a durable node-file layer — and nothing is written.
    #[test]
    fn write_node_refuses_plans_layer() {
        let root = temp_root("plans");
        let mut node = sample_node();
        node.layer = "plans".to_string();
        node.node_type = "task".to_string();
        let err = write_node(&root, &node).unwrap_err().to_string();
        assert!(err.contains("plans"), "{err}");
        assert!(!root.join("layers").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An allowlist-violating name is refused (never sanitized) — the file name
    /// must stay safe. Also covers an unknown layer.
    #[test]
    fn write_node_refuses_allowlist_violating_name() {
        let root = temp_root("badname");
        for bad in ["CamelCase", "with.dot", "with space", "-lead", ""] {
            let mut node = sample_node();
            node.name = bad.to_string();
            let err = write_node(&root, &node).unwrap_err().to_string();
            assert!(err.contains("allowlist"), "{bad}: {err}");
        }
        // An unknown layer is refused too.
        let mut node = sample_node();
        node.layer = "banana".to_string();
        let err = write_node(&root, &node).unwrap_err().to_string();
        assert!(err.contains("banana"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Metadata properties (short ids) are stored verbatim, never treated as
    /// identity — the file name stays layer.type.name even when an `id` is
    /// present.
    #[test]
    fn write_node_preserves_metadata_properties_verbatim() {
        let root = temp_root("metadata");
        let mut node = sample_node();
        node.properties = BTreeMap::from([("id".to_string(), "R1".to_string())]);
        let path = write_node(&root, &node).unwrap();
        // The identity is the path, never the short id.
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("place-order.json")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let back: NodeFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.properties.get("id").map(String::as_str), Some("R1"));
        assert_eq!(back, node);
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- In/out edge pairing (phase-3 task-9) ---

    /// A bare node file with no edges, for building the pairing test fixtures.
    fn node(layer: &str, node_type: &str, name: &str) -> NodeFile {
        NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: String::new(),
            properties: BTreeMap::new(),
            out: Vec::new(),
            in_edges: Vec::new(),
        }
    }

    /// A bare out-edge with no properties.
    fn out_edge(kind: &str, target: &str) -> OutEdge {
        OutEdge {
            kind: kind.to_string(),
            target: target.to_string(),
            properties: BTreeMap::new(),
        }
    }

    /// A bare in-edge with no properties.
    fn in_edge(kind: &str, source: &str) -> InEdge {
        InEdge {
            kind: kind.to_string(),
            source: source.to_string(),
            properties: BTreeMap::new(),
        }
    }

    /// A fully paired set — A's out `contains -> B` and B's in `contains <-
    /// A`, matching source/kind/target/properties — passes.
    #[test]
    fn paired_in_and_out_edges_pass() {
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.b"));
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        assert!(check_edge_pairing(&[a, b]).is_ok());
    }

    /// An out edge in A without the matching in edge in B is an ERROR — the
    /// error names the node, the kind, and the missing counterpart.
    #[test]
    fn out_edge_without_matching_in_edge_errors() {
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.b"));
        let b = node("requirements", "requirement", "b");
        let msg = check_edge_pairing(&[a, b]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.a"), "{msg}");
        assert!(msg.contains("contains"), "{msg}");
        assert!(msg.contains("BOTH endpoint files"), "{msg}");
    }

    /// An in edge in B without the matching out edge in A is an ERROR — the
    /// symmetric half of the pairwise rule.
    #[test]
    fn in_edge_without_matching_out_edge_errors() {
        let a = node("requirements", "requirement", "a");
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        let msg = check_edge_pairing(&[a, b]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.b"), "{msg}");
        assert!(msg.contains("contains"), "{msg}");
        assert!(msg.contains("BOTH endpoint files"), "{msg}");
    }

    /// The SAME endpoints but DIFFERENT edge properties is a mismatch — a
    /// match requires identical properties, not merely endpoint existence.
    #[test]
    fn same_endpoints_different_properties_is_a_mismatch() {
        let mut a = node("requirements", "requirement", "a");
        let mut oe = out_edge("contains", "requirements.requirement.b");
        oe.properties
            .insert("flavor".to_string(), "direct".to_string());
        a.out.push(oe);
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        let msg = check_edge_pairing(&[a, b]).unwrap_err().to_string();
        assert!(msg.contains("contains"), "{msg}");
        assert!(msg.contains("properties"), "{msg}");
    }

    /// A dangling authored target — parses as `<layer>.<type>.<name>` but no
    /// such node file exists — is an error, not a silent skip.
    #[test]
    fn dangling_authored_target_errors() {
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.ghost"));
        let msg = check_edge_pairing(&[a]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.ghost"), "{msg}");
    }

    /// A dangling authored source on an in edge is the symmetric error.
    #[test]
    fn dangling_authored_source_errors() {
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.ghost"));
        let msg = check_edge_pairing(&[b]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.ghost"), "{msg}");
    }

    /// An `implemented-by` out edge to a code FQN (non-parsing target) is NOT
    /// flagged — code endpoints are exempt from the pairwise rule.
    #[test]
    fn implemented_by_to_code_fqn_is_not_flagged() {
        let mut sys = node("solution", "system", "payments");
        sys.out.push(out_edge("implemented-by", "apg.main"));
        assert!(check_edge_pairing(&[sys]).is_ok());
    }

    /// A `details` out edge to a code FQN is NOT flagged — the target is a
    /// code node (no file), so there is only the spec-side half.
    #[test]
    fn details_to_code_fqn_is_not_flagged() {
        let mut n = node("requirements", "note", "n1");
        n.out.push(out_edge(
            "details",
            "apg.artifacts.write_jsonl_and_reingest",
        ));
        assert!(check_edge_pairing(&[n]).is_ok());
    }

    /// A symmetric in edge whose source is a code FQN (non-parsing) is NOT
    /// flagged — the parse-based rule treats code endpoints as exempt in both
    /// directions.
    #[test]
    fn in_edge_from_code_fqn_is_not_flagged() {
        let mut sys = node("solution", "system", "payments");
        sys.in_edges.push(in_edge("implemented-by", "apg.main"));
        assert!(check_edge_pairing(&[sys]).is_ok());
    }
}
