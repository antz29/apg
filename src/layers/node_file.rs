//! The node-file schema and single-node writer (SPEC §4.1): the on-disk
//! `NodeFile` shape with its paired out/in edges, the FQN/path derivations,
//! and the cheap single-file read/write helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::catalog::{LAYERS_DIR, Layer, StoragePolicy};
use super::validate::valid_name;

/// The node-file schema's properties map (SPEC §4.1). Known keys
/// (kind/attribute/root) are validated per type by [`validate_node`]; unknown
/// keys are metadata — allowed, never identity.
pub type NodeProperties = BTreeMap<String, String>;

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
