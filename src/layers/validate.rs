//! Write-time validation of the node-file model (SPEC §4/§4.1): the node
//! rules, the edge-kind matrix, the derived group-coupling relation, prose
//! constraints, the whole-model cardinalities, and the in/out edge-pairing
//! invariant.

use std::collections::{BTreeMap, BTreeSet};

use super::catalog::Layer;
use super::node_file::{NodeFile, NodeProperties, fqn};

// ---------------------------------------------------------------------------
// SPEC §4 node rules — write-time validation (phase-3 task-2)
// ---------------------------------------------------------------------------

/// Property keys the tier model validates (SPEC §4). A key is validated
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

/// The SPEC §4 name allowlist `[a-z0-9][a-z0-9-]*` — refuse, never
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

/// Validate one proposed node against the SPEC §4 node rules before it is
/// written — write-time enforcement. Pure: no I/O; `existing` is the current
/// (layer, type, name) identity universe, used only for the uniqueness rule.
///
/// Enforced (the binary can check these):
/// - the name matches the allowlist `[a-z0-9][a-z0-9-]*` — refused, never
///   sanitized;
/// - the type exists in its layer — case-sensitive exact match against the
///   catalog's lowercase type spellings ([`Layer::node_types`]); a `note`
///   outside the implementation layer is refused, naming the note-confinement
///   law `global.constraint.note-implementation-only`;
/// - kind attributes when the property is present: `Entity` **requires**
///   `kind` ∈ {entity, event} (the spec's only "requires"); `Group` takes
///   `attribute` ∈ {core, supporting, generic} and an optional `root`
///   (format-validated against the name allowlist, independent of the
///   attribute); `Container` takes `kind` ∈ {app, service, db, queue};
/// - names unique per (layer, type).
///
/// Not enforced (prose/agent guidance the binary cannot check): requirement
/// decomposition atomicity, "Value immutable", "Service stateless", the
/// Stakeholder/User and Person (C4 view) semantics. Tree acyclicity for a
/// `contains` edge set is [`validate_trees_acyclic`].
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
    // The note-confinement law (`global.constraint.note-implementation-only`):
    // a note attaches only to implementation-layer code, so a note node in any
    // other layer (requirements/domain/solution/global — and plans, which holds
    // no note type) is refused here, ahead of the generic catalog check, so the
    // refusal names the rule rather than "type `note` does not exist in layer".
    if node_type == "note" && layer != Layer::Implementation {
        anyhow::bail!(
            "a `note` attaches only to implementation-layer code — `{}.note.{name}` is a violation of `global.constraint.note-implementation-only` (notes are confined to the implementation layer)",
            layer.layer_dir()
        );
    }
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

/// Refuse a cycle in a `contains` edge set — the SPEC §4 node rule
/// "contains trees acyclic". `edges` is the per-change set of
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
    // Dangling FQN references are write-time errors (SPEC §4).
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
    // contains tree must stay acyclic — a self-loop is a cycle).
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
                        anyhow::bail!("contains cycle: {}", cycle.join(" -> "));
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
// SPEC §4 edge-kind matrix — write-time validation (phase-3 task-4)
// ---------------------------------------------------------------------------

/// The SPEC §4 durable edge kinds — the complete list. Plan edges
/// (`contains` plan tiers, `gates`, `satisfies`, the Task→Implementation
/// verbs) and `reviews` are **transient** kinds — not here. The matrix rows
/// cover the twelve two-authored-endpoint kinds; `implemented-by` and
/// `details` (both a code-FQN target) have a code-exempt endpoint and are
/// special-cased in [`validate_edge`].
pub(crate) const EDGE_KINDS: [&str; 14] = [
    "requires",
    "contains",
    "drives",
    "reads",
    "writes",
    "calls",
    "publishes",
    "subscribes",
    "realises",
    "is-role",
    "uses",
    "represents",
    "implemented-by",
    "details",
];

/// One SPEC §4 matrix row: `(kind, source-layer, source-types,
/// target-layer, target-types)`. Each endpoint is a (layer, type) pair: a type
/// maps to a unique layer except `constraint`, which spans the layer-scoped
/// tiers (a `note`, by contrast, lives only in the implementation layer) — but
/// neither is a matrix endpoint, so the layer+type granularity here is
/// unambiguous. Source/target type sets are subsets of [`Layer::node_types`].
pub(crate) type MatrixRow = (
    &'static str,
    Layer,
    &'static [&'static str],
    Layer,
    &'static [&'static str],
);

/// The SPEC §4 edge-kind matrix — only the two-authored-endpoint kinds are
/// listed; `implemented-by` (Component → code) and `details` (Note → code) are
/// special-cased in [`validate_edge`].
pub(crate) const MATRIX: &[MatrixRow] = &[
    (
        "requires",
        Layer::Requirements,
        &["stakeholder", "user"],
        Layer::Requirements,
        &["requirement"],
    ),
    (
        "contains",
        Layer::Requirements,
        &["requirement"],
        Layer::Requirements,
        &["requirement"],
    ),
    (
        "contains",
        Layer::Domain,
        &["group"],
        Layer::Domain,
        &["group", "entity", "service"],
    ),
    (
        "contains",
        Layer::Domain,
        &["entity"],
        Layer::Domain,
        &["value"],
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
        &["service"],
    ),
    (
        "reads",
        Layer::Domain,
        &["service"],
        Layer::Domain,
        &["entity"],
    ),
    (
        "writes",
        Layer::Domain,
        &["service"],
        Layer::Domain,
        &["entity"],
    ),
    (
        "calls",
        Layer::Domain,
        &["actor"],
        Layer::Domain,
        &["service"],
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
        &["actor"],
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
        "realises",
        Layer::Solution,
        &["component"],
        Layer::Domain,
        &["service"],
    ),
    (
        "is-role",
        Layer::Solution,
        &["container"],
        Layer::Solution,
        &["role"],
    ),
    (
        "is-role",
        Layer::Solution,
        &["component"],
        Layer::Solution,
        &["role"],
    ),
    (
        "uses",
        Layer::Solution,
        &["person"],
        Layer::Solution,
        &["component"],
    ),
    (
        "represents",
        Layer::Domain,
        &["actor"],
        Layer::Requirements,
        &["user"],
    ),
    (
        "represents",
        Layer::Solution,
        &["person"],
        Layer::Domain,
        &["actor"],
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

/// Validate a set of proposed edges against the SPEC §4 edge-kind matrix —
/// write-time enforcement, pure (no I/O; the FQN string itself encodes
/// `layer.type.name`, so the endpoint (layer, type) is derived by parsing, no
/// store needed). Each tuple is `(kind, source FQN, target FQN)`; the batch
/// loops [`validate_edge`] over every edge.
///
/// This is also the spine check (SPEC §6): every spine hop is tier-locked by
/// the matrix (`requires`: Stakeholder/User → Requirement; `drives`:
/// Requirement → Service; `realises`: Component → Service; `implemented-by`:
/// Component → code), so a tier skip is impossible once the matrix holds — the
/// matrix IS the spine enforcement, and there is no extra tier machinery. A
/// dangling FQN endpoint (malformed FQN or unknown layer) is a write-time error.
pub fn validate_edges(edges: &[(&str, &str, &str)]) -> anyhow::Result<()> {
    for &(kind, source, target) in edges {
        validate_edge(kind, source, target)?;
    }
    Ok(())
}

/// Validate one `(kind, source, target)` edge against the SPEC §4 matrix.
/// The source of every §4 edge is an authored node — parsed first, so a
/// malformed source FQN or unknown layer is a dangling-reference write-time
/// error for every kind. `implemented-by` and `details` (both a code-FQN
/// target) have a code-exempt **target**: only their source is validated here;
/// code-FQN validation against the scanned graph is task-10
/// ([`validate_code_refs`]). For every other kind, both endpoints must parse
/// as authored-node FQNs and the (layer, type) shape must match a matrix row.
pub(crate) fn validate_edge(kind: &str, source: &str, target: &str) -> anyhow::Result<()> {
    let (src_layer, src_type, _src_name) = parse_fqn(source)?;

    match kind {
        // implemented-by: Component → code FQN. The target is exempt —
        // validated against the scanned graph by validate_code_refs.
        "implemented-by" => {
            if src_layer != Layer::Solution || src_type != "component" {
                anyhow::bail!(
                    "`implemented-by` source `{source}` must be a Solution Component, not {}.{src_type}",
                    src_layer.layer_dir()
                );
            }
            Ok(())
        }
        // details: Note → a single code identity. The target must NOT parse as
        // an authored-node FQN: a `details` edge names exactly one code FQN
        // (`global.constraint.note-details-shape`), so any authored target — a
        // `note` included, since a note parses as authored — is refused. A
        // non-parsing target is a code FQN and stays exempt from the
        // scanned-graph check here ([`validate_code_refs`] owns it).
        "details" => {
            if src_type.as_str() != "note" {
                anyhow::bail!(
                    "`details` source `{source}` must be a `note` (a note lives only in the implementation layer), not {}.{src_type}",
                    src_layer.layer_dir()
                );
            }
            if parse_fqn(target).is_ok() {
                anyhow::bail!(
                    "`details` target `{target}` from source `{source}` must be a code identity, not an authored node — a `details` edge names exactly one code FQN"
                );
            }
            Ok(())
        }
        // The matrix kinds: both endpoints parse, and the (layer, type) shape
        // must match a §4 row.
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

/// The property half of the SPEC §4 `publishes`/`subscribes` matrix rows:
/// those edges target an `Entity` with `kind: event` (a published/subscribed
/// event), not a plain entity. [`validate_edge`] is type-only — it never sees
/// a node's properties — so the caller supplies the assembled post-mutation
/// node set as FQN → node file and this function resolves each target against
/// it. A target absent from `assembled` is left to the dangling-reference
/// checks ([`validate_edges`] parses matrix endpoints; [`check_edge_pairing`]
/// resolves authored endpoints): this function only refuses a present target
/// that is not an event.
///
/// Shared by the write/scan paths ([`validate_assembled_rules`]) and the spec
/// lint's `model_errors` over its selected edges, so both report the identical
/// message. `pub(crate)` so `layers/mod.rs` can re-export it to the lint.
pub(crate) fn validate_event_targets(
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
                "`{kind}` target `{target}` must be an Entity with kind `event` — the §4 matrix qualifies publishes/subscribes targets as Entity (kind: event)"
            );
        }
    }
    Ok(())
}

/// The assembled-set property rules of SPEC §4 — the rules the edge-kind
/// matrix cannot express (it never sees properties, and a cycle only exists
/// across edges):
///
/// - `contains` trees are acyclic ([`validate_trees_acyclic`], including its
///   dangling-endpoint refusal);
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
pub(crate) fn validate_assembled_rules(
    assembled: &BTreeMap<String, &NodeFile>,
    universe: &BTreeSet<(Layer, String, String)>,
) -> anyhow::Result<()> {
    let mut tree_edges: Vec<(String, String)> = Vec::new();
    let mut event_edges: Vec<(String, String, String)> = Vec::new();
    for (from, n) in assembled {
        for oe in &n.out {
            match oe.kind.as_str() {
                "contains" => tree_edges.push((from.clone(), oe.target.clone())),
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
// SPEC §4/§8.1 — whole-model cardinalities
// ---------------------------------------------------------------------------

/// The consistency boundary a validation call enforces — **context**, distinct
/// from **scope** (SPEC §9.2). The context governs exactly one rule: the
/// presence of a Component's `implemented-by` claim (SPEC §8.2, the sole
/// deferred obligation).
///
/// - [`Context::Save`] and [`Context::Lint`]: a missing claim is permitted; a
///   present one must be at most one and its target must be real scanned code
///   (the target check is [`validate_code_refs`]'s, not this enum's).
/// - [`Context::MergeStrength`]: a missing claim is an error — merge requires
///   exactly one real scanned-code target per Component.
///
/// Lint uses `Lint`; the save boundary uses `Save`; the merge act
/// (`project_merge_at`) uses `MergeStrength`. Target realness itself stays
/// [`validate_code_refs`]'s job. Scope is the caller's tier filter
/// ([`validate_cardinalities`]'s `scope` argument), never a context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Context {
    /// Wired by the whole-model save boundary (SPEC §8.1): intermediate
    /// session state is incomplete, so save validates the buffered model.
    Save,
    Lint,
    /// Wired by the merge act (`project_merge_at`, SPEC §8.2/§9.2): the
    /// deferral of a Component's `implemented-by` presence ends here.
    MergeStrength,
}

/// Whole-model cardinality validation (SPEC §4.1–4.4, §8.1): the rules that
/// only hold across the assembled node set, not per node or per edge. Returns
/// one message per violation (never stops at the first), so a caller can report
/// the complete set. Pure — no I/O.
///
/// **Scope** (SPEC §9.2): `scope` is the tier whose rules are checked, or
/// `None` for the whole model. A node outside the scope contributes no
/// cardinality error of its own — its incoming/outgoing edges are still read,
/// so an in-scope node's typed upstream/downstream path is resolved against the
/// whole assembled set. A single-tier lint therefore checks that tier's local
/// structure without demanding that unrelated tiers are complete.
///
/// **Context** ([`Context`]): only the Component `implemented-by` presence rule
/// depends on it (SPEC §8.2) — missing permitted under `Save`/`Lint` and an
/// error under `MergeStrength`.
///
/// Enforced:
/// - every Service and Entity has exactly one `contains` owner (a Group); every
///   Value has exactly one `contains` owner, and that owner is an Entity (never
///   a Group);
/// - a leaf Group (no subgroup) contains at least one Service; a Group with
///   subgroups contains no direct Service/Entity (the two modes are exclusive);
/// - every Entity contains at least one Value;
/// - every Service is driven by at least one Requirement;
/// - every Component realises at least one Service;
/// - every Container and Component `is-role` exactly one Role;
/// - a Service's `reads`/`writes` target belongs to the Service's own Group;
/// - every Stakeholder and User requires at least one Requirement, and every
///   Requirement is rooted in a Stakeholder or User directly or through
///   Requirement decomposition;
/// - every Actor represents at least one User and calls at least one Service;
/// - every Person represents at least one Actor and uses at least one Component;
/// - every System contains at least one Container, and every Container
///   contains at least one Component;
/// - a Component carries at most one `implemented-by` claim, and under
///   [`Context::MergeStrength`] at least one (presence itself is deferred to
///   merge under `Save`/`Lint`, SPEC §8.2);
/// - `contains` is acyclic (delegated to [`validate_trees_acyclic`]).
pub(crate) fn validate_cardinalities(
    assembled: &BTreeMap<String, &NodeFile>,
    scope: Option<Layer>,
    context: Context,
) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    let in_scope = |node: &NodeFile| scope.is_none_or(|tier| node.layer == tier.layer_dir());

    // `kind → source` and `kind → target` adjacency, keyed by (kind, FQN).
    let mut out_edges: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut in_edges: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for (from, node) in assembled {
        for edge in &node.out {
            out_edges
                .entry((edge.kind.clone(), from.clone()))
                .or_default()
                .insert(edge.target.clone());
            in_edges
                .entry((edge.kind.clone(), edge.target.clone()))
                .or_default()
                .insert(from.clone());
        }
    }
    let out_of = |fqn: &str, kind: &str| -> BTreeSet<String> {
        out_edges
            .get(&(kind.to_string(), fqn.to_string()))
            .cloned()
            .unwrap_or_default()
    };
    let in_of = |fqn: &str, kind: &str| -> BTreeSet<String> {
        in_edges
            .get(&(kind.to_string(), fqn.to_string()))
            .cloned()
            .unwrap_or_default()
    };
    let is_type =
        |fqn: &str, node_type: &str| assembled.get(fqn).is_some_and(|n| n.node_type == node_type);
    // The present `contains` owners of one node (dangling endpoints are left to
    // the acyclicity check's dangling-endpoint refusal).
    let owners = |fqn: &str| -> BTreeSet<String> {
        in_of(fqn, "contains")
            .into_iter()
            .filter(|owner| assembled.contains_key(owner))
            .collect()
    };
    let owner_of = |fqn: &str| -> Option<String> {
        let owners = owners(fqn);
        (owners.len() == 1).then(|| owners.into_iter().next().unwrap())
    };

    // Exclusive ownership of Services, Entities, and Values (SPEC §4.2).
    for (fqn, node) in assembled {
        if !in_scope(node) {
            continue;
        }
        match node.node_type.as_str() {
            "service" | "entity" => {
                let owners = owners(fqn);
                if owners.len() != 1 {
                    errors.push(format!(
                        "`{fqn}` must be contained by exactly one Group — found {} `contains` owners",
                        owners.len()
                    ));
                } else {
                    let owner = owners.into_iter().next().unwrap();
                    if !is_type(&owner, "group") {
                        errors.push(format!(
                            "`{fqn}` is contained by `{owner}` — a Service/Entity is owned only by a Group"
                        ));
                    }
                }
            }
            "value" => {
                let owners = owners(fqn);
                if owners.len() != 1 {
                    errors.push(format!(
                        "Value `{fqn}` must be contained by exactly one Entity — found {} `contains` owners",
                        owners.len()
                    ));
                } else {
                    let owner = owners.into_iter().next().unwrap();
                    if !is_type(&owner, "entity") {
                        errors.push(format!(
                            "Value `{fqn}` is contained by `{owner}` — a Value is owned only by an Entity, never a Group"
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Group modes (SPEC §4.2): a Group either nests subgroups or holds leaf
    // contents, never both; a leaf Group holds at least one Service.
    for (fqn, node) in assembled {
        if node.node_type != "group" || !in_scope(node) {
            continue;
        }
        let children = out_of(fqn, "contains");
        let has_subgroup = children.iter().any(|c| is_type(c, "group"));
        let direct_services = children.iter().filter(|c| is_type(c, "service")).count();
        let direct_entities = children.iter().filter(|c| is_type(c, "entity")).count();
        if has_subgroup {
            if direct_services > 0 || direct_entities > 0 {
                errors.push(format!(
                    "Group `{fqn}` mixes subgroups with direct Service/Entity contents — a Group contains subgroups or leaf contents, never both"
                ));
            }
        } else if direct_services == 0 {
            errors.push(format!(
                "leaf Group `{fqn}` contains no Service — a Group with no subgroups must contain at least one Service"
            ));
        }
    }

    // Every Entity owns at least one Value (SPEC §4.2).
    for (fqn, node) in assembled {
        if node.node_type != "entity" || !in_scope(node) {
            continue;
        }
        let values = out_of(fqn, "contains")
            .iter()
            .filter(|c| is_type(c, "value"))
            .count();
        if values == 0 {
            errors.push(format!(
                "Entity `{fqn}` contains no Value — every Entity owns at least one Value"
            ));
        }
    }

    // Every Service is driven by at least one Requirement (SPEC §4.3).
    for (fqn, node) in assembled {
        if node.node_type != "service" || !in_scope(node) {
            continue;
        }
        let drivers = in_of(fqn, "drives")
            .iter()
            .filter(|s| is_type(s, "requirement"))
            .count();
        if drivers == 0 {
            errors.push(format!(
                "Service `{fqn}` is driven by no Requirement — every Service is driven by at least one Requirement"
            ));
        }
    }

    // Solution structure and realisation (SPEC §4.4).
    for (fqn, node) in assembled {
        if !in_scope(node) {
            continue;
        }
        match node.node_type.as_str() {
            "component" => {
                let services = out_of(fqn, "realises")
                    .iter()
                    .filter(|t| is_type(t, "service"))
                    .count();
                if services == 0 {
                    errors.push(format!(
                        "Component `{fqn}` realises no Service — every Component realises at least one Service"
                    ));
                }
                let claims = out_of(fqn, "implemented-by").len();
                if claims > 1 {
                    errors.push(format!(
                        "Component `{fqn}` carries {claims} `implemented-by` claims — a Component has at most one code target"
                    ));
                } else if claims == 0 && context == Context::MergeStrength {
                    errors.push(format!(
                        "Component `{fqn}` has no `implemented-by` claim — merge-strength validation requires exactly one real scanned-code target"
                    ));
                }
                let roles = out_of(fqn, "is-role")
                    .iter()
                    .filter(|t| is_type(t, "role"))
                    .count();
                if roles != 1 {
                    errors.push(format!(
                        "`{fqn}` `is-role` {roles} Role(s) — a Container/Component has exactly one Role"
                    ));
                }
            }
            "container" => {
                let roles = out_of(fqn, "is-role")
                    .iter()
                    .filter(|t| is_type(t, "role"))
                    .count();
                if roles != 1 {
                    errors.push(format!(
                        "`{fqn}` `is-role` {roles} Role(s) — a Container/Component has exactly one Role"
                    ));
                }
                let components = out_of(fqn, "contains")
                    .iter()
                    .filter(|t| is_type(t, "component"))
                    .count();
                if components == 0 {
                    errors.push(format!(
                        "Container `{fqn}` contains no Component — every Container contains at least one Component"
                    ));
                }
            }
            "system" => {
                let containers = out_of(fqn, "contains")
                    .iter()
                    .filter(|t| is_type(t, "container"))
                    .count();
                if containers == 0 {
                    errors.push(format!(
                        "System `{fqn}` contains no Container — every System contains at least one Container"
                    ));
                }
            }
            _ => {}
        }
    }

    // Cross-Group privacy (SPEC §5): a Service reads/writes only Entities in its
    // own leaf Group.
    for (fqn, node) in assembled {
        if node.node_type != "service" || !in_scope(node) {
            continue;
        }
        let Some(service_owner) = owner_of(fqn) else {
            continue;
        };
        for kind in ["reads", "writes"] {
            for target in out_of(fqn, kind) {
                let Some(entity_owner) = owner_of(&target) else {
                    continue;
                };
                if entity_owner != service_owner {
                    errors.push(format!(
                        "Service `{fqn}` {kind} Entity `{target}` across Group boundaries — `{fqn}` is owned by `{service_owner}`, `{target}` by `{entity_owner}`; reads/writes are confined to the Service's own Group"
                    ));
                }
            }
        }
    }

    // Identity refinement (SPEC §4.1, §8.1).
    for (fqn, node) in assembled {
        if !in_scope(node) {
            continue;
        }
        match node.node_type.as_str() {
            "stakeholder" | "user" => {
                let required = out_of(fqn, "requires")
                    .iter()
                    .filter(|t| is_type(t, "requirement"))
                    .count();
                if required == 0 {
                    errors.push(format!(
                        "`{fqn}` requires no Requirement — every Stakeholder/User requires at least one Requirement"
                    ));
                }
            }
            "actor" => {
                let users = out_of(fqn, "represents")
                    .iter()
                    .filter(|t| is_type(t, "user"))
                    .count();
                if users == 0 {
                    errors.push(format!(
                        "Actor `{fqn}` represents no User — every Actor represents at least one User"
                    ));
                }
                let services = out_of(fqn, "calls")
                    .iter()
                    .filter(|t| is_type(t, "service"))
                    .count();
                if services == 0 {
                    errors.push(format!(
                        "Actor `{fqn}` calls no Service — every Actor calls at least one Service"
                    ));
                }
            }
            "person" => {
                let actors = out_of(fqn, "represents")
                    .iter()
                    .filter(|t| is_type(t, "actor"))
                    .count();
                if actors == 0 {
                    errors.push(format!(
                        "Person `{fqn}` represents no Actor — every Person represents at least one Actor"
                    ));
                }
                let components = out_of(fqn, "uses")
                    .iter()
                    .filter(|t| is_type(t, "component"))
                    .count();
                if components == 0 {
                    errors.push(format!(
                        "Person `{fqn}` uses no Component — every Person uses at least one Component"
                    ));
                }
            }
            _ => {}
        }
    }

    // Requirement rooting (SPEC §4.1): a Requirement is rooted when it, or an
    // ancestor via `contains` decomposition, is a `requires` target of a
    // Stakeholder/User.
    for (fqn, node) in assembled {
        if node.node_type != "requirement" || !in_scope(node) {
            continue;
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = vec![fqn.clone()];
        let mut rooted = false;
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if in_of(&current, "requires")
                .iter()
                .any(|s| is_type(s, "stakeholder") || is_type(s, "user"))
            {
                rooted = true;
                break;
            }
            for parent in in_of(&current, "contains") {
                if is_type(&parent, "requirement") {
                    stack.push(parent);
                }
            }
        }
        if !rooted {
            errors.push(format!(
                "Requirement `{fqn}` is not rooted in a Stakeholder or User — directly via `requires` or through Requirement decomposition"
            ));
        }
    }

    // Containment is acyclic (SPEC §4.2/§4.4) — the existing check, delegated.
    let containment: Vec<(String, String)> = assembled
        .iter()
        .filter(|(_, node)| in_scope(node))
        .flat_map(|(from, node)| {
            node.out
                .iter()
                .filter(|edge| edge.kind == "contains")
                .map(move |edge| (from.clone(), edge.target.clone()))
        })
        .collect();
    let universe: BTreeSet<(Layer, String, String)> =
        assembled.keys().filter_map(|f| parse_fqn(f).ok()).collect();
    let refs: Vec<(&str, &str)> = containment
        .iter()
        .map(|(from, to)| (from.as_str(), to.as_str()))
        .collect();
    if let Err(err) = validate_trees_acyclic(&refs, &universe) {
        errors.push(err.to_string());
    }

    errors
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
// SPEC §7 — constraints are prose, review-only (phase-3 task-6)
// ---------------------------------------------------------------------------

/// Validate a proposed `constraint` node at write time (SPEC §7): constraints
/// are **prose** ("X must hold") over things that **exist**. The binary
/// validates only a constraint's *structure* — never whether the prose actually
/// holds. **Satisfaction is assessed by review** (non-deterministic), never
/// executed: there is no constraint-expression language in this change-set, and
/// this function takes no prose input at all (the prose `body` is the node-file
/// schema's top-level `body`, stored verbatim and read by a reviewer — not by
/// the binary).
///
/// Structure (reuses [`validate_node`]'s shared node rules — a `constraint`
/// carries no `kind`/`attribute`/`root`): the name matches the allowlist, the
/// type exists in its layer (requirements/domain/solution/implementation/global
/// host `constraint`; plans does not, so a plans-layer constraint is refused),
/// and the name is unique per (layer, type).
///
/// A constraint's *scope* is declared by its **layer** (SPEC §7.1): a `global`
/// constraint binds the whole durable spec, a `<tier>` constraint binds that
/// tier. It has no attachment property or edge; satisfaction is review-only.
///
/// `existing` is the current identity universe, exactly as in [`validate_node`]:
/// every node that already exists (or is co-proposed in the change) — *not* the
/// constraint being validated.
pub fn eval_constraint(
    layer: Layer,
    name: &str,
    properties: &NodeProperties,
    existing: &BTreeSet<(Layer, String, String)>,
) -> anyhow::Result<()> {
    // Structure: reuse the shared node rules — name allowlist, type-in-layer,
    // uniqueness. A `constraint` takes no kind/attribute/root, and neither the
    // prose body nor any metadata property is an input here.
    validate_node(layer, "constraint", name, properties, existing)
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
/// only §4 edges ([`validate_edges`] refuses the transient kinds
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
