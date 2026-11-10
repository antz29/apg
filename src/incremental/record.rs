use std::collections::BTreeSet;
use std::path::Path;

use crate::cache::{CacheKey, FactStore, FileFragment, FileIndex, Manifest, ModuleScaffolding};
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
    // language replays it, keeping the win-B-assembled graph a full rebuild's
    // equal. This runs BEFORE the manifest/scan-record save, so a scaffolding
    // write failure aborts the recording and the next scan takes the full-scan
    // fallback rather than reusing an incomplete store. A full scan's graph is
    // complete (spawned languages emit their full scaffolding; skipped ones were
    // just replayed), so its extraction is authoritative and written wholesale.
    // The delta path's graph is target-only, so each spawned language's
    // extraction is merged into the stored record instead — a partially-emitting
    // frontend (COBOL) must not lose its untouched modules, while a module whose
    // every file was re-emitted and that the delta no longer carries is retired.
    let scaffolding = ModuleScaffolding::extract(graph, scan_root);
    if full_scan {
        store.put_scaffolding_all(&scaffolding, cache_key)?;
    } else {
        for (lang, delta) in &scaffolding {
            let merged = merge_scaffolding(
                store.scaffolding(lang, cache_key).unwrap_or_default(),
                delta,
                targets_rel,
            );
            store.put_scaffolding(lang, &merged, cache_key)?;
        }
    }
    store.save_index()?;

    // The manifest + scan record (the next scan's baseline).
    manifest.save(store_root)?;
    ScanRecord {
        sha: sha.to_string(),
        cache_key: cache_key.clone(),
        manifest: manifest.clone(),
        // The same content-identity key the DB `Scan` row carries (this scan's
        // `scan_meta`), so the next scan can verify its LOCAL seed against the
        // shared record before splicing (feedback-101).
        content_key: graph
            .nodes
            .get(crate::schema::SCAN_HEAD)
            .and_then(|n| n.content_key.clone()),
    }
    .save(store_root)?;

    // The portable indexes. On the delta path the graph carries only the target
    // files' re-emitted facts, so the persisted state is merged with the delta
    // instead of re-derived from it — an unchanged file's entries survive. A
    // full scan (or the full-load fallback's re-assembled graph) derives the
    // whole state from the graph.
    if full_scan {
        State::from_graph(graph, scan_root).save(store_root)?;
    } else {
        let mut state = State::load(store_root);
        state.merge_delta(graph, scan_root, targets_rel);
        state.save(store_root)?;
    }
    Ok(())
}

/// Merges one language's delta extraction into its stored scaffolding.
///
/// A stored module is **retired** only when its recorded `files` list is
/// non-empty, every one of those files is a re-emitted target, and the delta no
/// longer carries the module — the target file(s) were deleted or renamed away.
/// A module with a surviving non-target file, and one whose `files` entry is
/// unknown or empty (a legacy record), is kept: an undecidable module is never
/// dropped. The delta's `modules`/`edges`/`languages`/`language_edges` are
/// unioned in and the `files` maps are unioned per module, then the record is
/// normalized exactly as [`ModuleScaffolding::extract`] — sorted, deduped, and
/// with every edge/file entry scoped to the modules it carries.
fn merge_scaffolding(
    mut stored: ModuleScaffolding,
    delta: &ModuleScaffolding,
    targets_rel: &BTreeSet<String>,
) -> ModuleScaffolding {
    let delta_modules: BTreeSet<&str> = delta.modules.iter().map(String::as_str).collect();
    let retired: BTreeSet<String> = stored
        .modules
        .iter()
        .filter(|m| {
            !delta_modules.contains(m.as_str())
                && stored.files.get(*m).is_some_and(|files| {
                    !files.is_empty() && files.iter().all(|f| targets_rel.contains(f))
                })
        })
        .cloned()
        .collect();

    stored.modules.retain(|m| !retired.contains(m));
    stored.modules.extend(delta.modules.iter().cloned());
    stored.files.retain(|m, _| !retired.contains(m));
    for (m, files) in &delta.files {
        stored
            .files
            .entry(m.clone())
            .or_default()
            .extend(files.iter().cloned());
    }
    stored
        .edges
        .retain(|(a, b)| !retired.contains(a) && !retired.contains(b));
    stored.edges.extend(delta.edges.iter().cloned());
    stored.languages.extend(delta.languages.iter().cloned());
    stored
        .language_edges
        .extend(delta.language_edges.iter().cloned());

    stored.modules.sort();
    stored.modules.dedup();
    let modules: BTreeSet<String> = stored.modules.iter().cloned().collect();
    stored.edges.sort();
    stored.edges.dedup();
    stored
        .edges
        .retain(|(a, b)| modules.contains(a) && modules.contains(b));
    stored.languages.sort();
    stored.languages.dedup();
    stored.language_edges.sort();
    stored.language_edges.dedup();
    stored
        .language_edges
        .retain(|(_, module)| modules.contains(module));
    stored.files.retain(|m, _| modules.contains(m));
    for files in stored.files.values_mut() {
        files.sort();
        files.dedup();
    }
    stored
}
