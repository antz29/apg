//! Win-B impact closure + signature early cutoff (phase-02 tasks 5 and 6).
//!
//! The re-emission target set is **not** just the changed files:
//!
//! ```text
//! changed files
//!   ∪ reverse-dependency closure over the cached file-level dep index
//!   ∪ overload-group peers
//! ```
//!
//! with a **signature early cutoff**: a body-only change whose exported
//! signature is unchanged does not cascade to dependents.
//!
//! ## Granularity and signatures (DECIDED per language, task-6)
//!
//! A frontend's *emission granularity* is the unit it can re-emit on a targeted
//! scan:
//!
//! | language | granularity |
//! |---|---|
//! | Java | package |
//! | Go | package |
//! | Rust | crate |
//! | TypeScript | file |
//! | C# | project |
//! | Python | package/module |
//! | C++ | file |
//! | Markdown | file |
//!
//! The **exported signature** of a unit is its declaration surface: for a
//! struct, its FQN + kind; for a function, its FQN + arity/params; for a file,
//! the sorted set of its declared FQNs. A body-only edit that leaves every
//! declared FQN and every function's arity/params unchanged has an unchanged
//! signature and does not pull dependents into the target set.

// The phase-02 win-B surface: several helpers here (e.g. `target_set`,
// `signature_changed_files`, `by_language`) are consumed by the impact unit
// tests and the phase-03 orchestration rather than by `cmd_scan` directly.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::graph::{Graph, NodeKind};

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

// ---------------------------------------------------------------------------
// File-level dependency index (reverse-dependency closure)
// ---------------------------------------------------------------------------

/// The file-level dependency index derived from a resolved graph: for each
/// source file, the set of files whose declared units it references (calls /
/// uses / unresolved-target edges). The reverse index maps a file to the files
/// that depend on it.
#[derive(Debug, Clone, Default)]
pub struct DepIndex {
    /// file → files it references (its dependencies).
    pub forward: BTreeMap<String, BTreeSet<String>>,
    /// file → files that reference it (its dependents).
    pub reverse: BTreeMap<String, BTreeSet<String>>,
}

impl DepIndex {
    /// Builds the index from a resolved graph: maps each edge's endpoints to the
    /// file that declares them (via node locations), then records both
    /// directions (skipping self-edges).
    pub fn from_graph(graph: &Graph) -> DepIndex {
        // FQN → declaring file (located nodes only; File nodes are their own
        // path).
        let mut file_of: HashMap<&str, &str> = HashMap::new();
        for (fqn, node) in &graph.nodes {
            match node.kind {
                NodeKind::File => {
                    // The File node's FQN *is* its absolute path.
                    file_of.insert(fqn, fqn);
                }
                NodeKind::Struct | NodeKind::Function => {
                    if let Some(loc) = &node.location {
                        file_of.insert(fqn.as_str(), loc.path.to_str().unwrap_or_default());
                    }
                }
                _ => {}
            }
        }
        let mut idx = DepIndex::default();
        let mut edge_files = |a: &str, b: &str| {
            let (Some(fa), Some(fb)) = (file_of.get(a).copied(), file_of.get(b).copied()) else {
                return;
            };
            if fa == fb || fa.is_empty() || fb.is_empty() {
                return;
            }
            idx.forward
                .entry(fa.to_string())
                .or_default()
                .insert(fb.to_string());
        };
        for (a, b) in &graph.calls {
            edge_files(a, b);
        }
        for (a, b) in &graph.uses {
            edge_files(a, b);
        }
        for (a, b, _) in &graph.unresolved_calls {
            edge_files(a, b);
        }
        for (a, b) in &graph.unresolved_uses {
            edge_files(a, b);
        }
        // Build the reverse index from the forward one (deterministic).
        for (f, deps) in &idx.forward {
            for d in deps {
                idx.reverse.entry(d.clone()).or_default().insert(f.clone());
            }
        }
        idx
    }

    /// The reverse-dependency closure of `seeds`: the seeds plus every file
    /// that transitively depends on them (BFS over the reverse index).
    pub fn reverse_closure(&self, seeds: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = seeds.clone();
        let mut queue: Vec<String> = seeds.iter().cloned().collect();
        while let Some(f) = queue.pop() {
            if let Some(deps) = self.reverse.get(&f) {
                for d in deps {
                    if out.insert(d.clone()) {
                        queue.push(d.clone());
                    }
                }
            }
        }
        out
    }

    /// True when no dependency information is available (an empty index).
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// The forward edges as a sorted `(dependent, dependency)` list of
    /// **checkout-relative** paths — the portable, persisted form. (The first
    /// element is the file that references the second.)
    pub fn edges_rel(&self, root: &std::path::Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (f, deps) in &self.forward {
            for d in deps {
                out.push((rel(root, f), rel(root, d)));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// An absolute path relative to `root`, `/`-separated; unchanged when not under
/// `root`.
fn rel(root: &std::path::Path, abs: &str) -> String {
    std::path::Path::new(abs)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs.to_string())
}

/// Rebuilds a [`DepIndex`] from persisted checkout-relative forward edges with
/// the **relative** keys kept as-is — the form the signature cascade works in
/// (its seeds and its answers are checkout-relative).
pub fn dep_index_rel(edges: &[(String, String)]) -> DepIndex {
    let mut idx = DepIndex::default();
    for (dependent, dependency) in edges {
        idx.forward
            .entry(dependent.clone())
            .or_default()
            .insert(dependency.clone());
        idx.reverse
            .entry(dependency.clone())
            .or_default()
            .insert(dependent.clone());
    }
    idx
}

/// Rebuilds a [`DepIndex`] from persisted checkout-relative forward edges
/// (each `(dependent, dependency)`) and the current scan root, so the index
/// addresses the current worktree's files.
pub fn dep_index_from_edges(edges: &[(String, String)], root: &std::path::Path) -> DepIndex {
    let join = |p: &str| -> String {
        if std::path::Path::new(p).is_absolute() {
            p.to_string()
        } else {
            root.join(p).to_string_lossy().into_owned()
        }
    };
    let mut idx = DepIndex::default();
    for (dependent, dependency) in edges {
        let (dependent, dependency) = (join(dependent), join(dependency));
        idx.forward
            .entry(dependent.clone())
            .or_default()
            .insert(dependency.clone());
        idx.reverse.entry(dependency).or_default().insert(dependent);
    }
    idx
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

// ---------------------------------------------------------------------------
// The impact closure
// ---------------------------------------------------------------------------

/// The **re-emission target set** for a delta, derived from the cached graph:
///
/// ```text
/// changed files
///   ∪ reverse-dependency closure (unless every changed file is body-only)
///   ∪ overload-group peers of changed files
/// ```
///
/// `signature_changed` is the set of changed files whose exported signature
/// actually changed; a changed file NOT in this set is body-only and does not
/// pull its dependents. (An added/removed file always counts as a signature
/// change — its declaration surface appeared or vanished.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetSet {
    pub files: BTreeSet<String>,
    /// The subset of `files` that were pulled in as reverse dependencies of a
    /// signature change (diagnostics / logging).
    pub dependents: BTreeSet<String>,
    /// The subset of `files` pulled in as overload peers (diagnostics).
    pub overload_peers: BTreeSet<String>,
}

impl TargetSet {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether `rel`/`abs` is in the target set.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }
}

/// Computes the re-emission target set.
///
/// * `changed` — the changed files (absolute paths) from the git delta/manifest.
/// * `signature_changed` — those changed files whose exported signature changed.
///   Files in `changed` but not here are body-only and do not cascade.
/// * `index` — the cached file-level dependency index.
/// * `cached_graph` — the previous graph (for the overload groups).
pub fn target_set(
    changed: &BTreeSet<String>,
    signature_changed: &BTreeSet<String>,
    index: &DepIndex,
    cached_graph: &Graph,
) -> TargetSet {
    let mut out = TargetSet {
        files: changed.clone(),
        ..TargetSet::default()
    };

    // Reverse-dependency closure: only a signature change cascades. A
    // body-only change (in `changed` but not `signature_changed`) is cut off.
    let cascade_seeds: BTreeSet<String> =
        changed.intersection(signature_changed).cloned().collect();
    if !cascade_seeds.is_empty() {
        let closure = index.reverse_closure(&cascade_seeds);
        for f in &closure {
            if !out.files.contains(f) {
                out.files.insert(f.clone());
                out.dependents.insert(f.clone());
            }
        }
    }

    // Overload-group peers: a changed file containing a function pulls in every
    // file declaring a function under the same scope (the FQNs re-suffix
    // graph-wide). Scope-based so a newly-added overload is still covered.
    let mut scopes: BTreeSet<String> = BTreeSet::new();
    for f in changed {
        scopes.extend(overload_groups_of_file(cached_graph, f));
    }
    if !scopes.is_empty() {
        let peers = overload_peer_files(cached_graph, &scopes);
        for p in peers {
            if out.files.insert(p.clone()) {
                out.overload_peers.insert(p);
            }
        }
    }

    out
}

/// The portable (checkout-relative) target closure: changed ∪ reverse-dep
/// closure (of a signature change) ∪ overload peers. Returns checkout-relative
/// paths — the entry point a fresh worktree uses directly.
pub fn target_set_rel(
    changed_rel: &BTreeSet<String>,
    signature_changed_rel: &BTreeSet<String>,
    index: &DepIndex,
    overloads: &OverloadIndex,
) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = changed_rel.clone();

    let cascade_seeds: BTreeSet<String> = changed_rel
        .intersection(signature_changed_rel)
        .cloned()
        .collect();
    if !cascade_seeds.is_empty() {
        out.extend(index.reverse_closure(&cascade_seeds));
    }

    for f in changed_rel {
        out.extend(overloads.peer_files(f));
    }
    out
}

/// Restricts a target set to files whose language is one of `languages` (the
/// per-language run/skip verdict). A language whose intersection is empty and
/// unchanged is skipped.
pub fn by_language(
    files: &BTreeSet<String>,
    language_of: impl Fn(&str) -> Option<String>,
    language: &str,
) -> BTreeSet<String> {
    files
        .iter()
        .filter(|f| language_of(f).as_deref() == Some(language))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Location, Node};
    use std::path::PathBuf;

    fn node(kind: NodeKind, path: &str) -> Node {
        Node {
            kind,
            location: Some(Location {
                path: PathBuf::from(path),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            }),
            ..Node::default()
        }
    }

    fn put(g: &mut Graph, fqn: &str, kind: NodeKind, path: &str) {
        g.nodes.insert(fqn.to_string(), node(kind, path));
    }

    #[test]
    fn granularity_is_decided_per_language() {
        assert_eq!(granularity("java"), Granularity::Package);
        assert_eq!(granularity("go"), Granularity::Package);
        assert_eq!(granularity("rust"), Granularity::Crate);
        assert_eq!(granularity("ts"), Granularity::File);
        assert_eq!(granularity("csharp"), Granularity::Project);
        assert_eq!(granularity("python"), Granularity::PackageOrModule);
        assert_eq!(granularity("cpp"), Granularity::File);
        assert_eq!(granularity("markdown"), Granularity::File);
        assert_eq!(granularity("unknown-lang"), Granularity::File);
    }

    #[test]
    fn reverse_dep_closure_pulls_dependents() {
        // a.go declares A; b.go depends on A; c.go depends on b.
        let mut g = Graph::default();
        put(&mut g, "m.A", NodeKind::Struct, "/r/a.go");
        put(&mut g, "m.B", NodeKind::Struct, "/r/b.go");
        put(&mut g, "m.C", NodeKind::Struct, "/r/c.go");
        put(&mut g, "/r/a.go", NodeKind::File, "/r/a.go");
        put(&mut g, "/r/b.go", NodeKind::File, "/r/b.go");
        put(&mut g, "/r/c.go", NodeKind::File, "/r/c.go");
        // b uses A, c uses B (file-level dependency chain).
        g.uses.insert(("m.B".into(), "m.A".into()));
        g.uses.insert(("m.C".into(), "m.B".into()));
        let idx = DepIndex::from_graph(&g);
        assert!(idx.forward["/r/b.go"].contains("/r/a.go"));
        let closure = idx.reverse_closure(&BTreeSet::from(["/r/a.go".to_string()]));
        assert!(closure.contains("/r/a.go"));
        assert!(closure.contains("/r/b.go"));
        assert!(closure.contains("/r/c.go"));

        // A leaf change with a signature change emits the leaf + dependents.
        let changed = BTreeSet::from(["/r/a.go".to_string()]);
        let ts = target_set(&changed, &changed, &idx, &g);
        assert!(ts.contains("/r/a.go"));
        assert!(ts.contains("/r/b.go"));
        assert!(ts.contains("/r/c.go"));
        assert!(ts.dependents.contains("/r/b.go"));

        // A body-only change is cut off: only the leaf is emitted.
        let body_only = BTreeSet::new();
        let ts = target_set(&changed, &body_only, &idx, &g);
        assert_eq!(ts.files, changed);
        assert!(ts.dependents.is_empty());
    }

    #[test]
    fn overload_group_peers_are_emitted() {
        // Two overloads of m.C.foo live in a.go and b.go; changing one must emit
        // the whole scope's files.
        let mut g = Graph::default();
        put(&mut g, "m.C.foo(int)", NodeKind::Function, "/r/a.go");
        put(&mut g, "m.C.foo(string)", NodeKind::Function, "/r/b.go");
        put(&mut g, "/r/a.go", NodeKind::File, "/r/a.go");
        put(&mut g, "/r/b.go", NodeKind::File, "/r/b.go");
        g.calls
            .insert(("m.C.foo(int)".into(), "m.C.foo(string)".into()));
        let scopes = overload_groups_of_file(&g, "/r/a.go");
        assert!(scopes.contains("m.C"));
        let peers = overload_peer_files(&g, &scopes);
        assert!(peers.contains("/r/a.go"));
        assert!(peers.contains("/r/b.go"));

        let idx = DepIndex::from_graph(&g);
        let changed = BTreeSet::from(["/r/a.go".to_string()]);
        let ts = target_set(&changed, &changed, &idx, &g);
        assert!(ts.overload_peers.contains("/r/b.go"));
        assert!(ts.contains("/r/b.go"));

        // A newly-added overload: the changed file declares only the new
        // signature, but its scope already holds the existing sibling in b.go —
        // the existing peer is still emitted (scope-based, not group-based).
        let mut new_graph = g.clone();
        put(
            &mut new_graph,
            "m.C.foo(bool)",
            NodeKind::Function,
            "/r/a.go",
        );
        let scopes = overload_groups_of_file(&new_graph, "/r/a.go");
        assert!(scopes.contains("m.C"));
        let peers = overload_peer_files(&new_graph, &scopes);
        assert!(
            peers.contains("/r/b.go"),
            "the existing sibling must re-emit"
        );
    }

    #[test]
    fn signature_cutoff_distinguishes_body_and_signature_changes() {
        let mut old = Graph::default();
        put(&mut old, "m.C.foo", NodeKind::Function, "/r/a.go");
        put(&mut old, "m.C.bar", NodeKind::Function, "/r/a.go");
        let mut newb = old.clone();
        // Body-only: no declaration surface change.
        assert!(signature_unchanged(&old, &newb, "/r/a.go"));

        // Signature change: an added declaration / changed params.
        put(&mut newb, "m.C.baz", NodeKind::Function, "/r/a.go");
        assert!(!signature_unchanged(&old, &newb, "/r/a.go"));

        // A changed arity: foo() -> foo(int) re-suffixes the overload.
        let mut old2 = Graph::default();
        put(&mut old2, "m.C.foo", NodeKind::Function, "/r/a.go");
        let mut new2 = Graph::default();
        put(&mut new2, "m.C.foo(int)", NodeKind::Function, "/r/a.go");
        assert!(!signature_unchanged(&old2, &new2, "/r/a.go"));
    }
}
