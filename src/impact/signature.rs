use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::graph::{Graph, NodeKind};

use super::depindex::rel;

// ---------------------------------------------------------------------------
// Per-language granularity / signature (task-6, DECIDED)
// ---------------------------------------------------------------------------

/// The emission granularity of a language (the unit a targeted scan re-emits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// Java/Go: a package (directory) re-emits as a whole.
    Package,
    /// Rust: a crate (Cargo package) re-emits as a whole.
    Crate,
    /// TypeScript/C++/Markdown: a single file re-emits.
    File,
    /// C#: a project re-emits.
    Project,
    /// Python: a package or a flat module re-emits.
    PackageOrModule,
}

/// The DECIDED granularity for a language id. Unknown/unlisted languages fall
/// back to per-file granularity (the finest safe unit).
pub fn granularity(language: &str) -> Granularity {
    match language {
        "java" => Granularity::Package,
        "go" => Granularity::Package,
        "rust" => Granularity::Crate,
        "ts" | "javascript" | "js" => Granularity::File,
        "csharp" | "c#" => Granularity::Project,
        "python" | "py" => Granularity::PackageOrModule,
        "cpp" | "c++" | "c" => Granularity::File,
        "md" | "markdown" => Granularity::File,
        _ => Granularity::File,
    }
}

/// The **exported signature** of one declared unit: the declaration surface a
/// dependent can observe. Two graphs with the same signatures for a unit are
/// body-equivalent (a body-only change does not cascade).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Signature {
    /// `struct` | `function`.
    pub kind: String,
    /// The declared FQN.
    pub fqn: String,
    /// For functions, the erased parameter list (arity/params); empty otherwise.
    pub params: Vec<String>,
    /// The containing scope (a function's `parent`; a struct's own FQN). Two
    /// functions sharing a scope are potential overload peers **even after a
    /// newly-added overload**: an overload must share its parent, so pulling in
    /// every file that declares a function under a changed file's scope is the
    /// exact peer rule.
    pub parent: String,
}

/// The signature set of a file: every Struct/Function declared in it, with
/// functions carrying their parameter list. A body-only edit leaves the set
/// unchanged; adding/removing a declaration or changing a function's params
/// changes it.
pub fn file_signature(graph: &Graph, abs_path: &str) -> BTreeSet<Signature> {
    let mut out = BTreeSet::new();
    for (fqn, node) in &graph.nodes {
        match node.kind {
            NodeKind::Struct | NodeKind::Function => {
                let here = node
                    .location
                    .as_ref()
                    .is_some_and(|l| l.path.to_string_lossy() == abs_path);
                if here {
                    out.insert(Signature {
                        kind: kind_str(node.kind).to_string(),
                        fqn: fqn.clone(),
                        params: params_of_node(node),
                        parent: signature_parent(fqn, node.kind),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::File => "file",
        NodeKind::Struct => "struct",
        NodeKind::Function => "function",
        _ => "unresolved",
    }
}

/// The containing scope of a declared unit: a function's `parent` (stripping any
/// overload suffix); a struct's own FQN.
fn signature_parent(fqn: &str, kind: NodeKind) -> String {
    match kind {
        NodeKind::Function => overload_key(fqn)
            .map(|(parent, _)| parent)
            .unwrap_or_else(|| fqn.to_string()),
        _ => fqn.to_string(),
    }
}

/// The erased parameter list of a function node: the node's carried `params`
/// when present (the exact declaration surface, independent of overload
/// suffixing), else the `(T1,T2)` suffix parsed from the FQN.
fn params_of_node(node: &crate::graph::Node) -> Vec<String> {
    if node.kind != NodeKind::Function {
        return Vec::new();
    }
    if !node.params.is_empty() {
        return node.params.clone();
    }
    Vec::new()
}

/// The erased parameter list of a function FQN as the ingestor rendered it: the
/// `(T1,T2)` suffix of an overloaded FQN, split on commas; empty for a
/// non-overloaded (unsuffixed) function or a struct.
fn params_of(fqn: &str, kind: NodeKind) -> Vec<String> {
    if kind != NodeKind::Function {
        return Vec::new();
    }
    let Some(open) = fqn.rfind('(') else {
        return Vec::new();
    };
    let Some(close) = fqn.rfind(')') else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    let inner = &fqn[open + 1..close];
    if inner.is_empty() {
        // A zero-param overload still carries the empty `()` suffix — an
        // unambiguous arity-0 declaration distinct from a unique function.
        vec![String::new()]
    } else {
        inner.split(',').map(str::to_string).collect()
    }
}

/// The **signature delta** between two graphs for one file: signatures present
/// in `new` but not `old` (changed/added), and vice versa (removed). Empty both
/// ways ⇒ body-only change ⇒ no cascade.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignatureDelta {
    pub changed: BTreeSet<Signature>,
    pub removed: BTreeSet<Signature>,
}

impl SignatureDelta {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Compares the exported signatures of one file between the cached (old) graph
/// and a freshly assembled (new) graph.
pub fn signature_delta(old: &BTreeSet<Signature>, new: &BTreeSet<Signature>) -> SignatureDelta {
    SignatureDelta {
        changed: new.difference(old).cloned().collect(),
        removed: old.difference(new).cloned().collect(),
    }
}

/// True when the file's signature is unchanged between the two graphs (the
/// early-cutoff predicate — a body-only change does not cascade).
pub fn signature_unchanged(old: &Graph, new: &Graph, abs_path: &str) -> bool {
    signature_delta(
        &file_signature(old, abs_path),
        &file_signature(new, abs_path),
    )
    .is_empty()
}

/// A portable (checkout-relative) file → exported-signature-set map, persisted
/// alongside the dependency index so a fresh worktree can compute a
/// signature-only delta without re-deriving the whole previous graph.
pub type SignatureMap = BTreeMap<String, BTreeSet<Signature>>;

/// Extracts each located Struct/Function file's signature set, keyed by its
/// checkout-relative path under `root`.
pub fn signatures_of_graph(graph: &Graph, root: &std::path::Path) -> SignatureMap {
    let mut out: SignatureMap = BTreeMap::new();
    for (fqn, node) in &graph.nodes {
        if !matches!(node.kind, NodeKind::Struct | NodeKind::Function) {
            continue;
        }
        let Some(loc) = &node.location else {
            continue;
        };
        out.entry(rel(root, &loc.path.to_string_lossy()))
            .or_default()
            .insert(Signature {
                kind: kind_str(node.kind).to_string(),
                fqn: fqn.clone(),
                params: params_of_node(node),
                parent: signature_parent(fqn, node.kind),
            });
    }
    out
}

/// The checkout-relative files whose exported signature changed between two
/// signature maps, plus files added or removed. A file present in only one map
/// is a signature change (its declaration surface appeared or vanished).
pub fn signature_changed_files(old: &SignatureMap, new: &SignatureMap) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (rel, new_sigs) in new {
        match old.get(rel) {
            None => {
                out.insert(rel.clone());
            }
            Some(old_sigs) if old_sigs != new_sigs => {
                out.insert(rel.clone());
            }
            Some(_) => {}
        }
    }
    for rel in old.keys() {
        if !new.contains_key(rel) {
            out.insert(rel.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Overload-group peers
// ---------------------------------------------------------------------------

/// The function **scopes** declared in one file: the `parent` (containing type
/// or package) of every function it declares. Two functions sharing a scope are
/// potential overload peers, so a change to one pulls in every file declaring a
/// function under the same scope — exact even when a newly-added overload changes
/// the group (the parent is unchanged by the addition).
pub fn overload_groups_of_file(graph: &Graph, abs_path: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (fqn, node) in &graph.nodes {
        if node.kind != NodeKind::Function {
            continue;
        }
        let in_file = node
            .location
            .as_ref()
            .is_some_and(|l| l.path.to_string_lossy() == abs_path);
        if in_file && let Some((parent, _name)) = overload_key(fqn) {
            out.insert(parent);
        }
    }
    out
}

/// The `(parent, simple-name)` of a function FQN, stripping any `(params)`
/// overload suffix. `None` for a non-function-shaped FQN.
fn overload_key(fqn: &str) -> Option<(String, String)> {
    let bare = match fqn.rfind('(') {
        Some(open) => &fqn[..open],
        None => fqn,
    };
    let (parent, name) = bare.rsplit_once('.')?;
    Some((parent.to_string(), name.to_string()))
}

/// The files declaring any function under one of `scopes` — the overload peer
/// files to pull into the target set.
pub fn overload_peer_files(graph: &Graph, scopes: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (fqn, node) in &graph.nodes {
        if node.kind != NodeKind::Function {
            continue;
        }
        if let Some((parent, _)) = overload_key(fqn)
            && scopes.contains(&parent)
            && let Some(loc) = &node.location
        {
            out.insert(loc.path.to_string_lossy().into_owned());
        }
    }
    out
}

/// The portable form of [`overload_groups_of_file`] + its peer file set: the
/// function **scopes** declared in each file. Persisted so a fresh worktree
/// computes peers without the previous graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverloadIndex {
    /// Function scope (`parent`) → the declaring FQNs under it.
    pub scopes: BTreeMap<String, Vec<String>>,
    /// relative file path → the function scopes its functions belong to.
    pub file_scopes: BTreeMap<String, BTreeSet<String>>,
}

impl OverloadIndex {
    /// Builds the portable overload index from a graph, keyed checkout-relative
    /// under `root`. Every function scope enters (a singleton scope can receive
    /// a new overload, so it is still a peer group).
    pub fn from_graph(graph: &Graph, root: &std::path::Path) -> OverloadIndex {
        let mut scopes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut file_scopes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (fqn, node) in &graph.nodes {
            if node.kind != NodeKind::Function {
                continue;
            }
            let Some((parent, _name)) = overload_key(fqn) else {
                continue;
            };
            scopes.entry(parent.clone()).or_default().push(fqn.clone());
            if let Some(loc) = &node.location {
                file_scopes
                    .entry(rel(root, &loc.path.to_string_lossy()))
                    .or_default()
                    .insert(parent);
            }
        }
        OverloadIndex {
            scopes,
            file_scopes,
        }
    }

    /// The checkout-relative peer files of the function scopes declared in
    /// `rel_changed` (a checkout-relative path). Empty when the file declares no
    /// function.
    pub fn peer_files(&self, rel_changed: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let Some(scopes) = self.file_scopes.get(rel_changed) else {
            return out;
        };
        for s in scopes {
            for (f, fs) in &self.file_scopes {
                if fs.contains(s) {
                    out.insert(f.clone());
                }
            }
        }
        out
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.file_scopes.is_empty()
    }
}
