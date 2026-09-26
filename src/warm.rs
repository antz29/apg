//! Warm-cache seed preparation and the reused-fact code-FQN universes.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — the same warm verdict, the
//! same reused-universe derivation.

use std::collections::BTreeSet;
use std::path::Path;

use crate::{cache, delta, git, graph, incremental};

/// The warm-cache seed preparation (phase-06 tasks 3/4/6): when the shared store
/// holds a COMPLETE recorded scan for exactly this checkout's HEAD under the
/// current cache key, build the [`incremental::Prepared`] state from the
/// **recorded manifest alone** — never `Manifest::build`, so no full-tree walk —
/// and return it. `None` means the caller runs the ordinary
/// `incremental::prepare`.
///
/// The verdict comes from the pure `delta::scan_verdict` wrapper
/// ([`delta::scan_state`]): the recorded scan is at exactly HEAD under the
/// current key. Completeness is then judged without a source walk: every
/// source-language entry of the recorded manifest must have a stored fact unit
/// under the current key (non-source entries never get units and are ignored),
/// and every such language must have stored module scaffolding. A single missing
/// unit/scaffold is a miss (the caller falls back), so the warm path can never
/// assemble an incomplete graph. The recorded content-identity key must equal
/// this checkout's own, so a record from a different tree at the same sha is
/// refused.
pub fn warm_prepared(
    project_dir: &Path,
    apg_root: &Path,
    git_state: &git::GitState,
    scan_config: &cache::ScanConfigKey,
) -> Option<incremental::Prepared> {
    let cache_key = cache::CacheKey::compute(scan_config);
    let store_root = cache::FactStore::resolve(apg_root).ok()?.root;
    let (record, verdict) = delta::scan_state(apg_root, Some(&store_root), &cache_key);
    if !verdict.is_warm_cache() {
        return None;
    }
    let record = record?;
    // The recorded tree content must equal THIS checkout's: a record from a
    // dirty tree (or another commit's tree at the same sha) is not this tree.
    let (Some(current_key), Some(recorded_key)) = (
        git_state.content_key.as_deref(),
        record.content_key.as_deref(),
    ) else {
        return None;
    };
    if current_key != recorded_key {
        return None;
    }
    let store = cache::FactStore::at(store_root.clone()).load();
    let mut languages: BTreeSet<String> = BTreeSet::new();
    let mut reuse_candidates: Vec<incremental::ReuseFile> = Vec::new();
    for (rel, oid) in &record.manifest.entries {
        let lang = incremental::language_of(rel);
        if lang == "other" {
            // Not a scanned source file: it never carries a fact unit.
            continue;
        }
        // A source file with no stored unit means the cache is not complete
        // for this tree — fall back (conservative, never a partial assembly).
        store.candidate(lang, rel, oid, &cache_key)?;
        languages.insert(lang.to_string());
        reuse_candidates.push(incremental::ReuseFile {
            abs: incremental::absolute(project_dir, rel),
            rel: rel.clone(),
            oid: oid.clone(),
            lang: lang.to_string(),
        });
    }
    if reuse_candidates.is_empty() {
        return None;
    }
    for lang in &languages {
        // A language with no stored scaffolding cannot be reconstructed from
        // the cache — fall back.
        store.scaffolding(lang, &cache_key)?;
    }
    // The recorded manifest describes this tree; re-base its recorded root onto
    // the reading checkout so the re-recorded baseline stays coherent.
    let mut manifest = record.manifest.clone();
    manifest.root = project_dir.to_string_lossy().into_owned();
    Some(incremental::Prepared {
        store_root,
        cache_key,
        full_scan: None,
        targets_rel: BTreeSet::new(),
        changed_rel: BTreeSet::new(),
        removed_fqns: BTreeSet::new(),
        reuse_candidates,
        manifest,
        recorded_content_key: record.content_key.clone(),
    })
}

/// The code-FQN universe of the shared store's complete warm cache — the
/// full-universe seam's source when no local export exists yet (a fresh
/// worktree): every fragment's re-based code FQNs (module FQNs included) plus
/// every language's module scaffolding. The warm assembly produces exactly these
/// nodes, so authored `implemented-by` validation sees a full rebuild's universe
/// without the local `graph.jsonl` a full scan would have written.
pub fn warm_universe(
    store_root: &Path,
    cache_key: &cache::CacheKey,
    manifest: &cache::Manifest,
    languages: &BTreeSet<String>,
) -> BTreeSet<String> {
    let store = cache::FactStore::at(store_root.to_path_buf()).load();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (rel, oid) in &manifest.entries {
        let lang = incremental::language_of(rel);
        if lang == "other" {
            continue;
        }
        let Some((frag, stored_root)) = store.candidate(lang, rel, oid, cache_key) else {
            continue;
        };
        let (modules, nodes, _) = frag.project(&stored_root, "");
        out.extend(modules);
        for (fqn, node) in nodes {
            if node.status.is_none()
                && matches!(
                    node.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                )
            {
                out.insert(fqn);
            }
        }
    }
    for lang in languages {
        if let Some(scaffolding) = store.scaffolding(lang, cache_key) {
            out.extend(scaffolding.modules);
        }
    }
    out
}

/// The code-FQN universe contributed by this scan's **reused** fact units: each
/// reused file's re-based fragment FQNs plus every skipped language's replayed
/// module scaffolding — exactly the code nodes the win-B assembly splices in
/// from the shared store (the incremental sibling of [`warm_universe`]).
///
/// The incremental full-universe seam ([`incremental::full_universe`]) derives
/// its base from THIS checkout's previous export, which can lag the shared store
/// when another worktree scanned ahead: a file that is byte-identical to the
/// shared record but newer than the local export is reused (not re-emitted), so
/// its code FQNs would drop out of the universe and falsely trip `spec drift`
/// during `implemented-by` validation. Unioning this set in closes that gap.
pub fn reuse_universe(
    store_root: &Path,
    cache_key: &cache::CacheKey,
    files: &[(String, String, String)],
    scaffold_langs: &BTreeSet<String>,
) -> BTreeSet<String> {
    let store = cache::FactStore::at(store_root.to_path_buf()).load();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (rel, lang, oid) in files {
        let Some((frag, stored_root)) = store.candidate(lang, rel, oid, cache_key) else {
            continue;
        };
        let (modules, nodes, _) = frag.project(&stored_root, "");
        out.extend(modules);
        for (fqn, node) in nodes {
            if node.status.is_none()
                && matches!(
                    node.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                )
            {
                out.insert(fqn);
            }
        }
    }
    for lang in scaffold_langs {
        if let Some(scaffolding) = store.scaffolding(lang, cache_key) {
            out.extend(scaffolding.modules);
        }
    }
    out
}
