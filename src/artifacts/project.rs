use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::schema::Record;
use crate::specs;

use super::db::ArtifactDb;
use super::merge::{edge_merge, node_fqn};

/// The exact set of node FQNs a transient metadata mutation must detach before
/// re-merging (phase-05 task-3): every FQN present before but gone now
/// (removed), every FQN whose node record changed (changed), and the source of
/// every edge that vanished — a MERGE-only re-merge never deletes a vanished
/// edge, so its surviving source is detached and re-merged with its current
/// out-edges. Detaching a node drops its incident edges; re-merging the full
/// post-mutation record set restores the current node and its current edges, so
/// the projection converges to the sources exactly.
pub(crate) fn transient_delta(before: &[Record], after: &[Record]) -> BTreeSet<String> {
    let before_nodes: HashMap<&str, &Record> = before
        .iter()
        .filter_map(|r| node_fqn(r).map(|f| (f, r)))
        .collect();
    let after_nodes: HashMap<&str, &Record> = after
        .iter()
        .filter_map(|r| node_fqn(r).map(|f| (f, r)))
        .collect();

    let mut deletes: BTreeSet<String> = BTreeSet::new();
    for (fqn, prior) in &before_nodes {
        match after_nodes.get(fqn) {
            // Unchanged (same node record) — leave it and its edges in place.
            Some(current) if current == prior => {}
            // Removed, or changed (its dropped incident edges must not survive).
            _ => {
                deletes.insert((*fqn).to_string());
            }
        }
    }

    // A vanished edge is only a MERGE away from surviving. Detach its source
    // (which is guaranteed to be part of the merged set — a transient record's
    // source is itself transient) so the stale out-edge drops and the re-merge
    // restores the current edge set.
    let edge_key = |r: &Record| {
        edge_merge(r).map(|(table, from, to)| (table.to_string(), from.to_string(), to.to_string()))
    };
    let before_edges: BTreeSet<(String, String, String)> =
        before.iter().filter_map(edge_key).collect();
    let after_edges: BTreeSet<(String, String, String)> =
        after.iter().filter_map(edge_key).collect();
    for (_, from, _) in before_edges.difference(&after_edges) {
        if after_nodes.contains_key(from.as_str()) {
            deletes.insert(from.clone());
        }
    }
    deletes
}

/// Applies the exact transient projection delta in ONE transaction: detach
/// exactly `deletes` (the removed ∪ changed FQNs plus vanished-edge sources),
/// then re-merge `records` — nodes first, then edges. A removed planned FQN
/// with no project prefix is deleted by exact FQN; an added node/edge is
/// MERGEd. A failure rolls the whole transaction back, so the DB keeps its
/// prior committed state.
pub(crate) fn reingest_project_with(
    apg_root: &Path,
    deletes: &BTreeSet<String>,
    records: &[Record],
) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    let conn = db.conn()?;
    conn.query("BEGIN TRANSACTION")?;
    let result = (|| -> anyhow::Result<()> {
        db.detach_delete_project(&conn, deletes)?;
        // Test-only injection: prove a mid-apply failure rolls the projection
        // back to its prior state (phase-05 task-10). Compiled unconditionally
        // (a no-op unless a test installs a hook) so the relocated
        // `tests/artifacts_e2e.rs` integration crate can reach
        // `install_projection_hook`.
        fire_projection_hook()?;
        db.merge_records(&conn, records)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.query("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            // A failed query aborts the write transaction in the engine; roll
            // back so the DB keeps its prior committed state (no residue from
            // the failed mutation). The transaction may already be gone.
            let _ = conn.query("ROLLBACK");
            Err(e)
        }
    }
}

// A test-only one-shot injection fired inside a transient projection apply,
// after the delete set is detached and before the records are re-merged.
// Tests use it to force a mid-apply failure and assert the transaction rolls
// back to the prior projection (phase-05 task-10). Compiled unconditionally
// (never installed outside tests, so the hook is a no-op) so the relocated
// `tests/artifacts_e2e.rs` integration crate can reach
// `install_projection_hook`.
type ProjectionHook = Box<dyn FnOnce() -> anyhow::Result<()>>;

thread_local! {
    static PROJECTION_HOOK: std::cell::RefCell<Option<ProjectionHook>> =
        const { std::cell::RefCell::new(None) };
}

pub fn install_projection_hook(hook: impl FnOnce() -> anyhow::Result<()> + 'static) {
    PROJECTION_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

fn fire_projection_hook() -> anyhow::Result<()> {
    let hook = PROJECTION_HOOK.with(|cell| cell.borrow_mut().take());
    match hook {
        Some(hook) => hook(),
        None => Ok(()),
    }
}

/// Assembles the record set a re-ingest merges: the project's transient
/// state — the plan store plus the five feedback tier mirrors (SPEC §5:
/// `.trans/plans/<project>.jsonl` and `.trans/<tier>/<project>.jsonl`).
/// Feedback on durable/code nodes lives in the mirrors, and every file shares
/// the project's `<project>/feedback-<n>` namespace, so a re-ingest after ANY
/// of them must merge ALL of them (the delta's delete set covers every changed
/// or removed FQN; a write-through that forgot the other mirrors would silently
/// erase their feedback from the DB). The committed spec/note
/// durable halves are gone — spec data lives in the `apg/layers` tree,
/// re-ingested separately. When `substitute` names one of the transient
/// files, it contributes `records` instead of its on-disk content.
pub(crate) fn assembled_records(
    apg_root: &Path,
    project: &str,
    substitute: Option<(&Path, &[Record])>,
) -> anyhow::Result<Vec<Record>> {
    let (sub_path, sub_records) = match substitute {
        Some((p, r)) => (Some(p), r),
        None => (None, &[][..]),
    };

    let mut records: Vec<Record> = Vec::new();
    for f in specs::project_transient_files(apg_root, project) {
        let sub_is_f = sub_path == Some(f.as_path());
        if sub_is_f || f.exists() {
            if sub_is_f {
                records.extend_from_slice(sub_records);
            } else {
                records.extend(specs::read_jsonl(&f)?);
            }
        }
    }
    Ok(records)
}

/// The next free `feedback-<n>` / `note-<n>` number for a project, scanning
/// the given records (fqn suffix after `feedback-`/`note-`).
pub fn next_free(records: &[Record], kind: &str) -> u64 {
    let prefix = format!("{kind}-");
    records
        .iter()
        .filter_map(|r| node_fqn(r))
        .filter_map(|f| {
            let (_, suffix) = f.split_once(&format!("/{prefix}"))?;
            suffix.parse::<u64>().ok()
        })
        .max()
        .map(|n| n + 1)
        .unwrap_or(1)
}

/// The two code-reference universes `layers::ingest_tree` validates
/// `implemented-by` targets against, read from the **live DB**: `scanned` =
/// every code-node FQN the last scan produced (Module/File/Struct/Function
/// without `status: planned`), `planned` = the planned-node FQNs still awaiting
/// realization (`status: planned`).
///
/// This opens `apg/.trans/db.lbug` read-write, so it must NOT be used on the
/// direct node/edge mutation path (phase-02 decoupled that path onto
/// [`code_universes_from_export`]); its remaining callers are the flock-guarded
/// plan paths (`plan_cmd`), where an exclusive DB read is already serialized.
pub fn code_universes(apg_root: &Path) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let db = ArtifactDb::open(apg_root)?;
    let conn = db.conn()?;
    let mut scanned = BTreeSet::new();
    let mut planned = BTreeSet::new();
    for label in ["Module", "File", "Struct", "Function"] {
        let result = conn.query(&format!("MATCH (n:{label}) RETURN n.fqn, n.status"))?;
        for row in result {
            let fqn = row.first().map(|v| v.to_string()).unwrap_or_default();
            let status = row.get(1).map(|v| v.to_string()).unwrap_or_default();
            if status == "planned" {
                planned.insert(fqn);
            } else {
                scanned.insert(fqn);
            }
        }
    }
    Ok((scanned, planned))
}

/// The two code-reference universes `layers::validate_change` / `ingest_tree`
/// validate `implemented-by` targets against, resolved **without opening
/// `db.lbug`** (phase-02 DB decoupling):
///
/// - `scanned` — the real code FQNs from the `apg/.trans/graph.jsonl` export
///   (the sole code-identity source: a `module`/`file`/`struct`/`function`
///   record without `status: planned`). `graph.jsonl` is written only by a
///   scan, so a FQN absent from it is not called drift here while the export is
///   missing — the caller gates on the export (see `layers::validate_change`).
/// - `planned` — the FQNs the plan store declares still awaiting realization
///   (`Record::PlannedNode` in every `apg/.trans/plans/<project>.jsonl`), UNION
///   any `status: planned` code record already projected into the export. A
///   declared-but-unscanned FQN (e.g. `apg.session.Coordinator` before its scan)
///   therefore classifies Pending, never drift.
///
/// The export is parsed as generic JSON: only the four Implementation record
/// types and their `fqn`/`status` fields are consulted, and the `scan_meta`
/// control record on line 1 (or any spec/plan/edge record) is skipped.
pub fn code_universes_from_export(
    apg_root: &Path,
) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut scanned = BTreeSet::new();
    let mut planned = BTreeSet::new();

    let export = apg_root.join(specs::TRANS).join("graph.jsonl");
    if export.exists() {
        let text = std::fs::read_to_string(&export)?;
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                anyhow::anyhow!(
                    "{}:{}: bad graph.jsonl record: {e}",
                    export.display(),
                    i + 1
                )
            })?;
            let Some(kind) = value.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            if !matches!(kind, "module" | "file" | "struct" | "function") {
                continue;
            }
            let Some(fqn) = value.get("fqn").and_then(|f| f.as_str()) else {
                continue;
            };
            let status = value.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if status == "planned" {
                planned.insert(fqn.to_string());
            } else {
                scanned.insert(fqn.to_string());
            }
        }
    }

    // The plan store's planned-node declarations. A plan store FQN already
    // scanned stays in both sets; `classify_code_ref` prefers `scanned` (Real).
    for f in specs::plan_files(apg_root) {
        for record in specs::read_jsonl(&f)? {
            if let Record::PlannedNode { fqn, .. } = record {
                planned.insert(fqn);
            }
        }
    }

    Ok((scanned, planned))
}

/// Re-ingests the durable layers delta into the live DB after a node/edge
/// mutation: detaches exactly the mutated FQNs `deletes` (removed ∪ changed —
/// never a whole layer-dir prefix) and re-merges the caller-supplied `records`
/// (the `layers::ingest_tree` output) — nodes first, then edges, in one
/// transaction. A failed merge rolls back, so the DB keeps its prior committed
/// state.
pub fn reingest_layers(
    apg_root: &Path,
    deletes: &BTreeSet<String>,
    records: &[Record],
) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    db.reingest_layers_on(deletes, records)
}

impl ArtifactDb {
    /// The write-through projection apply for the durable layers tree, run
    /// against an **already-held** database handle: detach exactly the mutated
    /// FQNs `deletes` ([`detach_delete_project`], with the planned-code guard) and
    /// re-merge the caller-supplied `records` (the `layers::ingest_tree`
    /// output) — nodes first, then edges, in one transaction. A failed merge
    /// rolls back, so the DB keeps its prior committed state.
    ///
    /// Split from the free [`reingest_layers`](crate::artifacts::reingest_layers)
    /// so the phase-03 session coordinator can amortize ONE DB open across N
    /// routed mutations: the coordinator owns the handle for the session's life
    /// and applies every mutation's projection delta synchronously through it —
    /// the open is amortized, visibility never is.
    pub fn reingest_layers_on(
        &self,
        deletes: &BTreeSet<String>,
        records: &[Record],
    ) -> anyhow::Result<()> {
        let conn = self.conn()?;
        conn.query("BEGIN TRANSACTION")?;
        let result = (|| -> anyhow::Result<()> {
            self.detach_delete_project(&conn, deletes)?;
            self.merge_records(&conn, records)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                conn.query("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.query("ROLLBACK");
                Err(e)
            }
        }
    }
}
