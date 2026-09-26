use std::collections::BTreeSet;
use std::path::Path;

use crate::cache::{CacheKey, FactStore, FileFragment, FileIndex, Manifest};
use crate::delta::ScanRecord;
use crate::graph::Graph;

use super::language::language_of;
use super::prepare::State;

/// Records the just-completed scan into the shared store: writes the new
/// manifest, the scan record, each file's fact unit, and the portable indexes.
///
/// `targets_rel` is the FINAL phase-2 re-emission target set (checkout-relative)
/// — the SAME set that drove the frontend `--targets` hand-off and the win-C DB
/// splice, threaded through and never re-derived. On the incremental path
/// (non-empty) only the re-emitted/target files' units are (re)written; on a
/// full scan (empty) every located file's unit is written so the shared store
/// stays complete and reusable. A target the assembled graph does not carry is
/// skipped — a stale unit would be a correctness bug, not a saving.
pub fn record(
    store_root: &Path,
    cache_key: &CacheKey,
    scan_root: &Path,
    graph: &Graph,
    manifest: &Manifest,
    sha: &str,
    targets_rel: &BTreeSet<String>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(store_root)?;
    let writer_root = scan_root.to_string_lossy().into_owned();
    let mut store = FactStore::at(store_root.to_path_buf()).load();

    // Per-file fact units for every located file in the assembled graph, derived
    // from ONE per-file index (phase-04 task-12) rather than an `O(F*(N+E))`
    // per-file graph scan.
    let index = FileIndex::build(graph);
    let full_scan = targets_rel.is_empty();
    let mut files: BTreeSet<String> = BTreeSet::new();
    for node in graph.nodes.values() {
        if let Some(loc) = &node.location {
            files.insert(loc.path.to_string_lossy().into_owned());
        }
    }
    for abs in &files {
        let rel = crate::cache::rel_path_of(scan_root, abs);
        // Target-scoped recording (phase-04 task-13): with a target set in force
        // write ONLY the re-emitted/target files' units — an unchanged non-target
        // file keeps the unit a previous scan recorded. The full-scan path keeps
        // writing every located file's unit. A target path the assembled graph
        // does not carry is skipped, never written as a stale unit.
        if !full_scan && !targets_rel.contains(&rel) {
            continue;
        }
        let Some(oid) = manifest.oid(&rel) else {
            continue;
        };
        let lang = language_of(rel.as_str());
        let frag = FileFragment::from_index(&index, abs, &rel, oid, lang);
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
