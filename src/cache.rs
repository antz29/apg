//! Win-B incremental content: the **content-addressed manifest** and the shared
//! **content-addressed fact store** (phase-02 tasks 1 and 2).
//!
//! Two concerns live here:
//!
//! 1. [`Manifest`] — a `path -> git blob OID` map recorded with each scan
//!    (`rust.apg.cache`, task-1). Delta/reuse decisions compare blob OIDs,
//!    never mtime: touching a file without changing a byte keeps the same OID
//!    (its cached facts are reused); a byte edit changes the OID and
//!    invalidates them. The OID is git's own blob hash of the working-tree
//!    bytes ([`git2::Oid::hash_object`]), so the manifest is exactly the git
//!    content identity of the tree.
//!
//! 2. [`FactStore`] — the shared content-addressed store
//!    (`rust.apg.cache.FactStore`, task-2). The store root is the repository's
//!    **git common dir** (`<git-common-dir>/apg/facts`), shared across every
//!    worktree and branch of the repo. It holds per-file **fact units** keyed
//!    by content OID + resolution inputs + the global cache key, and serves
//!    per-worktree projections: a unit stored from one worktree can be read
//!    back from a fresh, near-identical worktree (same relative path + content
//!    OID) and re-based onto the reading worktree's absolute paths.
//!
//! The global cache key ([`CacheKey`], `domain.value.cache-key`) is the
//! version/format/config identity that invalidates the whole cache on drift:
//! binary version, scanner JSONL schema/format, ingestor projection rules, and
//! the scan config (languages, excludes, modules).

// This module lands the phase-02 win-B content-addressed API; a few of its
// accessors (e.g. `FactStore::has`, `Manifest::rebase_root`) are consumed by
// the phase-03 splice path and the test suites rather than by `cmd_scan`
// today, so the whole module's surface is deliberately public.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graph::{Graph, Location, Node, NodeKind};
use crate::specs;

/// The scanner JSONL schema/format version folded into [`CacheKey`]. Bump when
/// the wire format changes in a way that invalidates stored facts.
pub const JSONL_SCHEMA_VERSION: &str = "1";

/// The ingestor projection-rule version folded into [`CacheKey`]. Bump when the
/// ingestor's FQN rendering / node projection changes so cached units produced
/// by an older projection are discarded.
pub const PROJECTION_RULES_VERSION: &str = "1";

/// The subdirectory under the git common dir that hosts the shared store.
pub const STORE_DIR: &str = "apg";
/// The leaf directory name of the shared fact store.
pub const FACTS_DIR: &str = "facts";

// ---------------------------------------------------------------------------
// Global cache key (domain.value.cache-key)
// ---------------------------------------------------------------------------

/// The scan config identity folded into the global cache key: the exact set of
/// languages, path excludes, and module restrictions a scan ran with. A change
/// to any of them invalidates the whole cache (the projection could differ).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanConfigKey {
    pub languages: Vec<String>,
    pub excludes: Vec<String>,
    pub modules: Vec<String>,
}

/// The version/format/config identity that invalidates the whole cache when it
/// drifts (`domain.value.cache-key`): binary version + JSONL schema/format +
/// ingestor projection rules + scan config. A mismatch forces a full scan for
/// correctness, never as a heuristic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKey {
    pub binary_version: String,
    pub jsonl_schema: String,
    pub projection: String,
    pub config: String,
}

impl CacheKey {
    /// Builds the key for a scan config: the binary's own version plus the
    /// pinned schema/projection versions plus a stable digest of the scan
    /// config (a sorted, `\0`-joined rendering so ordering never matters).
    pub fn compute(config: &ScanConfigKey) -> CacheKey {
        let mut parts: Vec<String> = Vec::new();
        for (tag, vals) in [
            ("lang", &config.languages),
            ("exclude", &config.excludes),
            ("module", &config.modules),
        ] {
            let mut sorted = vals.clone();
            sorted.sort();
            for v in sorted {
                parts.push(format!("{tag}={v}"));
            }
        }
        CacheKey {
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            jsonl_schema: JSONL_SCHEMA_VERSION.to_string(),
            projection: PROJECTION_RULES_VERSION.to_string(),
            config: digest_str(&parts.join("\0")),
        }
    }

    /// The key rendered as one filesystem-safe, collision-resistant token used
    /// as the per-key directory name in the store.
    pub fn token(&self) -> String {
        digest_str(&format!(
            "{}|{}|{}|{}",
            self.binary_version, self.jsonl_schema, self.projection, self.config
        ))
    }

    /// True when this key is compatible with `other` (identical in every
    /// component) — the cache-key drift predicate the fallback uses.
    pub fn matches(&self, other: &CacheKey) -> bool {
        self == other
    }
}

// ---------------------------------------------------------------------------
// Content-addressed manifest (rust.apg.cache, task-1)
// ---------------------------------------------------------------------------

/// A `path -> git blob OID` manifest of a checkout's working tree. Paths are
/// **checkout-relative** with `/` separators so the manifest is portable across
/// worktrees of the same repository (cross-worktree fact sharing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The (canonical) absolute scan root the manifest was built from — the
    /// base a reading worktree re-bases stored relative paths onto.
    #[serde(default)]
    pub root: String,
    pub entries: BTreeMap<String, String>,
}

/// The three-way delta between two manifests: paths present only in the newer
/// manifest, paths whose OID changed, and paths that disappeared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestDelta {
    pub added: BTreeSet<String>,
    pub modified: BTreeSet<String>,
    pub removed: BTreeSet<String>,
}

impl ManifestDelta {
    /// True when nothing changed between the two manifests.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    /// Every changed path (added ∪ modified ∪ removed).
    pub fn changed(&self) -> BTreeSet<String> {
        self.added
            .iter()
            .chain(&self.modified)
            .chain(&self.removed)
            .cloned()
            .collect()
    }
}

/// The git blob OID of a byte slice — git's own content hash
/// (`Oid::hash_object(ObjectType::Blob, …)`), so a manifest OID is exactly the
/// OID git would assign the same bytes.
pub fn blob_oid_of_bytes(bytes: &[u8]) -> String {
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
        .map(|o| o.to_string())
        .unwrap_or_default()
}

/// The git blob OID of a file's current bytes, or `None` when it cannot be
/// read (missing, a directory, an unreadable symlink target).
pub fn blob_oid_of_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(blob_oid_of_bytes(&bytes))
}

/// Directories never walked when building a manifest: git's own store, linked
/// worktrees, and the build/dependency trees the scanner excludes. The
/// gitignore check additionally drops `apg/.trans/` and any other ignored
/// content.
const SKIP_DIRS: &[&str] = &[".git", ".worktrees", "target", "node_modules"];

impl Manifest {
    /// Builds the manifest for the scan root `root` (the scanned directory, an
    /// absolute path) by walking its tree and hashing every non-ignored file's
    /// bytes. The root itself is recorded (`root`) so a reading worktree can
    /// re-base stored paths. Gitignored content (the scan's own `apg/.trans/`
    /// outputs, `.worktrees/`) never enters — the manifest is a content
    /// identity of the *scanned* tree, so a scan's own writes cannot invalidate
    /// it.
    pub fn build(root: &Path) -> Manifest {
        let base = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let repo = git2::Repository::discover(&base).ok();
        let mut entries = BTreeMap::new();
        walk_files(&base, &base, repo.as_ref(), &mut entries);
        Manifest {
            root: base.to_string_lossy().into_owned(),
            entries,
        }
    }

    /// Persists the manifest as JSON under the shared store root
    /// (`<store-root>/manifest.json`).
    pub fn save(&self, store_root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(store_root)?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(store_root.join("manifest.json"), json)?;
        Ok(())
    }

    /// Loads a manifest previously saved under the shared store root, or
    /// `None` when none exists / it is unreadable.
    pub fn load(store_root: &Path) -> Option<Manifest> {
        let text = std::fs::read_to_string(store_root.join("manifest.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The OID recorded for `rel`, if any.
    pub fn oid(&self, rel: &str) -> Option<&str> {
        self.entries.get(rel).map(String::as_str)
    }

    /// Re-bases a relative path recorded under this manifest's root onto
    /// `reader_root` (`/`-joined).
    pub fn rebase_root(&self, reader_root: &str) -> String {
        reader_root.to_string()
    }

    /// The three-way delta from `self` (older) to `other` (newer).
    pub fn diff(&self, other: &Manifest) -> ManifestDelta {
        let mut delta = ManifestDelta::default();
        for (rel, oid) in &other.entries {
            match self.entries.get(rel) {
                None => {
                    delta.added.insert(rel.clone());
                }
                Some(old) if old != oid => {
                    delta.modified.insert(rel.clone());
                }
                Some(_) => {}
            }
        }
        for rel in self.entries.keys() {
            if !other.entries.contains_key(rel) {
                delta.removed.insert(rel.clone());
            }
        }
        delta
    }
}

/// Recursively hashes every non-ignored regular file under `dir` into `out`,
/// keyed by its `/`-separated path relative to `base`.
fn walk_files(
    base: &Path,
    dir: &Path,
    repo: Option<&git2::Repository>,
    out: &mut BTreeMap<String, String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            // Ignored directories (`apg/.trans/`, `.worktrees/` when ignored)
            // never enter the manifest.
            if repo.is_some_and(|r| r.status_should_ignore(&p).unwrap_or(false)) {
                continue;
            }
            walk_files(base, &p, repo, out);
        } else {
            if name.starts_with(".git") {
                continue;
            }
            if repo.is_some_and(|r| r.status_should_ignore(&p).unwrap_or(false)) {
                continue;
            }
            if let Some(oid) = blob_oid_of_file(&p)
                && let Ok(rel) = p.strip_prefix(base)
            {
                out.insert(rel.to_string_lossy().replace('\\', "/"), oid);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fact units (per-file resolved fragments)
// ---------------------------------------------------------------------------

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

/// Re-bases an absolute path FQN stored from `stored_root` onto `reader_root`;
/// identifiers (which never carry a worktree-root prefix) pass through.
fn rebase(stored_root: &str, reader_root: &str, s: &str) -> String {
    match s.strip_prefix(stored_root) {
        Some(tail) if !tail.is_empty() && (tail.starts_with('/') || tail.starts_with('\\')) => {
            format!("{reader_root}{tail}")
        }
        _ => s.to_string(),
    }
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

// ---------------------------------------------------------------------------
// The shared content-addressed fact store (rust.apg.cache.FactStore, task-2)
// ---------------------------------------------------------------------------

/// An index entry mapping a byte-identity to the stored unit that realizes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactIndexEntry {
    /// The unit's file name under `<root>/<lang>/<cache-key>/`.
    pub unit: String,
    /// The resolution-inputs digest the unit was stored under (the unit is
    /// reusable only when the current file's inputs digest equals this).
    pub inputs: String,
    /// The absolute path the unit was written from (for cross-worktree
    /// path re-basing).
    pub root: String,
    /// The absolute source path the unit was written for.
    pub abs_path: String,
}

/// The shared content-addressed fact store. Root:
/// `<git-common-dir>/apg/facts` — shared across every worktree and branch of
/// the repository, so a fresh worktree reuses the main checkout's units.
pub struct FactStore {
    pub root: PathBuf,
    index: BTreeMap<String, FactIndexEntry>,
}

impl FactStore {
    /// Resolves the store for the repository containing `apg_root`: the repo's
    /// **common** git dir (shared by linked worktrees) plus `apg/facts`.
    pub fn resolve(apg_root: &Path) -> anyhow::Result<FactStore> {
        let repo = git2::Repository::discover(apg_root)?;
        let common = common_git_dir(&repo);
        let root = common.join(STORE_DIR).join(FACTS_DIR);
        Ok(FactStore {
            root,
            index: BTreeMap::new(),
        })
    }

    /// The same store rooted at an explicit directory (tests and callers that
    /// already resolved the common dir).
    pub fn at(root: PathBuf) -> FactStore {
        FactStore {
            root,
            index: BTreeMap::new(),
        }
    }

    /// Loads the store's index from disk (or starts empty when absent).
    pub fn load(mut self) -> FactStore {
        let path = self.root.join("index.json");
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(idx) = serde_json::from_str::<BTreeMap<String, FactIndexEntry>>(&text)
        {
            self.index = idx;
        }
        self
    }

    /// Persists the index (call after writes).
    pub fn save_index(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let json = serde_json::to_string_pretty(&self.index)?;
        std::fs::write(self.root.join("index.json"), json)?;
        Ok(())
    }

    /// The per-key directory: `<root>/<lang>/<cache-key-token>/`.
    fn key_dir(&self, lang: &str, cache_key: &CacheKey) -> PathBuf {
        self.root.join(lang).join(cache_key.token())
    }

    /// The index key for a file's byte identity under one cache key.
    fn index_key(lang: &str, rel: &str, oid: &str, cache_key: &CacheKey) -> String {
        format!("{lang}|{rel}|{oid}|{}", cache_key.token())
    }

    /// True when a unit for this byte identity + inputs + cache key exists.
    pub fn has(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        inputs: &str,
        cache_key: &CacheKey,
    ) -> bool {
        self.index
            .get(&Self::index_key(lang, rel, oid, cache_key))
            .is_some_and(|e| e.inputs == inputs)
    }

    /// Reads the reusable unit for `(lang, rel, oid)` under `cache_key` —
    /// `Some` only when the stored unit's resolution-inputs digest equals
    /// `inputs`. A byte-identical file whose inputs drifted is NOT reusable.
    pub fn reuse(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        inputs: &str,
        cache_key: &CacheKey,
    ) -> Option<(FileFragment, String)> {
        let entry = self
            .index
            .get(&Self::index_key(lang, rel, oid, cache_key))?;
        if entry.inputs != inputs {
            return None;
        }
        let text = std::fs::read_to_string(self.key_dir(lang, cache_key).join(&entry.unit)).ok()?;
        let frag: FileFragment = serde_json::from_str(&text).ok()?;
        Some((frag, entry.root.clone()))
    }

    /// Reads the stored candidate unit for `(lang, rel, oid)` regardless of
    /// input drift, with the writing root — used to discover prior inputs.
    pub fn candidate(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        cache_key: &CacheKey,
    ) -> Option<(FileFragment, String)> {
        let entry = self
            .index
            .get(&Self::index_key(lang, rel, oid, cache_key))?;
        let text = std::fs::read_to_string(self.key_dir(lang, cache_key).join(&entry.unit)).ok()?;
        let frag: FileFragment = serde_json::from_str(&text).ok()?;
        Some((frag, entry.root.clone()))
    }

    /// Writes a unit. The unit's file name is a digest over content OID +
    /// resolution-inputs digest + the global cache key, so identical
    /// content+inputs+key always maps to the same stored unit (content
    /// addressing) even across worktrees.
    pub fn put(
        &mut self,
        frag: &FileFragment,
        writer_root: &str,
        cache_key: &CacheKey,
    ) -> anyhow::Result<()> {
        let dir = self.key_dir(&frag.lang, cache_key);
        std::fs::create_dir_all(&dir)?;
        let inputs = frag.inputs_digest();
        let unit = format!(
            "{}.json",
            digest_str(&format!(
                "{}|{}|{}|{}",
                frag.blob_oid,
                inputs,
                cache_key.token(),
                frag.rel_path
            ))
        );
        std::fs::write(dir.join(&unit), serde_json::to_string(frag)?)?;
        let entry = FactIndexEntry {
            unit,
            inputs,
            root: writer_root.to_string(),
            abs_path: abs_path_of(writer_root, &frag.rel_path),
        };
        self.index.insert(
            Self::index_key(&frag.lang, &frag.rel_path, &frag.blob_oid, cache_key),
            entry,
        );
        Ok(())
    }

    /// The number of indexed units (tests).
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// True when the store holds no units.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

/// The git **common** dir of a repository — for a linked worktree,
/// `<main>/.git`; for the main checkout, `<checkout>/.git`. Read from the
/// gitdir's `commondir` file when present (the linked-worktree case), else the
/// gitdir itself. Canonicalized when possible; falling back to the lexical
/// path.
pub fn common_git_dir(repo: &git2::Repository) -> PathBuf {
    let gitdir = repo.path();
    let rel = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match rel {
        Some(rel) => {
            let joined = gitdir.join(rel);
            std::fs::canonicalize(&joined).unwrap_or(joined)
        }
        None => std::fs::canonicalize(gitdir).unwrap_or_else(|_| gitdir.to_path_buf()),
    }
}

/// The absolute path a relative path names under `root` (`/`-joined).
pub fn abs_path_of(root: &str, rel: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

/// The checkout-relative path of an absolute path under `root`, or the path
/// unchanged when it is not under `root`.
pub fn rel_path_of(root: &Path, abs: &str) -> String {
    Path::new(abs)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs.to_string())
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

/// A stable, dependency-free 128-bit FNV-1a hex digest of a string. Equal bytes
/// always give equal digests across processes; it is only ever compared within
/// the same binary's rule.
pub fn digest_str(s: &str) -> String {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    for (i, b) in s.bytes().enumerate() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 ^= b as u64 ^ (i as u64);
        h2 = h2.wrapping_mul(0x0000_0100_0000_01b3).rotate_left(5);
    }
    format!("{h1:016x}{h2:016x}")
}

/// The store root the scan records facts into for `apg_root`, or `None` when
/// `apg_root` is not inside a git repo (no shared store to resolve).
pub fn store_root_for(apg_root: &Path) -> Option<PathBuf> {
    FactStore::resolve(apg_root).ok().map(|s| s.root)
}

/// Convenience: the shared store root under a checkout's `apg/` layout
/// (`<git-common-dir>/apg/facts`).
pub fn store_root_under_apg(apg_root: &Path) -> Option<PathBuf> {
    let _ = specs::TRANS;
    store_root_for(apg_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;

    #[test]
    fn cache_key_tracks_version_schema_projection_and_config() {
        let base = ScanConfigKey {
            languages: vec!["go".into()],
            excludes: vec![],
            modules: vec![],
        };
        let k = CacheKey::compute(&base);
        // The binary version is folded in.
        assert_eq!(k.binary_version, env!("CARGO_PKG_VERSION"));
        // A config drift (an added exclude) changes the key.
        let mut cfg2 = base.clone();
        cfg2.excludes.push("vendor".into());
        let k2 = CacheKey::compute(&cfg2);
        assert!(!k.matches(&k2));
        // Ordering of the config lists never matters (sorted rendering).
        let mut cfg3 = base.clone();
        cfg3.languages.push("java".into());
        let mut cfg4 = ScanConfigKey {
            languages: vec!["java".into(), "go".into()],
            ..base.clone()
        };
        let _ = &mut cfg4;
        assert_eq!(
            CacheKey::compute(&cfg3).config,
            CacheKey::compute(&cfg4).config
        );
        // Identical configs render identical keys.
        assert!(CacheKey::compute(&base).matches(&CacheKey::compute(&base)));
    }

    #[test]
    fn manifest_blob_oid_is_content_addressed_not_mtime() {
        let dir = std::env::temp_dir().join(format!("apg-cache-oid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.go");
        std::fs::write(&f, "package a\n").unwrap();
        let oid1 = blob_oid_of_file(&f).unwrap();

        // Touching without changing bytes keeps the OID (mtime ignored).
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        let times = std::fs::FileTimes::new().set_modified(later);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(times)
            .unwrap();
        assert_eq!(blob_oid_of_file(&f).unwrap(), oid1);

        // A byte edit changes it.
        std::fs::write(&f, "package a\n\nvar X = 1\n").unwrap();
        assert_ne!(blob_oid_of_file(&f).unwrap(), oid1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_diff_reports_added_modified_removed() {
        let mut older = Manifest::default();
        older.entries.insert("a.go".into(), "oid-a".into());
        older.entries.insert("b.go".into(), "oid-b".into());
        let mut newer = Manifest::default();
        newer.entries.insert("a.go".into(), "oid-a".into()); // unchanged
        newer.entries.insert("b.go".into(), "oid-b2".into()); // modified
        newer.entries.insert("c.go".into(), "oid-c".into()); // added
        let d = older.diff(&newer);
        assert!(d.added.contains("c.go"));
        assert!(d.modified.contains("b.go"));
        assert!(d.removed.is_empty());
        assert_eq!(d.changed().len(), 2);
        // A deletion is reported as removed.
        let d2 = newer.diff(&older);
        assert!(d2.removed.contains("c.go"));
        assert!(d2.modified.contains("b.go"));
        assert!(d2.added.is_empty());
    }

    #[test]
    fn fact_store_reuses_only_on_bytes_and_inputs_and_key() {
        let dir = std::env::temp_dir().join(format!("apg-cache-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        let mut g = Graph::default();
        g.nodes.insert(
            "fixture.mod".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/w/a.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: PathBuf::from("/w/a.go"),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 3,
                }),
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/w/a.go.A".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: PathBuf::from("/w/a.go"),
                    start: 0,
                    end: 3,
                    start_line: 1,
                    end_line: 3,
                }),
                code_type: "src".into(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("/w/a.go".to_string(), "/w/a.go.A".to_string()));
        g.contains
            .insert(("fixture.mod".to_string(), "/w/a.go".to_string()));
        // A function with a non-empty param list: the declaration surface the
        // signature early-cutoff compares must survive the cache round-trip.
        g.nodes.insert(
            "/w/a.go.F".to_string(),
            Node {
                kind: NodeKind::Function,
                location: Some(Location {
                    path: PathBuf::from("/w/a.go"),
                    start: 1,
                    end: 2,
                    start_line: 2,
                    end_line: 2,
                }),
                params: vec!["int".into()],
                code_type: "src".into(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("/w/a.go".to_string(), "/w/a.go.F".to_string()));
        let frag = FileFragment::from_graph(&g, "/w/a.go", "a.go", "oid-a", "go");
        let inputs = frag.inputs_digest();
        store.put(&frag, "/w", &cache_key).unwrap();

        // Same bytes + same inputs + same key ⇒ reusable.
        assert!(store.has("go", "a.go", "oid-a", &inputs, &cache_key));
        let (got, root) = store
            .reuse("go", "a.go", "oid-a", &inputs, &cache_key)
            .unwrap();
        assert_eq!(root, "/w");
        let (mods, nodes, edges) = got.project("/w", "/fresh");
        assert_eq!(mods, vec!["fixture.mod".to_string()]);
        // The function's params survive the round-trip (the signature cutoff
        // would otherwise see an empty list after one incremental generation).
        let f = nodes
            .iter()
            .find(|(fqn, _)| fqn == "/fresh/a.go.F")
            .expect("the cached function projects");
        assert_eq!(f.1.params, vec!["int".to_string()]);
        // The File node's FQN (an absolute path) is re-based onto the reader's
        // root; a path-derived struct FQN is re-based too, while an
        // identifier-shaped FQN (the module) passes through unchanged.
        assert!(nodes.iter().any(|(f, _)| f == "/fresh/a.go"));
        assert!(nodes.iter().any(|(f, _)| f == "/fresh/a.go.A"));
        assert!(edges.iter().any(|e| e.kind == "contains"));

        // Different bytes (new OID) ⇒ not reusable.
        assert!(!store.has("go", "a.go", "oid-b", &inputs, &cache_key));
        // Same bytes but drifted resolution inputs ⇒ not reusable.
        assert!(!store.has("go", "a.go", "oid-a", "different", &cache_key));
        assert!(
            store
                .reuse("go", "a.go", "oid-a", "different", &cache_key)
                .is_none()
        );
        // Cache-key drift ⇒ not reusable.
        let drifted = CacheKey::compute(&ScanConfigKey {
            languages: vec!["java".into()],
            ..Default::default()
        });
        assert!(!store.has("go", "a.go", "oid-a", &inputs, &drifted));
        let _ = std::fs::remove_dir_all(tmp_root(&dir));
    }

    fn tmp_root(dir: &Path) -> PathBuf {
        // The store root is nested; remove the whole scratch.
        dir.parent().unwrap().to_path_buf()
    }

    #[test]
    fn cross_worktree_unit_is_rebased_but_content_addressed_once() {
        let dir = std::env::temp_dir().join(format!("apg-cache-xwt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        let mut g = Graph::default();
        g.nodes.insert(
            "fixture.mod".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/main/src/a.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: PathBuf::from("/main/src/a.go"),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 2,
                }),
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/main/src/a.go.A".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: PathBuf::from("/main/src/a.go"),
                    start: 0,
                    end: 2,
                    start_line: 1,
                    end_line: 2,
                }),
                code_type: "src".into(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("fixture.mod".to_string(), "/main/src/a.go".to_string()));
        let frag = FileFragment::from_graph(&g, "/main/src/a.go", "src/a.go", "oid-a", "go");
        let inputs = frag.inputs_digest();
        store.put(&frag, "/main", &cache_key).unwrap();

        // The fresh worktree has the SAME relative path + bytes (sharing keys
        // are relative), so it reuses the unit, re-based onto its own root.
        assert!(store.has("go", "src/a.go", "oid-a", &inputs, &cache_key));
        let (frag2, root) = store
            .reuse("go", "src/a.go", "oid-a", &inputs, &cache_key)
            .unwrap();
        let (mods, nodes, _) = frag2.project(&root, "/fresh");
        // Module FQNs are identifiers, the File FQN is re-based.
        assert_eq!(mods, vec!["fixture.mod".to_string()]);
        assert!(
            nodes
                .iter()
                .any(|(f, n)| f == "/fresh/src/a.go" && n.location.is_some())
        );
        assert!(nodes.iter().any(|(f, _)| f == "/fresh/src/a.go.A"));
        // Exactly one stored unit for the shared content.
        assert_eq!(store.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
