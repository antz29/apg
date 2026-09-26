//! Write-time validation of the node-file model (SPEC §3.3/§4.1): the node
//! rules, the edge-kind matrix, the derived group-coupling relation, prose
//! constraints, and the in/out edge-pairing invariant.

use std::collections::{BTreeMap, BTreeSet};

use super::catalog::Layer;
use super::node_file::{NodeFile, NodeProperties, fqn};

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
pub(crate) const EDGE_KINDS: [&str; 11] = [
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
pub(crate) type MatrixRow = (
    &'static str,
    Layer,
    &'static [&'static str],
    Layer,
    &'static [&'static str],
);

/// The SPEC §3.3 edge-kind matrix — only the two-authored-endpoint kinds are
/// listed; `implemented-by` and `details` are special-cased in
/// [`validate_edge`].
pub(crate) const MATRIX: &[MatrixRow] = &[
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
pub(crate) fn validate_edge(kind: &str, source: &str, target: &str) -> anyhow::Result<()> {
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
pub(crate) fn validate_assembled_rules(
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
