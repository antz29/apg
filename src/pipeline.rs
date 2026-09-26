//! The ingest → cleanup → DB-build pipeline: `run_pipeline` (assembly, fact
//! recording, and the full-load reference) plus the win-C splice dispatch.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — the same ingested graph, the
//! same recorded facts, and the same `db.lbug` + `graph.jsonl` artifacts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lbug::{Connection, Database};

use crate::cleanup::{CleanupOptions, cleanup};
use crate::logging::Log;
use crate::scan::temp_dir;
use crate::{cache, classify, graph, incremental, ingest, load, schema, session, splice, timing};

/// Consumes the merged scanner JSONL stream, ingests it, and loads `db.lbug` +
/// `graph.jsonl` (SPEC §6).
///
/// `input` carries the win-B incremental state when the scan is incremental
/// (phase-02 task-7/task-8): the cached fact units to splice into the assembly
/// and the store to record the completed scan back into. `None` on the full
/// path (and for the hermetic test harness), where assembly is spool-only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_pipeline(
    records: impl IntoIterator<Item = schema::Record>,
    blacklist: &[String],
    path_excludes: &[String],
    language: &str,
    config: Option<&classify::ApgConfig>,
    input: Option<&incremental::PipelineInput>,
    base: Option<&Path>,
    log: &mut Log,
) -> timing::PipelineTimings {
    // Phase-04 task-4: this function owns two reported phases. Ingest-assembly
    // covers the ingestor passes, cleanup, and the fact-store recording from
    // entry to the DB dispatch below; db-load covers that dispatch (splice OR
    // parquet/Database::new/create_schema/copy_from) and the `graph.jsonl`
    // export.
    let assembly_start = std::time::Instant::now();
    let (mut graph, report) = {
        // Stream the scanner JSONL straight into the ingestor. On the win-B
        // path the target-only spool is ingested here and the unaffected files'
        // cached fact units are spliced into the SAME in-memory graph
        // (phase-02 task-7 owns this assembly on every path). The resulting
        // full fact-spliced graph is the single graph consumed downstream.
        let reuse_holder;
        let reuse = match input.and_then(|i| i.reuse.as_ref()) {
            Some(plan) => {
                reuse_holder = cache::FactStore::at(plan.store_root.clone()).load();
                Some(plan.reuse(&reuse_holder))
            }
            None => None,
        };
        if let Some(r) = reuse.as_ref() {
            ingest::ingest_with_reuse(
                records,
                &ingest::IngestOptions {
                    blacklist,
                    language,
                    config,
                    base,
                },
                Some(r),
            )
        } else {
            ingest::ingest(
                records,
                &ingest::IngestOptions {
                    blacklist,
                    language,
                    config,
                    base,
                },
            )
        }
    };
    log.ln(&format!("Skipped {} blacklisted messages", report.skipped));
    if report.shadowed_modules > 0 {
        log.ln(&format!(
            "{} module(s) shadowed by a type of the same name (package/type collision; type wins)",
            report.shadowed_modules
        ));
    }
    if report.shadowed_functions > 0 {
        log.ln(&format!(
            "{} function(s) shadowed by a struct of the same FQN (struct wins)",
            report.shadowed_functions
        ));
    }

    let cleanup_report = cleanup(
        &mut graph,
        &CleanupOptions {
            user_excludes: path_excludes.to_vec(),
            language: language.to_string(),
        },
    );
    log.ln(&format!(
        "cleanup: removed {} nodes, {} contains, {} calls, {} uses, {} unresolved calls, {} unresolved uses, {} span violations",
        cleanup_report.nodes_removed,
        cleanup_report.contains_removed,
        cleanup_report.calls_removed,
        cleanup_report.uses_removed,
        cleanup_report.unresolved_calls_removed,
        cleanup_report.unresolved_uses_removed,
        cleanup_report.span_violations_removed,
    ));

    log.ln(&format!(
        "graph: {} nodes, {} contain edges, {} calls edges, {} uses edges, {} unresolved calls, {} unresolved uses",
        graph.nodes.len(),
        graph.contains.len(),
        graph.calls.len(),
        graph.uses.len(),
        graph.unresolved_calls.len(),
        graph.unresolved_uses.len(),
    ));

    // Win-B: record the just-assembled graph back into the shared
    // content-addressed store (manifest, scan record, per-file fact units, the
    // portable dep/signature/overload indexes). Recording is a best-effort
    // cache write — a failure downgrades the next scan to a full scan, never
    // the current graph.
    if let Some(input) = input
        && let Some(store_root) = &input.store_root
    {
        match incremental::record(
            store_root,
            &input.cache_key,
            &input.scan_root,
            &graph,
            &input.manifest,
            &input.sha,
            // The SAME phase-2 re-emission target set that drove the frontend
            // hand-off and the win-C splice, threaded through — never re-derived.
            // Empty on the full-scan fallback, so `record` keeps its full-scan
            // behaviour (phase-04 task-28 / task-13 AC (b)).
            &input.targets_rel,
        ) {
            Ok(()) => log.ln("[scan] content-addressed facts recorded"),
            Err(e) => log.ln(&format!("[scan] fact recording skipped: {e:#}")),
        }
    }

    // Ingest-assembly ends here; the DB build dispatch below is the db-load
    // phase (phase-04 task-4).
    let ingest_assembly = assembly_start.elapsed();
    let db_start = std::time::Instant::now();

    // `run_pipeline` runs from inside `<apg_root>/.trans` (both `cmd_scan` and
    // the hermetic test harness chdir there), so the previous/next artifacts
    // are `<apg_root>/.trans/{db.lbug,graph.jsonl}` (SPEC §6).
    let apg_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.parent().map(Path::to_path_buf));

    // Defense in depth (phase-03 lifecycle exclusivity): never unlink — or seed
    // from — a DB a live session holds. `cmd_scan` refuses earlier; this guard
    // catches a session that started mid-scan before the projected DB is
    // replaced.
    if let Some(apg_root) = &apg_root
        && session::live_session(apg_root)
    {
        panic!(
            "refused: a live apg session owns this db.lbug — run `apg session end` before scanning"
        );
    }

    // ---- DB build dispatch (win C, phase-03 task-4) ------------------------
    //
    // On the win-B incremental path the previous `db.lbug` already holds every
    // unaffected row, so seed a copy of it, apply the phase-2 delta as DML
    // (`splice`), and publish BOTH artifacts atomically instead of rebuilding
    // from scratch. The existing full load (remove + create_schema + copy_from
    // + write_graph_jsonl) stays the correctness reference and runs whenever the
    // splice is ineligible or fails mid-sequence. `input.reuse` is `Some`
    // exactly on the incremental path (it is `None` on every correctness
    // full-scan fallback), and `input.targets_rel`/`removed_fqns` are the SAME
    // phase-2 sets that drove the frontend target hand-off.
    let splice_report = match (input, apg_root.as_deref()) {
        (Some(input), Some(apg_root)) => try_splice_build(&graph, input, apg_root, log),
        _ => None,
    };
    if let Some(report) = splice_report {
        log.ln(&format!(
            "[load] splice: {} node(s) upserted, {} deleted, {} rel(s) re-inserted, {} unresolved GC'd, scan row refreshed: {}; full load skipped",
            report.nodes_upserted,
            report.nodes_deleted,
            report.edges_merged,
            report.unresolved_gc,
            report.scan_refreshed,
        ));
        // Export routing (phase-03 task-7): on the splice branch the export is
        // serialized by the UNCHANGED `load::write_graph_jsonl` into a
        // same-directory `.graph-*.tmp` sibling inside `splice::publish` above
        // and renamed in AFTER the spliced DB (DB first, then export), so both
        // artifacts of the P2-assembled full in-memory graph land atomically.
        // The full-load branch below keeps the standalone direct `graph.jsonl`
        // write, so every Export record kind/property still comes from the one
        // tested writer.
    } else {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        log.ln("[load] writing parquet load files...");
        load::build_load_files(&graph, &dir).unwrap();
        log.ln("[load] parquet files written");

        let _ = std::fs::remove_file("db.lbug");
        if std::path::Path::new("db.lbug").exists() {
            panic!(
                "db.lbug still exists (a previous run is still holding it?) — kill any stray apg/java processes and retry"
            );
        }
        log.ln("[load] Database::new...");
        let db = Database::new("db.lbug", Default::default()).unwrap();
        log.ln("[load] Database::new done");
        let conn = Connection::new(&db).unwrap();
        log.ln("[load] create_schema...");
        load::create_schema(&conn).unwrap();
        log.ln("[load] schema created");
        log.ln("[load] copy_from...");
        load::copy_from(&conn, &dir).unwrap();
        log.ln("[load] copy_from done");

        log.ln("[load] write_graph_jsonl...");
        load::write_graph_jsonl(&graph, std::path::Path::new("graph.jsonl")).unwrap();
        log.ln("[load] graph.jsonl written");

        log.ln("[load] dropping db...");
        drop(conn);
        drop(db);
        log.ln("[load] db dropped");
        let _ = std::fs::remove_dir_all(&dir);
        log.ln("[load] temp dir removed");
    }
    timing::PipelineTimings {
        ingest_assembly,
        db_load: db_start.elapsed(),
    }
}

/// The win-C DB-build dispatch (phase-03 task-4): try to seed the working
/// `db.lbug` from the previous scan and apply the phase-2 delta as DML, then
/// publish `db.lbug` + `graph.jsonl` atomically (`splice::publish`).
///
/// Returns the splice report on success; `None` on any ineligibility or
/// mid-sequence failure, in which case the caller runs the existing full load
/// (the correctness reference) and the previous artifacts are left in place.
/// This function never panics: a failure is a logged fallback.
///
/// `graph` is the win-B assembled graph (re-emitted target units PLUS cached
/// unaffected units) and `input` carries the same phase-2 target/removed sets
/// that drove the frontend hand-off — the delete scope is never re-derived.
///
/// The seed is EQUIVALENCE-GUARDED (feedback-101): the delta/manifest are
/// shared across worktrees while `db.lbug` is local, so the splice is refused
/// unless this worktree's seed was built from the same tree content the shared
/// [`incremental::Prepared::recorded_content_key`] names — see
/// [`splice::seed_checked`]. A refusal is a full load (the correctness
/// reference), never a published DB that diverges from a rebuild.
pub fn try_splice_build(
    graph: &graph::Graph,
    input: &incremental::PipelineInput,
    apg_root: &Path,
    log: &mut Log,
) -> Option<splice::SpliceReport> {
    // Eligibility: the win-B incremental path (a phase-2 delta/manifest exists)
    // with a previous DB to seed from. A full-scan fallback must never splice —
    // it has no target/removed set and its graph is the full universe.
    input.reuse.as_ref()?;
    let db = splice::db_path(apg_root);
    if !db.exists() {
        log.ln("[load] splice: no previous db.lbug to seed from — full load");
        return None;
    }
    // The seed is a WHOLE-FILE copy, so a previous DB that was not
    // checkpointed/closed cleanly (a leftover WAL/SHM sidecar) could lose its
    // unflushed rows in the copy. Fall back to the full load rather than
    // publish an incomplete database (task-1 checkpoint guard).
    for suffix in [".wal", ".shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", db.display()));
        if sidecar.exists() {
            log.ln(&format!(
                "[load] splice: previous db.lbug has a {suffix} sidecar (not cleanly closed) — full load"
            ));
            return None;
        }
    }
    // The delta refreshes the single Scan row; without the ingested scan_meta
    // there is nothing to write, so fall back rather than publish a bogus head.
    let Some(scan) = scan_row_from_graph(graph) else {
        log.ln("[load] splice: assembled graph carries no Scan row — full load");
        return None;
    };
    // The delete scope is the FINAL phase-2 target set (stage-1 ∪ the signature
    // cascade). The assembled graph carries repo-relative identities now, so the
    // delete scope includes each target's repo-relative identity verbatim;
    // the absolute spelling is also included so a graph built from absolute
    // fixture paths still matches.
    let targets: BTreeSet<String> = input
        .targets_rel
        .iter()
        .flat_map(|rel| [rel.clone(), incremental::absolute(&input.scan_root, rel)])
        .collect();

    let seeded = match splice::seed_checked(&db, input.recorded_content_key.as_deref()) {
        splice::SeedDecision::Seed(seeded) => seeded,
        splice::SeedDecision::FullLoad(reason) => {
            log.ln(&format!("[load] splice: {} — full load", reason.describe()));
            return None;
        }
    };

    let delta = splice::SpliceDelta {
        graph,
        targets: &targets,
        removed_fqns: &input.removed_fqns,
        scan,
    };
    let report = match seeded.apply(&delta) {
        Ok(report) => report,
        Err(e) => {
            // A mid-sequence failure leaves the seeded COPY partially mutated;
            // discard it and hand the caller to the full load. The previous DB
            // was only ever read, so it is untouched.
            log.ln(&format!(
                "[load] splice: delta application failed ({e:#}) — discarding seed, full load"
            ));
            let _ = seeded.discard();
            return None;
        }
    };

    match splice::publish(seeded, graph, &splice::export_path(apg_root)) {
        Ok(()) => Some(report),
        Err(e) => {
            // `publish` rolls both targets back to their previous bytes on a
            // reported failure, so the full load starts from a clean pair.
            log.ln(&format!(
                "[load] splice: publish failed ({e:#}) — falling back to the full load"
            ));
            None
        }
    }
}

/// The `ScanRow` the splice refreshes, read from the assembled graph's single
/// `Scan` node (the ingested `scan_meta`). `None` when the graph carries no
/// scan_meta, which makes the splice ineligible.
pub(crate) fn scan_row_from_graph(graph: &graph::Graph) -> Option<splice::ScanRow> {
    let node = graph.nodes.get(schema::SCAN_HEAD)?;
    Some(splice::ScanRow {
        git_sha: node.git_sha.clone(),
        git_clean: node.git_clean,
        content_key: node.content_key.clone(),
        scanned_at: node.scanned_at.clone().unwrap_or_default(),
    })
}
