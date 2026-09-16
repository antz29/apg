//! Win-B incremental-scan orchestration (phase-02 task-8): the glue that turns
//! the content-addressed manifest ([`crate::cache`]), the git delta + fallbacks
//! ([`crate::delta`]), and the impact closure ([`crate::impact`]) into the
//! per-scan state `apg scan` needs.
//!
//! The persisted state lives under the shared store root
//! (`<git-common-dir>/apg/facts`, [`crate::cache::FactStore`]) so it is shared
//! across every worktree and branch:
//!
//! * `manifest.json` — the last scan's content manifest (path → blob OID),
//! * `scan.json` — the last scan's commit sha + global cache key,
//! * `index/deps.json` — the file-level dependency index (checkout-relative),
//! * `index/signatures.json` — each file's exported signature set,
//! * `index/overloads.json` — overload groups (checkout-relative),
//! * `<lang>/<cache-key>/` — the per-file fact units.
//!
//! Paths are checkout-relative throughout, so a fresh worktree of the same
//! repository reads the same state and re-bases it onto its own root — the
//! cross-worktree sharing half of `requirements.requirement.cross-worktree-cache-sharing`.

// The phase-02 win-B surface: a few helpers are consumed by the phase-03 splice
// path and the integration tests rather than by `cmd_scan` directly.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::cache::{CacheKey, FactStore, FileFragment, Manifest, ScanConfigKey};
use crate::delta::{self, Delta, FullScanReason, ScanRecord};
use crate::graph::Graph;
use crate::impact::{DepIndex, OverloadIndex, SignatureMap, signatures_of_graph};

/// One reusable file: a fact unit to splice into the win-B assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReuseFile {
    /// Absolute path in the current worktree.
    pub abs: String,
    /// Checkout-relative path (the cross-worktree identity).
    pub rel: String,
    /// The content OID of the current bytes.
    pub oid: String,
    /// The language the unit was produced as.
    pub lang: String,
}

/// The durable orchestration state persisted under the store root.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Checkout-relative forward dependency edges `(dependency, dependent)`.
    pub dep_edges_rel: Vec<(String, String)>,
    /// Checkout-relative file → exported signature set.
    pub signatures: SignatureMap,
    /// Checkout-relative overload groups.
    pub overloads: OverloadIndex,
}

impl State {
    fn dir(root: &Path) -> PathBuf {
        root.join("index")
    }

    /// Loads the state from disk (empty when absent/unreadable).
    pub fn load(root: &Path) -> State {
        let dir = Self::dir(root);
        let deps = std::fs::read_to_string(dir.join("deps.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<(String, String)>>(&t).ok())
            .unwrap_or_default();
        let signatures = std::fs::read_to_string(dir.join("signatures.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<SignatureMap>(&t).ok())
            .unwrap_or_default();
        let overloads = std::fs::read_to_string(dir.join("overloads.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<OverloadIndex>(&t).ok())
            .unwrap_or_default();
        State {
            dep_edges_rel: deps,
            signatures,
            overloads,
        }
    }

    /// Persists the state under the store root.
    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let dir = Self::dir(root);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("deps.json"),
            serde_json::to_string_pretty(&self.dep_edges_rel)?,
        )?;
        std::fs::write(
            dir.join("signatures.json"),
            serde_json::to_string_pretty(&self.signatures)?,
        )?;
        std::fs::write(
            dir.join("overloads.json"),
            serde_json::to_string_pretty(&self.overloads)?,
        )?;
        Ok(())
    }

    /// Derives the state from a freshly assembled full graph, keyed
    /// checkout-relative under `root` (the scan root the graph was built for).
    pub fn from_graph(graph: &Graph, root: &Path) -> State {
        let index = DepIndex::from_graph(graph);
        State {
            dep_edges_rel: index.edges_rel(root),
            signatures: signatures_of_graph(graph, root),
            overloads: OverloadIndex::from_graph(graph, root),
        }
    }
}

/// The per-scan incremental preparation. `full_scan: Some(reason)` means the
/// caller MUST run the full pipeline and ignore `targets`/`reuse` — the
/// correctness fallbacks fire.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub store_root: PathBuf,
    pub cache_key: CacheKey,
    pub full_scan: Option<FullScanReason>,
    /// The re-emission target set (checkout-relative): changed files ∪ the
    /// reverse-dependency closure over the cached dep index ∪ overload peers.
    pub targets_rel: BTreeSet<String>,
    /// The changed files (checkout-relative).
    pub changed_rel: BTreeSet<String>,
    /// FQNs that disappeared with the delta's removed/renamed-away files — the
    /// subtraction half of the full-universe seam (feedback-92).
    pub removed_fqns: BTreeSet<String>,
    /// The checkout-relative files that are byte-identical to the prior scan
    /// and outside the target set — the fact-reuse candidates.
    pub reuse_candidates: Vec<ReuseFile>,
    /// The current content manifest (path → blob OID), built with the prepare
    /// so the pipeline can record it without re-walking the tree.
    pub manifest: Manifest,
    /// The content-identity key of the recorded scan the delta was derived from
    /// (`ScanRecord::content_key`), captured BEFORE the completed scan
    /// overwrites `scan.json`. The win-C splice is eligible only when the LOCAL
    /// seed DB's own recorded key equals this — otherwise another worktree's
    /// scan advanced the shared state past this worktree's DB and seeding would
    /// publish a DB that is not a full rebuild (feedback-101, see
    /// [`crate::splice::seed_checked`]).
    pub recorded_content_key: Option<String>,
}

impl Prepared {
    /// The target files re-based onto the current scan root (absolute paths for
    /// the frontend `--targets` file).
    pub fn target_abs(&self, scan_root: &Path) -> BTreeSet<String> {
        self.targets_rel
            .iter()
            .map(|rel| absolute(scan_root, rel))
            .collect()
    }
}

/// The owned reuse plan `run_pipeline` consumes: the store root, the cache key,
/// and the file units to splice. Owned (not borrowed) so the pipeline call site
/// can pass `None` on the full path.
#[derive(Debug, Clone)]
pub struct ReusePlan {
    pub store_root: PathBuf,
    pub cache_key: CacheKey,
    /// `(checkout-relative path, language, blob OID)` per reusable file.
    pub files: Vec<(String, String, String)>,
    /// The current scan root the cached units are re-based onto.
    pub reader_root: String,
    /// The languages whose frontend is skipped entirely this scan (empty target
    /// set on a partial scan). Their module scaffolding is replayed from the
    /// store into the assembled graph (feedback-102) — the SAME per-language
    /// verdict the win-C spawn skip uses, never re-derived.
    pub skipped_langs: BTreeSet<String>,
}

impl ReusePlan {
    /// Builds the ingest-level [`crate::ingest::Reuse`] view from a loaded
    /// store.
    pub fn reuse<'a>(&'a self, store: &'a FactStore) -> crate::ingest::Reuse<'a> {
        crate::ingest::Reuse {
            store,
            cache_key: &self.cache_key,
            files: self.files.clone(),
            reader_root: self.reader_root.clone(),
            skipped_langs: self.skipped_langs.clone(),
        }
    }
}

/// The complete incremental input `run_pipeline` needs to (a) splice cached
/// facts into the win-B assembly and (b) record the just-completed scan back
/// into the shared store. `None` on a caller that does not maintain the store
/// (the hermetic test harness).
#[derive(Debug, Clone)]
pub struct PipelineInput {
    /// The shared store root, or `None` when the scan is not in a git repo.
    pub store_root: Option<PathBuf>,
    pub cache_key: CacheKey,
    /// The scan root the manifest/graph were built for (absolute).
    pub scan_root: PathBuf,
    /// The current content manifest (path → blob OID).
    pub manifest: Manifest,
    /// The commit sha the scan ran at (empty when not a git repo).
    pub sha: String,
    /// The fact-reuse plan for the win-B assembly (`None` on a full scan).
    pub reuse: Option<ReusePlan>,
    /// The FINAL phase-2 re-emission target set (checkout-relative): changed
    /// files ∪ the signature-cascade reverse-dependency closure ∪ overload
    /// peers. This is the win-C splice's delete scope (phase-03 task-4) and the
    /// very set that drove the frontend `--targets` hand-off — it is threaded
    /// through, never re-derived. Empty on a full-scan fallback.
    pub targets_rel: BTreeSet<String>,
    /// The code FQNs that disappeared with the delta (the subtraction half of
    /// the full-universe seam, feedback-92). The splice detaches them even when
    /// their file path lies outside `targets_rel`. Empty on a full-scan
    /// fallback.
    pub removed_fqns: BTreeSet<String>,
    /// The content-identity key of the recorded scan the delta was derived from
    /// (`ScanRecord::content_key`), captured before the completed scan
    /// overwrites the shared store. `None` on a full-scan fallback and on a
    /// pre-hardening record; the win-C splice refuses a seed that does not
    /// match this key (feedback-101).
    pub recorded_content_key: Option<String>,
}

/// Prepares the incremental scan: computes the delta + fallbacks, the changed
/// set, the re-emission target set, the removed-FQN set, and the reusable-file
/// list.
///
/// The signature early cutoff uses the persisted signature map: a changed file
/// whose byte edit did not alter its exported signature does **not** pull its
/// reverse dependencies; its own facts are still re-emitted.
pub fn prepare(scan_root: &Path, apg_root: &Path, config: &ScanConfigKey) -> Prepared {
    let cache_key = CacheKey::compute(config);
    let store_root = match FactStore::resolve(apg_root) {
        Ok(s) => s.root,
        Err(_) => {
            return Prepared {
                store_root: PathBuf::new(),
                cache_key,
                full_scan: Some(FullScanReason::NotAGitRepo),
                targets_rel: BTreeSet::new(),
                changed_rel: BTreeSet::new(),
                removed_fqns: BTreeSet::new(),
                reuse_candidates: Vec::new(),
                manifest: Manifest::default(),
                recorded_content_key: None,
            };
        }
    };
    // The content identity the delta is derived from, captured BEFORE the
    // completed scan rewrites the shared `scan.json` (feedback-101): the win-C
    // splice seeds the LOCAL db.lbug and must refuse it when its own recorded
    // key differs — another worktree scanned in between.
    let recorded_content_key = ScanRecord::load(&store_root).and_then(|r| r.content_key);
    let plan = delta::plan(apg_root, Some(&store_root), config);
    let manifest = Manifest::build(scan_root);
    if let Some(reason) = plan.full_scan {
        // A full scan still records its manifest/facts on completion (the cold
        // baseline the next scan diffs against); only reuse is disabled.
        return Prepared {
            store_root,
            cache_key,
            full_scan: Some(reason),
            targets_rel: BTreeSet::new(),
            changed_rel: BTreeSet::new(),
            removed_fqns: BTreeSet::new(),
            reuse_candidates: Vec::new(),
            manifest,
            recorded_content_key: None,
        };
    }

    let recorded = Manifest::load(&store_root);
    let manifest_delta = recorded
        .as_ref()
        .map(|m| m.diff(&manifest))
        .unwrap_or_default();
    let git_delta: Delta = plan.delta.unwrap_or_default();

    // Changed set: manifest add/modify/remove (working tree vs recorded) ∪ the
    // git delta (which also carries committed changes and renames).
    let mut changed_rel: BTreeSet<String> = manifest_delta.changed();
    changed_rel.extend(git_delta.changed());

    // Deleted files: recorded but no longer present, or removed by the delta.
    let deleted: BTreeSet<String> = manifest_delta
        .removed
        .iter()
        .chain(
            git_delta
                .removed
                .iter()
                .filter(|p| !manifest.entries.contains_key(*p)),
        )
        .cloned()
        .collect();

    let state = State::load(&store_root);

    // Stage-1 target set: the changed files ∪ their overload peers. The
    // reverse-dependency closure is computed AFTER phase 1 by
    // [`extra_cascade_targets`], driven by the phase-1 stream's exported
    // signature delta — the signature early cutoff (phase-02 task-6). This
    // function does NOT cascade on bytes alone.
    let mut targets_rel = changed_rel.clone();
    for f in &changed_rel {
        targets_rel.extend(state.overloads.peer_files(f));
    }

    // FULL-UNIVERSE SEAM (feedback-92): the incremental path derives the full
    // code-FQN universe from the PREVIOUS export. With a non-empty target set
    // the frontends emission-filter, so the target-only spool would shrink the
    // universe (and `validate_code_refs` would falsely bail `spec drift`) — the
    // task body pins "no previous export ⇒ full scan". A fresh worktree that
    // reuses the shared store but has no local `graph.jsonl` therefore full-
    // scans. An EMPTY target set emits every fact (no filter), so the full
    // spool is already the universe and reuse stays available.
    if !targets_rel.is_empty()
        && !apg_root
            .join(crate::specs::TRANS)
            .join("graph.jsonl")
            .exists()
    {
        return Prepared {
            store_root,
            cache_key,
            full_scan: Some(FullScanReason::NoPreviousExport),
            targets_rel: BTreeSet::new(),
            changed_rel: BTreeSet::new(),
            removed_fqns: BTreeSet::new(),
            reuse_candidates: Vec::new(),
            manifest,
            recorded_content_key: None,
        };
    }

    // Removed FQNs: every FQN the recorded signature map attributes to a
    // removed/renamed-away file (the full-universe subtraction half).
    let mut removed_fqns: BTreeSet<String> = BTreeSet::new();
    for rel in &deleted {
        if let Some(sigs) = state.signatures.get(rel) {
            for s in sigs {
                removed_fqns.insert(s.fqn.clone());
            }
        }
    }
    for (old, _new) in &git_delta.renamed {
        if let Some(sigs) = state.signatures.get(old) {
            for s in sigs {
                removed_fqns.insert(s.fqn.clone());
            }
        }
    }

    // Reuse candidates: files present in the CURRENT manifest that are byte-
    // identical to the recorded OID and outside the target set.
    let mut reuse_candidates = Vec::new();
    if let Some(recorded) = &recorded {
        for (rel, oid) in &manifest.entries {
            if targets_rel.contains(rel) || deleted.contains(rel) {
                continue;
            }
            let Some(prev_oid) = recorded.oid(rel) else {
                continue; // new-but-not-changed cannot happen; be conservative
            };
            if prev_oid != oid {
                continue;
            }
            let abs = absolute(scan_root, rel);
            reuse_candidates.push(ReuseFile {
                abs,
                rel: rel.clone(),
                oid: oid.clone(),
                lang: language_of(rel).to_string(),
            });
        }
    }

    Prepared {
        store_root,
        cache_key,
        full_scan: None,
        targets_rel,
        changed_rel,
        removed_fqns,
        reuse_candidates,
        manifest,
        recorded_content_key,
    }
}

/// The signature early cutoff (phase-02 task-6), applied after the phase-1
/// re-emission: given the phase-1 assembled graph and the stage-1 target set,
/// return the **additional** files the reverse-dependency closure must pull in.
///
/// A stage-1 file whose exported signature changed (compared to the stored
/// signature map) seeds the cascade; a body-only change does not. Returns
/// checkout-relative paths outside `stage1` that depend (transitively) on a
/// signature-changed file.
pub fn extra_cascade_targets(
    store_root: &Path,
    scan_root: &Path,
    phase1_graph: &Graph,
    stage1: &BTreeSet<String>,
) -> BTreeSet<String> {
    let _ = scan_root;
    let state = State::load(store_root);
    // The phase-1 graph's signatures, keyed checkout-relative.
    let new_sigs = signatures_of_graph(phase1_graph, scan_root);
    // Signature-changed stage-1 files: compare only the stage-1 entries (files
    // outside the stage-1 set were not re-emitted, so their signatures are
    // unchanged by definition).
    let mut changed: BTreeSet<String> = BTreeSet::new();
    for rel in stage1 {
        match (state.signatures.get(rel), new_sigs.get(rel)) {
            (None, Some(_)) => {
                changed.insert(rel.clone());
            }
            (Some(old), Some(new)) if old != new => {
                changed.insert(rel.clone());
            }
            (Some(_), None) => {
                // The file's units vanished (removed/renamed away).
                changed.insert(rel.clone());
            }
            _ => {}
        }
    }
    if changed.is_empty() {
        return BTreeSet::new();
    }
    let index = crate::impact::dep_index_rel(&state.dep_edges_rel);
    let closure = index.reverse_closure(&changed);
    closure
        .into_iter()
        .filter(|f| !stage1.contains(f))
        .collect()
}

/// The full-universe seam (feedback-92): the complete code-FQN universe
/// `layers::ingest_tree` validates authored `implemented-by` targets against, on
/// the incremental path. Derived from the PREVIOUS export (`graph.jsonl`) MINUS
/// the delta's removed FQNs UNION the delta's emitted real code FQNs — never
/// from the target-only spool.
pub fn full_universe(
    apg_root: &Path,
    prepared: &Prepared,
    emitted_fqns: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut universe: BTreeSet<String> = crate::artifacts::code_universes_from_export(apg_root)
        .map(|(scanned, _planned)| scanned)
        .unwrap_or_default();
    for f in &prepared.removed_fqns {
        universe.remove(f);
    }
    universe.extend(emitted_fqns.iter().cloned());
    universe
}

/// The absolute path of a checkout-relative path.
pub fn absolute(root: &Path, rel: &str) -> String {
    if Path::new(rel).is_absolute() {
        rel.to_string()
    } else {
        root.join(rel).to_string_lossy().into_owned()
    }
}

/// Records the just-completed scan into the shared store: writes the new
/// manifest, the scan record, each file's fact unit, and the portable indexes.
///
/// `reuse_langs` maps a checkout-relative path to the language its unit was
/// stored under, so reuse candidates carry their language on the next scan.
pub fn record(
    store_root: &Path,
    cache_key: &CacheKey,
    scan_root: &Path,
    graph: &Graph,
    manifest: &Manifest,
    sha: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(store_root)?;
    let writer_root = scan_root.to_string_lossy().into_owned();
    let mut store = FactStore::at(store_root.to_path_buf()).load();

    // Per-file fact units for every located file in the assembled graph.
    let mut files: BTreeSet<String> = BTreeSet::new();
    for node in graph.nodes.values() {
        if let Some(loc) = &node.location {
            files.insert(loc.path.to_string_lossy().into_owned());
        }
    }
    for abs in &files {
        let rel = crate::cache::rel_path_of(scan_root, abs);
        let Some(oid) = manifest.oid(&rel) else {
            continue;
        };
        let lang = language_of(rel.as_str());
        let frag = FileFragment::from_graph(graph, abs, &rel, oid, lang);
        store.put(&frag, &writer_root, cache_key)?;
    }
    // feedback-102: the per-file units cannot carry a language's
    // pure-intermediate modules or its `Module -> Module` hierarchy, so record
    // that scaffolding alongside them. A later partial scan that skips a
    // language replays it, keeping the win-B-assembled graph — and therefore the
    // `graph.jsonl` rendered from it — a full rebuild's equal. This runs BEFORE
    // the manifest/scan-record save, so a scaffolding write failure aborts the
    // recording and the next scan takes the full-scan fallback rather than
    // reusing an incomplete store. The assembled graph is complete here (spawned
    // languages emit their full scaffolding; skipped ones were just replayed),
    // so the extraction is authoritative.
    let scaffolding = crate::cache::ModuleScaffolding::extract(graph, scan_root);
    store.put_scaffolding_all(&scaffolding, cache_key)?;
    store.save_index()?;

    // The manifest + scan record (the next scan's baseline).
    manifest.save(store_root)?;
    ScanRecord {
        sha: sha.to_string(),
        cache_key: cache_key.clone(),
        manifest: manifest.clone(),
        // The same content-identity key the DB `Scan` row and graph.jsonl line
        // 1 carry (this scan's `scan_meta`), so the next scan can verify its
        // LOCAL seed against the shared record before splicing (feedback-101).
        content_key: graph
            .nodes
            .get(crate::schema::SCAN_HEAD)
            .and_then(|n| n.content_key.clone()),
    }
    .save(store_root)?;

    // The portable indexes.
    State::from_graph(graph, scan_root).save(store_root)?;
    Ok(())
}

/// The C++ source/header extensions the C++ frontend recognizes — the dotless
/// mirror of `cpplib.is_cpp_ext` (`src/cpplib/main.cpp`). This is the root
/// crate's single source of truth for C++ membership: [`language_of`]
/// classifies by it and `main::auto_detect_languages` derives its C++ candidate
/// list from it. Keeping one list means a changed `.hxx`/`.tpp`/`.ipp`/`.c++`
/// lands in the C++ target set instead of falling through to `other`, which
/// would leave it in the impact set but in no per-language list — re-emitted by
/// neither the frontend nor the cache, silently dropping its facts.
pub const CPP_EXTENSIONS: &[&str] = &[
    "cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx", "tpp", "ipp",
];

/// The language a checkout-relative path belongs to, by extension — the
/// per-language granularity key for the fact store.
pub fn language_of(rel: &str) -> &'static str {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "go" => "go",
        "java" => "java",
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => "ts",
        "cs" | "csx" => "csharp",
        "py" | "pyi" => "python",
        _ if CPP_EXTENSIONS.contains(&ext) => "cpp",
        "md" | "markdown" => "md",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Location, Node, NodeKind};

    fn located(kind: NodeKind, path: &str) -> Node {
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

    /// unit tier -- pure in-memory: no filesystem, database, git or process.
    mod unit {
        use super::*;

        #[test]
        fn language_of_maps_extensions() {
            assert_eq!(language_of("a.go"), "go");
            assert_eq!(language_of("A.java"), "java");
            assert_eq!(language_of("x.rs"), "rust");
            assert_eq!(language_of("x.tsx"), "ts");
            assert_eq!(language_of("x.cs"), "csharp");
            assert_eq!(language_of("x.py"), "python");
            assert_eq!(language_of("x.cpp"), "cpp");
            // Every extension `cpplib.is_cpp_ext` accepts classifies as cpp, so a
            // changed header/impl lands in the C++ target list (feedback-99).
            assert_eq!(language_of("x.cc"), "cpp");
            assert_eq!(language_of("x.cxx"), "cpp");
            assert_eq!(language_of("x.c++"), "cpp");
            assert_eq!(language_of("x.h"), "cpp");
            assert_eq!(language_of("x.hpp"), "cpp");
            assert_eq!(language_of("x.hh"), "cpp");
            assert_eq!(language_of("x.hxx"), "cpp");
            assert_eq!(language_of("x.tpp"), "cpp");
            assert_eq!(language_of("x.ipp"), "cpp");
            assert_eq!(language_of("x.md"), "md");
            assert_eq!(language_of("x.unknown"), "other");
        }

        #[test]
        fn function_params_flow_to_signatures() {
            let mut g = Graph::default();
            let mut n = located(NodeKind::Function, "/r/a.go");
            n.params = vec!["int".into()];
            g.nodes.insert("m.Leaf".into(), n);
            let sigs = signatures_of_graph(&g, Path::new("/r"));
            let set = &sigs["a.go"];
            let sig = set.iter().find(|s| s.fqn == "m.Leaf").unwrap();
            assert_eq!(sig.params, vec!["int".to_string()]);
        }

        #[test]
        fn absolute_joins_relative_only() {
            let root = Path::new("/r");
            assert_eq!(absolute(root, "src/a.go"), "/r/src/a.go");
            assert_eq!(absolute(root, "/abs/a.go"), "/abs/a.go");
        }
    }

    /// e2e tier -- real I/O: these tests stage temp dirs, write state files and
    /// run real libgit2 operations. Each is `#[ignore]`d, so a plain `cargo
    /// test` never runs one; the only entry point is the named guard
    /// `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
    mod e2e {
        use super::*;

        #[test]
        #[ignore = "e2e tier: real I/O (temp dir/state files/git); run via cargo test-e2e"]
        fn state_round_trips_and_is_checkout_relative() {
            let dir = std::env::temp_dir().join(format!("apg-inc-state-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let root = dir.join("wt");
            std::fs::create_dir_all(root.join("src")).unwrap();

            let mut g = Graph::default();
            g.nodes.insert(
                "m.A".into(),
                located(NodeKind::Struct, &root.join("src/a.go").to_string_lossy()),
            );
            g.nodes.insert(
                "m.B".into(),
                located(NodeKind::Struct, &root.join("src/b.go").to_string_lossy()),
            );
            g.uses.insert(("m.B".into(), "m.A".into()));
            let state = State::from_graph(&g, &root);
            assert_eq!(
                state.dep_edges_rel,
                vec![("src/b.go".to_string(), "src/a.go".to_string())]
            );
            assert!(state.signatures.contains_key("src/a.go"));
            state.save(&dir).unwrap();

            let loaded = State::load(&dir);
            assert_eq!(loaded.dep_edges_rel, state.dep_edges_rel);
            assert_eq!(loaded.signatures, state.signatures);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp dir/state files/git); run via cargo test-e2e"]
        fn cascade_targets_only_on_a_signature_change() {
            let dir = std::env::temp_dir().join(format!("apg-cascade-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let root = dir.join("wt");
            std::fs::create_dir_all(root.join("a")).unwrap();
            std::fs::create_dir_all(root.join("b")).unwrap();
            std::fs::create_dir_all(root.join("c")).unwrap();
            let apath = root.join("a/a.go").to_string_lossy().into_owned();
            let bpath = root.join("b/b.go").to_string_lossy().into_owned();
            let cpath = root.join("c/c.go").to_string_lossy().into_owned();

            // The stored baseline: a.Leaf() — no params; b calls a; c calls b.
            let mut old = Graph::default();
            let mut fa = located(NodeKind::Function, &apath);
            fa.params = vec![];
            old.nodes.insert("scratch/a.Leaf".into(), fa);
            let mut fb = located(NodeKind::Function, &bpath);
            fb.params = vec![];
            old.nodes.insert("scratch/b.Foo".into(), fb);
            let mut fc = located(NodeKind::Function, &cpath);
            fc.params = vec![];
            old.nodes.insert("scratch/c.Bar".into(), fc);
            old.calls
                .insert(("scratch/b.Foo".into(), "scratch/a.Leaf".into()));
            old.calls
                .insert(("scratch/c.Bar".into(), "scratch/b.Foo".into()));
            State::from_graph(&old, &root).save(&dir).unwrap();

            // Body-only: a.Leaf keeps its (empty) params ⇒ no cascade.
            let body = old.clone();
            let stage1 = BTreeSet::from(["a/a.go".to_string()]);
            assert!(
                extra_cascade_targets(&dir, &root, &body, &stage1).is_empty(),
                "a body-only change must not cascade"
            );

            // Signature change: a.Leaf gains a param ⇒ b and c cascade.
            let mut sig = old.clone();
            sig.nodes.get_mut("scratch/a.Leaf").unwrap().params = vec!["int".into()];
            let extra = extra_cascade_targets(&dir, &root, &sig, &stage1);
            assert!(extra.contains("b/b.go"), "{extra:?}");
            assert!(extra.contains("c/c.go"), "{extra:?}");
            assert!(!extra.contains("a/a.go"), "the seed is already stage-1");

            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp dir/state files/git); run via cargo test-e2e"]
        fn no_previous_export_forces_full_scan_only_when_filtering() {
            // A scratch git repo with a recorded scan (manifest + scan record) but
            // NO local `apg/.trans/graph.jsonl`: with a non-empty target set the
            // full-universe seam cannot be derived, so the correctness fallback
            // forces a full scan (feedback-92). With an EMPTY target set nothing is
            // emission-filtered, so reuse stays available.
            use crate::cache::{CacheKey, Manifest, ScanConfigKey};
            use crate::delta::ScanRecord;

            let dir = std::env::temp_dir().join(format!("apg-noexport-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let repo_dir = dir.join("repo");
            std::fs::create_dir_all(&repo_dir).unwrap();
            let repo = git2::Repository::init(&repo_dir).unwrap();
            {
                let mut cfg = repo.config().unwrap();
                cfg.set_str("user.name", "apg test").unwrap();
                cfg.set_str("user.email", "t@example.com").unwrap();
            }
            std::fs::write(repo_dir.join(".gitignore"), "apg/.trans/\n").unwrap();
            std::fs::write(repo_dir.join("a.go"), "package a\n").unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            let sha = repo
                .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
                .unwrap()
                .to_string();

            // The shared store carries a recorded scan + an empty manifest (so the
            // current tree looks changed), but the worktree has no export.
            let apg_root = repo_dir.join("apg");
            std::fs::create_dir_all(&apg_root).unwrap();
            let store = FactStore::resolve(&apg_root).unwrap().root;
            std::fs::create_dir_all(&store).unwrap();
            let config = ScanConfigKey {
                languages: vec!["go".into()],
                ..Default::default()
            };
            let key = CacheKey::compute(&config);
            Manifest::default().save(&store).unwrap();
            ScanRecord {
                sha,
                cache_key: key.clone(),
                manifest: Manifest::default(),
                content_key: None,
            }
            .save(&store)
            .unwrap();

            let prepared = prepare(&repo_dir, &apg_root, &config);
            assert!(
                matches!(prepared.full_scan, Some(FullScanReason::NoPreviousExport)),
                "a non-empty target set with no previous export must full-scan: {:?}",
                prepared.full_scan
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
