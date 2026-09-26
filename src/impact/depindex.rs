use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::graph::{Graph, NodeKind};

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
pub(crate) fn rel(root: &std::path::Path, abs: &str) -> String {
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
