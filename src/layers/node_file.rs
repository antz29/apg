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

/// Encode a node-file / edge properties map to the **canonical JSON string**
/// stored in the layers projection's serialized-properties column.
///
/// This is the single source of truth for that column on every durable
/// authored node table and every rel table a durable authored edge can
/// occupy, shared by both the full-scan path (create_schema /
/// `build_load_files`) and the session's incremental reingest path
/// (`merge_node` / `merge_edge`), and mirrored by `properties_from_json`'s
/// decode (phase-4 task-2).
///
/// Canonical means:
/// - **deterministic key order** — the map is a [`BTreeMap`], which iterates
///   in sorted key order, so equal maps always encode to identical strings;
/// - **every key preserved** — unknown keys and keys carrying an empty value
///   are emitted verbatim (the properties map is metadata, never identity);
/// - an **empty map encodes as `"{}"`** — the exact value the shared
///   `Contains` / `Calls` / `Uses` tables' scanned-code rows carry, so a code
///   row and an authored row with no properties are indistinguishable in the
///   column (both decode back to an empty map).
///
/// Infallible: a `BTreeMap<String, String>` always serializes.
pub fn properties_json(properties: &NodeProperties) -> String {
    serde_json::to_string(properties).expect("a string->string map always serializes")
}

/// Decode the **canonical JSON string** stored in the layers projection's
/// serialized-properties column back into the full [`NodeProperties`] map —
/// the exact inverse of [`properties_json`], used by
/// `ArtifactDb::node_files_from_db`.
///
/// Round-trip fidelity mirrors the encoder: every key is preserved, unknown
/// keys and keys carrying an empty value alike, and equal strings decode to
/// equal maps. Two inputs decode to the **empty map**:
/// - `"{}"` — the canonical encoding of an empty map ([`properties_json`]);
/// - the **empty column value** (after trimming) — the projection's
///   representation of an absent/unset column, e.g. a scanned-code row of the
///   shared `Contains`/`Calls`/`Uses` tables, which carries no properties.
///
/// Any other malformed input — invalid JSON, or valid JSON that is not a
/// `string -> string` object (an array, a number, `null`, a non-string value)
/// — is an [`Err`] rather than a silently-partial map, so a corrupt column can
/// never drop a key unnoticed.
pub fn properties_from_json(raw: &str) -> anyhow::Result<NodeProperties> {
    if raw.trim().is_empty() {
        return Ok(NodeProperties::new());
    }
    serde_json::from_str(raw)
        .map_err(|e| anyhow::anyhow!("malformed serialized properties `{raw}`: {e}"))
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
/// no implicit unset. The shared merge helper behind the `node update` /
/// `edge update` command surfaces.
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

// ---------------------------------------------------------------------------
// The in-memory node-file overlay (phase-00 task-1)
// ---------------------------------------------------------------------------

/// The identity of one node file: `(layer, node_type, name)`. The file name IS
/// the identity, so the FQN is derived from it ([`fqn`]), never stored. The key
/// is [`Layer`] (not its dir string) so the FQN rendering reuses [`fqn`]
/// directly.
pub type NodeIdentity = (Layer, String, String);

/// The staged state of one node-file identity: `Some(node)` is a staged write
/// (the identity's new full content), `None` is a delete marker (the identity's
/// file is to be removed).
pub type StagedNode = Option<NodeFile>;

/// An **in-memory overlay** of staged node-file writes and deletes over the
/// on-disk `apg/layers/**` tree (phase-00 task-1) — the seam the session's
/// write-back buffer and `build_change_over` compose on.
///
/// The map keys by node identity `(layer, node_type, name)` and keeps only the
/// last staged state for an identity: a staged write ([`stage_write`](Self::stage_write))
/// or a delete marker ([`stage_delete`](Self::stage_delete)). Reads resolve
/// **staged content before falling back to disk**: a staged write shadows the
/// disk file, a delete marker means the identity is absent even when its file
/// still exists on disk, and an unstaged identity falls through to
/// [`read_node_file`]/[`node_file_path`]. Pure map logic over those disk
/// helpers — it never writes.
///
/// [`apply_to_base`](Self::apply_to_base) folds the overlay over a base node
/// list (the disk store) to yield the cumulative **effective** node set: staged
/// writes replace (or add), delete markers drop, and unstaged disk nodes are
/// kept. [`touched_fqns`](Self::touched_fqns) names every staged identity as
/// its `<layer>.<type>.<name>` FQN — the projection's delete set.
#[derive(Debug, Clone, Default)]
pub struct LayersOverlay {
    /// The staged state per identity: `Some` = write, `None` = delete marker.
    /// Only the last state for an identity survives (the map keys by identity).
    staged: BTreeMap<NodeIdentity, StagedNode>,
}

impl LayersOverlay {
    /// An empty overlay — every identity resolves to disk.
    pub fn new() -> Self {
        Self::default()
    }

    /// An overlay over an existing staged map (the session builds one from its
    /// write-back buffer).
    pub fn from_map(staged: BTreeMap<NodeIdentity, StagedNode>) -> Self {
        Self { staged }
    }

    /// Stage one identity's state (a write `Some(node)` or a delete marker
    /// `None`), replacing any earlier staged state for that identity.
    pub fn insert(&mut self, identity: NodeIdentity, staged: StagedNode) {
        self.staged.insert(identity, staged);
    }

    /// Stage a full node-file write. The identity is the node's own
    /// `layer`/`type`/`name`, so the staged content and the map key agree by
    /// construction. Errors on an unknown layer (a programming error — the
    /// catalog is closed).
    pub fn stage_write(&mut self, node: NodeFile) -> anyhow::Result<()> {
        let layer = Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == node.layer)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
        let identity = (layer, node.node_type.clone(), node.name.clone());
        self.staged.insert(identity, Some(node));
        Ok(())
    }

    /// Stage a delete marker for an identity — it resolves as absent even while
    /// its file still exists on disk.
    pub fn stage_delete(&mut self, layer: Layer, node_type: &str, name: &str) {
        self.staged
            .insert((layer, node_type.to_string(), name.to_string()), None);
    }

    /// Resolve an identity: the staged write if present, `None` if the identity
    /// is a staged delete, else the disk file via [`read_node_file`] (`None`
    /// when no file exists). A malformed on-disk file still errors.
    pub fn read(
        &self,
        apg_root: &Path,
        layer: Layer,
        node_type: &str,
        name: &str,
    ) -> anyhow::Result<Option<NodeFile>> {
        let identity = (layer, node_type.to_string(), name.to_string());
        match self.staged.get(&identity) {
            Some(Some(node)) => Ok(Some(node.clone())),
            Some(None) => Ok(None),
            None => {
                if node_file_path(apg_root, layer, node_type, name).exists() {
                    read_node_file(apg_root, layer, node_type, name).map(Some)
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Whether an identity exists: a staged write is present, a staged delete is
    /// absent, and an unstaged identity falls through to the disk file's
    /// present-ness ([`node_file_path`]).
    pub fn exists(&self, apg_root: &Path, layer: Layer, node_type: &str, name: &str) -> bool {
        let identity = (layer, node_type.to_string(), name.to_string());
        match self.staged.get(&identity) {
            Some(Some(_)) => true,
            Some(None) => false,
            None => node_file_path(apg_root, layer, node_type, name).exists(),
        }
    }

    /// Resolve an identity over the overlay against a **caller-supplied base
    /// node set** — the base-supplied twin of [`read`](Self::read)/[`exists`](Self::exists)
    /// for a caller that holds its durable node universe in memory (e.g. the
    /// session's `ArtifactDb::node_files_from_db`) and must **not** fall back to
    /// `apg/layers/**`. The precedence matches [`read`](Self::read): a staged
    /// write yields its buffered content, a staged delete marker yields `None`,
    /// and an unstaged identity yields the matching base node (or `None` when
    /// the base holds none). Pure map/list logic — no filesystem access, so it
    /// cannot observe a node file the caller's base does not carry.
    ///
    /// A base node whose `layer` directory is not a known [`Layer`] can never
    /// match a `layer` key and is ignored (it is kept verbatim by
    /// [`apply_to_base`](Self::apply_to_base) but has no resolvable identity).
    pub fn over_base(
        &self,
        base: &[NodeFile],
        layer: Layer,
        node_type: &str,
        name: &str,
    ) -> Option<NodeFile> {
        let identity = (layer, node_type.to_string(), name.to_string());
        match self.staged.get(&identity) {
            Some(Some(node)) => Some(node.clone()),
            Some(None) => None,
            None => base
                .iter()
                .find(|node| {
                    node.layer == layer.layer_dir()
                        && node.node_type == node_type
                        && node.name == name
                })
                .cloned(),
        }
    }

    /// Fold the overlay over a base node list (the disk store) into the
    /// cumulative **effective** node set: staged writes replace the matching
    /// base node (or add one absent from the base), delete markers drop the
    /// matching base node, and every unstaged base node is kept. A base node
    /// whose `layer` is unknown is kept verbatim (it can never match a staged
    /// [`Layer`] key). Deterministic: the base's order is preserved and new
    /// staged writes are appended in identity order.
    pub fn apply_to_base(&self, base: &[NodeFile]) -> Vec<NodeFile> {
        let mut effective: Vec<NodeFile> = Vec::with_capacity(base.len());
        let mut seen: BTreeSet<NodeIdentity> = BTreeSet::new();
        for node in base {
            let identity = Layer::ALL
                .iter()
                .find(|l| l.layer_dir() == node.layer)
                .copied()
                .map(|layer| (layer, node.node_type.clone(), node.name.clone()));
            let Some(identity) = identity else {
                effective.push(node.clone());
                continue;
            };
            seen.insert(identity.clone());
            match self.staged.get(&identity) {
                Some(None) => {} // delete marker — drop the disk node
                Some(Some(staged)) => effective.push(staged.clone()), // staged write wins
                None => effective.push(node.clone()), // unstaged — keep the disk node
            }
        }
        // Staged writes for identities absent from the base are additions.
        for (identity, staged) in &self.staged {
            if seen.contains(identity) {
                continue;
            }
            if let Some(node) = staged {
                effective.push(node.clone());
            }
        }
        effective
    }

    /// Every staged identity (writes **and** delete markers) rendered as its
    /// `<layer>.<type>.<name>` FQN — the projection's touched/delete set. Sorted
    /// and deduplicated by the underlying [`BTreeMap`]/[`BTreeSet`].
    pub fn touched_fqns(&self) -> BTreeSet<String> {
        self.staged
            .keys()
            .map(|(layer, node_type, name)| fqn(*layer, node_type, name))
            .collect()
    }
}
