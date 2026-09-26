//! Per-file fact units, the single-pass per-file index, and the module
//! scaffolding — split out of the former single-file `src/cache.rs` (phase-04
//! task-19 wave F). Behaviour preserving: `crate::cache::<name>` keeps
//! resolving through the re-exports in [`crate::cache`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::{digest_str, kind_str, rel_path_of};
use crate::graph::{Graph, Location, Node, NodeKind};

/// One node of a per-file fact unit: the minimal, serde-round-trippable shape
/// needed to rebuild a [`Node`] without re-running a frontend. FQNs are already
/// resolved (the unit is a slice of the resolved graph), so a unit stored for
/// one worktree composes with emitted facts by FQN — never by opaque id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragNode {
    pub fqn: String,
    /// `module` | `file` | `struct` | `function` | `unresolved`.
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub start: u32,
    #[serde(default)]
    pub end: u32,
    #[serde(default)]
    pub start_line: u32,
    #[serde(default)]
    pub end_line: u32,
    #[serde(default)]
    pub code_type: String,
    /// The erased parameter list of a Function node. It is the declaration
    /// surface the win-B signature early-cutoff compares, so it must survive the
    /// cache round-trip: without it a reused function would present an empty
    /// param list and a body-only edit could be misjudged as a signature change.
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

impl FragNode {
    fn of(fqn: &str, node: &Node) -> FragNode {
        let loc = node.location.as_ref();
        FragNode {
            fqn: fqn.to_string(),
            kind: kind_str(node.kind).to_string(),
            path: loc
                .map(|l| l.path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            start: loc.map(|l| l.start).unwrap_or(0),
            end: loc.map(|l| l.end).unwrap_or(0),
            start_line: loc.map(|l| l.start_line).unwrap_or(0),
            end_line: loc.map(|l| l.end_line).unwrap_or(0),
            code_type: node.code_type.clone(),
            params: node.params.clone(),
            category: node.category.clone(),
            status: node.status.clone(),
        }
    }

    /// Rebuilds a [`Node`]; `rebase` rewrites the stored absolute path onto the
    /// reading worktree's root (cross-worktree projection).
    fn to_node(&self, rebase: &dyn Fn(&str) -> String) -> Node {
        let kind = match self.kind.as_str() {
            "module" => NodeKind::Module,
            "file" => NodeKind::File,
            "struct" => NodeKind::Struct,
            "function" => NodeKind::Function,
            _ => NodeKind::UnresolvedTarget,
        };
        let location = if self.path.is_empty() {
            None
        } else {
            Some(Location {
                path: PathBuf::from(rebase(&self.path)),
                start: self.start,
                end: self.end,
                start_line: self.start_line,
                end_line: self.end_line,
            })
        };
        Node {
            kind,
            location,
            params: self.params.clone(),
            category: self.category.clone(),
            code_type: self.code_type.clone(),
            status: self.status.clone(),
            ..Node::default()
        }
    }
}

/// One edge of a per-file fact unit, authored by the file's own units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragEdge {
    /// `contains` | `calls` | `uses` | `unresolved_call` | `unresolved_use`.
    pub kind: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub target_type: String,
}

/// Re-bases a stored identity onto the **repo-relative** identity base
/// (`requirements.requirement.portable-graph-identity`): a stored ABSOLUTE path
/// (a legacy fact unit written from a checkout root) is stripped of its writer
/// root — or of the reading checkout's own root — to its `/`-separated tail; an
/// identifier or an already repo-relative identity passes through unchanged.
///
/// `reader_root` is the reading checkout's base. The stored graph is
/// checkout-independent, so a unit written in one worktree composes verbatim in
/// another at the same commit (task-6: cross-checkout reuse) — but a unit whose
/// identity is an absolute spelling of the READER's own tree (e.g. a fragment
/// re-projected under the reader) must normalize to the same repo-relative tail
/// rather than leak the checkout path. Two clean checkouts of one commit
/// therefore mint the SAME identity for the same file.
pub fn rebase(stored_root: &str, reader_root: &str, s: &str) -> String {
    for root in [stored_root, reader_root] {
        if root.is_empty() {
            continue;
        }
        if let Some(tail) = s.strip_prefix(root)
            && !tail.is_empty()
            && (tail.starts_with('/') || tail.starts_with('\\'))
        {
            return tail.trim_start_matches(['/', '\\']).to_string();
        }
    }
    s.to_string()
}

/// A **per-file fact unit**: the resolved graph fragment for one source file,
/// plus the module FQNs it belongs to and the set of FQNs it references (its
/// *resolution inputs*). Content-addressed by the file's OID + these inputs +
/// the global cache key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFragment {
    /// Checkout-relative path (the portable identity across worktrees).
    pub rel_path: String,
    /// The content OID this unit was produced from.
    pub blob_oid: String,
    /// The language the file was scanned as.
    pub lang: String,
    /// Module FQNs this file belongs to (a file belongs to one module; the
    /// module record is emitted per module, so the unit carries the FQNs).
    pub modules: Vec<String>,
    /// Declared nodes: the File node plus its Struct/Function children and any
    /// UnresolvedTarget placeholders referenced by this file's edges.
    pub nodes: Vec<FragNode>,
    /// Edges authored by this file's units (from = a unit declared here).
    pub edges: Vec<FragEdge>,
    /// The resolution inputs of this unit: every project FQN referenced by its
    /// edges (targets and unresolved targets). A unit is reusable only when the
    /// file's bytes AND these inputs are unchanged.
    pub inputs: BTreeSet<String>,
}

impl FileFragment {
    /// The resolution-inputs digest folded into the unit key.
    pub fn inputs_digest(&self) -> String {
        let joined: Vec<&str> = self.inputs.iter().map(String::as_str).collect();
        digest_str(&joined.join("\0"))
    }

    /// Extracts the fragment for one absolute source path from a resolved
    /// graph. `declared` is the file's File-node FQN + every Struct/Function
    /// node located in it; edges are attributed to the file of their `from`
    /// node.
    pub fn from_graph(
        graph: &Graph,
        abs_path: &str,
        rel_path: &str,
        blob_oid: &str,
        lang: &str,
    ) -> FileFragment {
        let mut frag = FileFragment {
            rel_path: rel_path.to_string(),
            blob_oid: blob_oid.to_string(),
            lang: lang.to_string(),
            ..FileFragment::default()
        };

        // Declared nodes located in this file, plus the File node keyed by the
        // absolute path.
        let mut declared: BTreeSet<String> = BTreeSet::new();
        for (fqn, node) in &graph.nodes {
            let in_file = match node.kind {
                NodeKind::File => fqn == abs_path,
                NodeKind::Struct | NodeKind::Function => node
                    .location
                    .as_ref()
                    .is_some_and(|l| l.path.to_string_lossy() == abs_path),
                _ => false,
            };
            if in_file {
                declared.insert(fqn.clone());
                frag.nodes.push(FragNode::of(fqn, node));
            }
        }
        // The module FQNs this file belongs to (from the File node's contains
        // in-edge and any module containment of the file).
        for (a, b) in &graph.contains {
            if b == abs_path
                && graph
                    .nodes
                    .get(a)
                    .is_some_and(|n| n.kind == NodeKind::Module)
            {
                frag.modules.push(a.clone());
            }
        }
        frag.modules.sort();
        frag.modules.dedup();

        // Edges authored by this file's declared units.
        let mut edges: Vec<FragEdge> = Vec::new();
        let mut inputs: BTreeSet<String> = BTreeSet::new();
        let push = |edges: &mut Vec<FragEdge>,
                    inputs: &mut BTreeSet<String>,
                    kind: &str,
                    from: &str,
                    to: &str,
                    target_type: &str| {
            if declared.contains(from) {
                edges.push(FragEdge {
                    kind: kind.to_string(),
                    from: from.to_string(),
                    to: to.to_string(),
                    target_type: target_type.to_string(),
                });
                inputs.insert(to.to_string());
            }
        };
        for (a, b) in &graph.contains {
            push(&mut edges, &mut inputs, "contains", a, b, "");
        }
        // A Module→File edge is authored by neither endpoint's file (the module
        // has no file and the File node is the target). Store it on the file's
        // unit so a cached projection can rebuild the containment.
        for (a, b) in &graph.contains {
            if b == abs_path
                && graph
                    .nodes
                    .get(a)
                    .is_some_and(|n| n.kind == NodeKind::Module)
            {
                edges.push(FragEdge {
                    kind: "contains".to_string(),
                    from: a.clone(),
                    to: b.clone(),
                    target_type: String::new(),
                });
                inputs.insert(a.clone());
            }
        }
        for (a, b) in &graph.calls {
            push(&mut edges, &mut inputs, "calls", a, b, "");
        }
        for (a, b) in &graph.uses {
            push(&mut edges, &mut inputs, "uses", a, b, "");
        }
        for (a, b, t) in &graph.unresolved_calls {
            push(&mut edges, &mut inputs, "unresolved_call", a, b, t);
        }
        for (a, b) in &graph.unresolved_uses {
            push(&mut edges, &mut inputs, "unresolved_use", a, b, "");
        }
        // Unresolved-target rows referenced by this file's edges travel with the
        // unit so a projection can rebuild them without a re-scan.
        for e in &edges {
            if matches!(e.kind.as_str(), "unresolved_call" | "unresolved_use")
                && let Some(n) = graph.nodes.get(&e.to)
            {
                frag.nodes.push(FragNode::of(&e.to, n));
            }
        }
        frag.edges = edges;
        frag.inputs = inputs;
        frag.nodes.sort_by(|a, b| a.fqn.cmp(&b.fqn));
        frag.nodes.dedup_by(|a, b| a.fqn == b.fqn);
        frag.edges
            .sort_by(|a, b| (&a.kind, &a.from, &a.to).cmp(&(&b.kind, &b.from, &b.to)));
        frag.edges.dedup();
        frag
    }

    /// Derives this file's fragment from a one-pass [`FileIndex`] (phase-04
    /// task-12). Identical to [`FileFragment::from_graph`] for the same graph
    /// and arguments, but it reads only this file's index buckets instead of
    /// rescanning the whole node map and all five edge sets — so building the
    /// ~1 003 located files' fragments costs ONE graph traversal, not
    /// `O(F*(N+E))`.
    ///
    /// `from_graph` stays the pre-fix reference oracle (task-20 compares the
    /// two); the production scan records through this method.
    pub fn from_index(
        index: &FileIndex,
        abs_path: &str,
        rel_path: &str,
        blob_oid: &str,
        lang: &str,
    ) -> FileFragment {
        let mut frag = FileFragment {
            rel_path: rel_path.to_string(),
            blob_oid: blob_oid.to_string(),
            lang: lang.to_string(),
            ..FileFragment::default()
        };

        if let Some(nodes) = index.nodes_by_file.get(abs_path) {
            frag.nodes.extend(nodes.iter().cloned());
        }
        // Edges authored by this file's declared units.
        if let Some(edges) = index.edges_by_file.get(abs_path) {
            for e in edges {
                frag.inputs.insert(e.to.clone());
                frag.edges.push(e.clone());
            }
        }
        // A `Module -> File` edge is authored by neither endpoint's file (the
        // module has no file and the File node is the target). Store it on the
        // file's unit so a cached projection can rebuild the containment.
        if let Some(mods) = index.module_edges_by_file.get(abs_path) {
            for e in mods {
                frag.modules.push(e.from.clone());
                frag.inputs.insert(e.from.clone());
                frag.edges.push(e.clone());
            }
        }
        // Unresolved-target rows referenced by this file's edges travel with the
        // unit so a projection can rebuild them without a re-scan.
        for e in &frag.edges {
            if matches!(e.kind.as_str(), "unresolved_call" | "unresolved_use")
                && let Some(n) = index.node_rows.get(&e.to)
            {
                frag.nodes.push(n.clone());
            }
        }
        frag.modules.sort();
        frag.modules.dedup();
        frag.nodes.sort_by(|a, b| a.fqn.cmp(&b.fqn));
        frag.nodes.dedup_by(|a, b| a.fqn == b.fqn);
        frag.edges
            .sort_by(|a, b| (&a.kind, &a.from, &a.to).cmp(&(&b.kind, &b.from, &b.to)));
        frag.edges.dedup();
        frag
    }

    /// Projects this unit onto a worktree: rebuilds its nodes (re-basing
    /// absolute paths from the writing worktree onto `reader_root`) and edges.
    /// Returns `(module_fqns, nodes, edges)`.
    pub fn project(
        &self,
        stored_root: &str,
        reader_root: &str,
    ) -> (Vec<String>, Vec<(String, Node)>, Vec<FragEdge>) {
        let nodes = self
            .nodes
            .iter()
            .map(|n| {
                (
                    rebase(stored_root, reader_root, &n.fqn),
                    n.to_node(&|p| rebase(stored_root, reader_root, p)),
                )
            })
            .collect();
        let edges = self
            .edges
            .iter()
            .map(|e| FragEdge {
                kind: e.kind.clone(),
                from: rebase(stored_root, reader_root, &e.from),
                to: rebase(stored_root, reader_root, &e.to),
                target_type: e.target_type.clone(),
            })
            .collect();
        (self.modules.clone(), nodes, edges)
    }
}

/// A **one-pass per-file index** over an assembled [`Graph`].
///
/// [`FileFragment::from_graph`] rescans the whole node map and all five code
/// edge sets for *each* file — `O(F*(N+E))`, ≈95 M iterations for the ~1 003
/// located files of the staged jgrapht copy. [`FileIndex::build`] traverses the
/// graph ONCE instead: every node is bucketed by the file it is located in and
/// every edge by the file of its `from` unit (a `Module -> File` edge lands on
/// the target File's bucket), so each file's fragment is derived from its own
/// bucket alone — [`FileFragment::from_index`].
///
/// The builder is **pure** — it reads only the in-memory graph (no filesystem,
/// database, git or process) — so the index property stays unit-testable.
#[derive(Debug, Default)]
pub struct FileIndex {
    /// File key -> the File/Struct/Function nodes located in it.
    nodes_by_file: BTreeMap<String, Vec<FragNode>>,
    /// File key -> the edges authored by that file's declared units.
    edges_by_file: BTreeMap<String, Vec<FragEdge>>,
    /// File key -> the `Module -> File` contains edges targeting it.
    module_edges_by_file: BTreeMap<String, Vec<FragEdge>>,
    /// Every node's fragment row, keyed by FQN — resolves the row an unresolved
    /// edge carries with the unit.
    node_rows: BTreeMap<String, FragNode>,
    /// The number of nodes visited while building (`== |nodes|`).
    node_visits: usize,
    /// The number of code-edge entries visited while building
    /// (`== |contains|+|calls|+|uses|+|unresolved_calls|+|unresolved_uses|`).
    edge_visits: usize,
}

impl FileIndex {
    /// Builds the index in ONE pass over the graph: every node once, every
    /// entry of the five code edge sets once.
    pub fn build(graph: &Graph) -> FileIndex {
        let mut index = FileIndex::default();
        for (fqn, node) in &graph.nodes {
            index.node_visits += 1;
            let row = FragNode::of(fqn, node);
            match node.kind {
                // A File node keys by its FQN (which is its absolute path).
                NodeKind::File => index
                    .nodes_by_file
                    .entry(fqn.clone())
                    .or_default()
                    .push(row.clone()),
                // A Struct/Function keys by the file it is located in.
                NodeKind::Struct | NodeKind::Function => {
                    if let Some(loc) = &node.location {
                        index
                            .nodes_by_file
                            .entry(loc.path.to_string_lossy().into_owned())
                            .or_default()
                            .push(row.clone());
                    }
                }
                _ => {}
            }
            index.node_rows.insert(fqn.clone(), row);
        }
        for (a, b) in &graph.contains {
            index.edge_visits += 1;
            if let Some(key) = Self::file_key_of(graph, a) {
                index.edges_by_file.entry(key).or_default().push(FragEdge {
                    kind: "contains".to_string(),
                    from: a.clone(),
                    to: b.clone(),
                    target_type: String::new(),
                });
            } else if graph
                .nodes
                .get(a)
                .is_some_and(|n| n.kind == NodeKind::Module)
            {
                // A `Module -> File` edge is authored by neither endpoint's
                // file, so it lands on the target File's bucket.
                index
                    .module_edges_by_file
                    .entry(b.clone())
                    .or_default()
                    .push(FragEdge {
                        kind: "contains".to_string(),
                        from: a.clone(),
                        to: b.clone(),
                        target_type: String::new(),
                    });
            }
        }
        for (a, b) in &graph.calls {
            index.edge_visits += 1;
            index.push_edge(graph, a, b, "calls", "");
        }
        for (a, b) in &graph.uses {
            index.edge_visits += 1;
            index.push_edge(graph, a, b, "uses", "");
        }
        for (a, b, t) in &graph.unresolved_calls {
            index.edge_visits += 1;
            index.push_edge(graph, a, b, "unresolved_call", t);
        }
        for (a, b) in &graph.unresolved_uses {
            index.edge_visits += 1;
            index.push_edge(graph, a, b, "unresolved_use", "");
        }
        index
    }

    /// The file key a node's `from` role contributes its edges to: a File node
    /// keys by its FQN, a Struct/Function by its location path, and every other
    /// kind (Module, UnresolvedTarget, spec nodes) by no file.
    fn file_key_of(graph: &Graph, fqn: &str) -> Option<String> {
        let node = graph.nodes.get(fqn)?;
        match node.kind {
            NodeKind::File => Some(fqn.to_string()),
            NodeKind::Struct | NodeKind::Function => node
                .location
                .as_ref()
                .map(|l| l.path.to_string_lossy().into_owned()),
            _ => None,
        }
    }

    /// Buckets one edge authored by a file's declared unit (a no-op for an edge
    /// whose `from` unit is not located in any file).
    fn push_edge(&mut self, graph: &Graph, from: &str, to: &str, kind: &str, target_type: &str) {
        if let Some(key) = Self::file_key_of(graph, from) {
            self.edges_by_file.entry(key).or_default().push(FragEdge {
                kind: kind.to_string(),
                from: from.to_string(),
                to: to.to_string(),
                target_type: target_type.to_string(),
            });
        }
    }

    /// The number of nodes visited while building (`== |nodes|`).
    pub fn node_visits(&self) -> usize {
        self.node_visits
    }

    /// The number of code-edge entries visited while building
    /// (`== |contains|+|calls|+|uses|+|unresolved_calls|+|unresolved_uses|`).
    pub fn edge_visits(&self) -> usize {
        self.edge_visits
    }

    /// Every file key the index carries a bucket for (tests/oracles).
    pub fn files(&self) -> BTreeSet<String> {
        self.nodes_by_file
            .keys()
            .chain(self.edges_by_file.keys())
            .chain(self.module_edges_by_file.keys())
            .cloned()
            .collect()
    }
}

/// One language's **global module scaffolding**: every real `Module` node and
/// every `Module -> Module` `contains` edge that language's frontend emitted.
///
/// A per-file fact unit carries only the file's **direct-parent** module and the
/// `Module -> File` edge (a file belongs to exactly one module), so a
/// pure-intermediate `Module` (one with no `File` child) and every
/// `Module -> Module` hierarchy edge are invisible to the fact cache. A partial
/// scan that skips an unchanged language's frontend therefore cannot rebuild
/// that scaffolding from the cached per-file units alone — the win-B-assembled
/// graph (and the `graph.jsonl` rendered from it) would be structurally
/// incomplete even though the spliced DB kept the seed's rows. This record is
/// stored per language under the same cache-key dir as the fact units and
/// replayed whenever that language's frontend is skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleScaffolding {
    /// The scaffolding's `Module` FQNs (sorted, deduped).
    pub modules: Vec<String>,
    /// The scaffolding's `Module -> Module` `contains` edges `(from, to)`
    /// (sorted, deduped).
    pub edges: Vec<(String, String)>,
    /// The `Language` node FQNs this scaffolding roots (PHASE_09 language
    /// rooting) — the bare scan language ids. `#[serde(default)]` so a unit
    /// written before the field existed decodes as empty.
    #[serde(default)]
    pub languages: Vec<String>,
    /// The `Language -> Module` `contains` edges `(from, to)` (sorted, deduped).
    #[serde(default)]
    pub language_edges: Vec<(String, String)>,
}

impl ModuleScaffolding {
    /// Extracts every language's module scaffolding from a resolved graph.
    ///
    /// Only **real** modules are considered — a `planned` placeholder (a
    /// transient tier-4 record) is not scanned scaffolding. A module's language
    /// is the language of a source `File` attached directly beneath it,
    /// propagated across the undirected `Module -> Module` component so
    /// pure-intermediate modules (no file of their own) and file-less
    /// descendants (e.g. an inline `mod tests`) are attributed to the same
    /// language as the files that anchor the component. A component with no
    /// attached file anywhere is an empty/planned placeholder and is dropped.
    pub fn extract(graph: &Graph, scan_root: &Path) -> BTreeMap<String, ModuleScaffolding> {
        let real: BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|(_, n)| n.kind == NodeKind::Module && n.status.is_none())
            .map(|(f, _)| f.clone())
            .collect();
        // The `Module -> Module` hierarchy, as an undirected component walk plus
        // the directed edges themselves.
        let mut adjacency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut edges: Vec<(String, String)> = Vec::new();
        for (a, b) in &graph.contains {
            if real.contains(a) && real.contains(b) {
                adjacency.entry(a.clone()).or_default().insert(b.clone());
                adjacency.entry(b.clone()).or_default().insert(a.clone());
                edges.push((a.clone(), b.clone()));
            }
        }
        // A module is anchored to the language of a File directly under it.
        // Deterministic when a module somehow anchors files of two languages:
        // the smallest language label wins.
        let mut label: BTreeMap<String, String> = BTreeMap::new();
        for (a, b) in &graph.contains {
            let anchored = graph.nodes.get(b).is_some_and(|n| n.kind == NodeKind::File);
            if !real.contains(a) || !anchored {
                continue;
            }
            let rel = rel_path_of(scan_root, b);
            let lang = crate::incremental::language_of(&rel).to_string();
            label
                .entry(a.clone())
                .and_modify(|current| {
                    if lang < *current {
                        *current = lang.clone();
                    }
                })
                .or_insert(lang);
        }

        let mut out: BTreeMap<String, ModuleScaffolding> = BTreeMap::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for start in &real {
            if seen.contains(start) {
                continue;
            }
            let mut stack = vec![start.clone()];
            let mut component: BTreeSet<String> = BTreeSet::new();
            let mut langs: BTreeSet<String> = BTreeSet::new();
            while let Some(m) = stack.pop() {
                if !seen.insert(m.clone()) {
                    continue;
                }
                if let Some(l) = label.get(&m) {
                    langs.insert(l.clone());
                }
                component.insert(m.clone());
                if let Some(neighbours) = adjacency.get(&m) {
                    for n in neighbours {
                        if !seen.contains(n) {
                            stack.push(n.clone());
                        }
                    }
                }
            }
            // No file anywhere in the component: an empty/planned placeholder,
            // not scaffolding a scan should replay.
            let Some(lang) = langs.into_iter().next() else {
                continue;
            };
            let entry = out.entry(lang).or_default();
            for m in &component {
                entry.modules.push(m.clone());
            }
            for (a, b) in &edges {
                if component.contains(a) && component.contains(b) {
                    entry.edges.push((a.clone(), b.clone()));
                }
            }
            // PHASE_09: the component's `Language` root(s) and the
            // `Language -> Module` edges, so a skipped language's root survives
            // the incremental assembly.
            for (a, b) in &graph.contains {
                if component.contains(b)
                    && graph
                        .nodes
                        .get(a)
                        .is_some_and(|n| n.kind == NodeKind::Language)
                {
                    entry.languages.push(a.clone());
                    entry.language_edges.push((a.clone(), b.clone()));
                }
            }
        }
        for scaffolding in out.values_mut() {
            scaffolding.modules.sort();
            scaffolding.modules.dedup();
            scaffolding.edges.sort();
            scaffolding.edges.dedup();
            scaffolding.languages.sort();
            scaffolding.languages.dedup();
            scaffolding.language_edges.sort();
            scaffolding.language_edges.dedup();
        }
        out
    }
}
