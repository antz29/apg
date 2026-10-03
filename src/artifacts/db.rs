use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use lbug::{Connection, Database, SystemConfig};

use crate::git;
use crate::layers::{InEdge, NodeFile, OutEdge, parse_fqn};
use crate::load;
use crate::schema::Record;
use crate::specs;
use crate::splice::seed::{cell, query_rows};

use super::project::{assembled_records, reingest_project_with, transient_delta};

/// The four Implementation labels a plan can declare as a **planned** node. A
/// planned placeholder carries `status = 'planned'`; when a branch scan realizes
/// the FQN as real code the row loses that marker (`status IS NULL`), and it
/// must never be detached by a metadata-mutation delete (phase-05 task-1).
pub(crate) const PLANNED_CODE_LABELS: [&str; 4] = ["Module", "File", "Struct", "Function"];

/// Code-graph labels that are never metadata: the four Implementation labels
/// (which can also hold a planned placeholder — see [`PLANNED_CODE_LABELS`]),
/// the scan control node, and unresolved references. A metadata delta must
/// never detach `UnresolvedTarget`/`Scan` by FQN — they are scanned code, not
/// spec/plan state.
pub(crate) const CODE_GRAPH_LABELS: [&str; 7] = [
    "Module",
    "File",
    "Struct",
    "Function",
    "UnresolvedTarget",
    "Scan",
    "Language",
];

/// The process-wide reentrant extended write lock (see `acquire_spec_lock`):
/// one `LOCK_EX` flock per lock-file path, held while any live guard exists and
/// released when the outermost guard drops (closing the fd).
static SPEC_LOCK: OnceLock<Mutex<SpecLockState>> = OnceLock::new();

/// The held flocks, keyed by lock-file path (a process can host several
/// fixtures/projects; a test process holds more than one at a time). Each entry
/// records the open `File` (the flock's open file description) and the nested
/// acquisition depth.
#[derive(Default)]
struct SpecLockState {
    held: HashMap<PathBuf, (File, usize)>,
}

/// A held acquisition of the extended write lock. Dropping the last live guard
/// for a lock file releases its flock (the `File` is removed, closing the fd);
/// nested acquisitions share the one fd, so a command that needs a second
/// acquisition (plan complete loading both plan and spec) never self-deadlocks.
#[must_use = "dropping the guard immediately releases the whole-durable-sequence lock"]
pub struct SpecLockGuard {
    lock_path: PathBuf,
}

impl Drop for SpecLockGuard {
    fn drop(&mut self) {
        let holder = SPEC_LOCK
            .get()
            .expect("a SpecLockGuard exists, so the state is initialized");
        let mut state = holder.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, depth)) = state.held.get_mut(&self.lock_path) {
            *depth -= 1;
            if *depth == 0 {
                // Dropping the File closes the fd, releasing the flock.
                state.held.remove(&self.lock_path);
            }
        }
    }
}

/// Acquires the exclusive cross-process lock that serializes a whole durable
/// sequence on the project: the **extended** spec/plan/review write lock.
///
/// A live session takes it exactly once at `Coordinator::start`, before any
/// node-file read, and holds it for the session's whole life — across every
/// admitted mutation's buffered write plus the single commit at `apg session
/// save` and its projection. The direct plan/review authoring paths (and the
/// JSONL funnel they write through) take the same lock for their own sequence.
/// That single flock is what serializes the three contended locks a parallel
/// burst hits: the node-file read-modify-write, git's `.git/index.lock` (inside
/// `git::commit_files`), and the read-write `db.lbug` projection apply. Because
/// the session holds it for its life, it and any direct writer are mutually
/// exclusive.
///
/// Reentrant within the process: a nested acquire returns a guard that shares
/// the already-held fd (incrementing the depth), so a command that acquires
/// twice does not deadlock; the flock is released only when the outermost guard
/// drops. The flock lives on `apg/.trans/specs.lock`.
pub fn acquire_spec_lock(apg_root: &Path) -> anyhow::Result<SpecLockGuard> {
    let holder = SPEC_LOCK.get_or_init(|| Mutex::new(SpecLockState::default()));
    let mut state = holder.lock().unwrap_or_else(|e| e.into_inner());
    let lock_path = apg_root.join(".trans").join("specs.lock");
    if let Some((_, depth)) = state.held.get_mut(&lock_path) {
        // Reentrant: share the existing fd, just add a nesting level.
        *depth += 1;
        return Ok(SpecLockGuard { lock_path });
    }
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = File::create(&lock_path)?;
    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&f), libc::LOCK_EX) };
        if rc != 0 {
            anyhow::bail!("could not acquire write lock {}", lock_path.display());
        }
    }
    state.held.insert(lock_path.clone(), (f, 1));
    Ok(SpecLockGuard { lock_path })
}

/// Writes `records` to `path` and re-ingests the project into the live DB.
/// A missing DB (no scan yet) is not an error — the JSONL is the durable
/// form — but a re-ingest failure when the DB exists is a hard error: the
/// mutation must be visible in the query index, and silently dropping it is
/// what let the authoring agents believe writes had landed when they had not.
///
/// This is the **central mutation funnel** (R4): every spec/plan/review/
/// invariant mutation — and every code-note ledger write (`add_note`'s
/// code-target branch) — routes through here, so the two gates beside each
/// other cover them all:
///
/// 1. **Membership guard** (R3): writes only happen inside a project context —
///    the project's worktree at `<main>/apg/.worktrees/<project>`, on the
///    project's branch. A refused mutation names which membership half failed
///    plus one fix line (exit 1 at the CLI). Reads are always unguarded; main
///    is never a mutation place. Universal-scope targets (the shared
///    `_invariants.jsonl` ledger) require any project context — scope is
///    orthogonal to mutation context (R3).
///
/// 2. **Refuse-on-stale gate** (agent-loop hardening): when a DB exists **and**
///    the DB is stale (`is_stale` — the tree moved on since the scan that
///    built it), the mutation bails *before* any JSONL write or re-ingest.
///    Missing-DB and non-git paths stay allowed (non-git paths cannot pass
///    the membership guard anyway).
///
/// **Auto-commit** (R8): after the durable write lands (and before the
/// projection), a durable target this funnel serves (anything outside the
/// gitignored `apg/.trans/`) is committed on the project branch via git2,
/// single-file diffs, and the staleness gate's recorded `scan_meta` is
/// re-anchored to the new state (DB and tree in sync by construction;
/// consecutive mutations do not each demand a rescan). Transient `.trans`
/// writes never commit: `apg/.trans` is gitignored and transient by design.
/// An auto-commit failure degrades to a warning on stderr: the mutation already
/// landed, and the staleness gate will demand a scan before the next one (the
/// same degradation as a hand-committed change).
///
/// Durable `apg node` / `apg edge` mutations do NOT pass through this funnel:
/// the live session admits them into its write-back buffer and projects each at
/// admission, and the whole buffered set becomes durable in exactly ONE commit
/// at `apg session save`. This funnel's per-write commit is therefore not the
/// durable-mutation contract.
///
/// **Commit-then-project** (phase-05 tasks 3/12): the durable/transient file
/// write and its commit land FIRST — the system-of-record durability point —
/// and only then is the exact projection delta applied to `db.lbug`. The delta
/// is computed HERE, inside the funnel, from the assembled transient record set
/// before and after this write (no per-caller threading): the delete set is
/// exactly the removed ∪ changed FQNs plus the sources of vanished edges
/// ([`transient_delta`]), never a `<project>/` prefix. A crash between the write
/// and the projection is reproduced by the next rebuild as the committed state,
/// never the uncommitted projection. The rename is still atomic (a sibling temp
/// swapped over `path`), so a crash mid-write never leaves half a JSONL.
pub fn write_jsonl_and_reingest(
    apg_root: &Path,
    path: &Path,
    project: &str,
    records: &[Record],
) -> anyhow::Result<()> {
    // Membership guard (R3/R4): every write happens inside a project context.
    // Universal-scope targets (the `_invariants.jsonl` shared ledger) have no
    // project of their own — any project context satisfies the guard.
    let universal = path.file_name().is_some_and(|n| n == "_invariants.jsonl");
    if universal {
        git::require_project_context(apg_root)?;
    } else {
        git::require_membership(apg_root, project)?;
    }
    let has_db = apg_root.join(specs::TRANS).join("db.lbug").exists();
    if has_db && let Some(msg) = git::refusal_message(apg_root) {
        anyhow::bail!("{msg}");
    }

    // The exact delta, computed while the committed file still holds the old
    // content. `after` substitutes the in-memory records for this file; every
    // other transient file is read as-is. A durable target (the shared
    // `_invariants.jsonl` ledger) is not part of the transient set: merge
    // exactly the records written.
    let delta = if has_db && path.starts_with(apg_root.join(specs::TRANS)) {
        let before = assembled_records(apg_root, project, None)?;
        let after = assembled_records(apg_root, project, Some((path, records)))?;
        Some((transient_delta(&before, &after), after))
    } else if has_db {
        Some((BTreeSet::new(), records.to_vec()))
    } else {
        None
    };

    // 1. Durable file write (the commit): atomic temp + rename.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    specs::write_jsonl(&tmp, records)?;
    std::fs::rename(&tmp, path)?;

    // 2. Auto-commit durable targets, then re-anchor scan_meta.
    //
    // Capture the post-commit state so the DB's OWN `Scan` row can be brought to
    // the identical state (step 4): the re-anchor is graph.jsonl-only, and the
    // two halves must name the same git state.
    let mut reanchored: Option<git::GitState> = None;
    if !path.starts_with(apg_root.join(specs::TRANS)) {
        match git::auto_commit(apg_root, path) {
            Ok(Some(_)) => {
                let state = git::git_state(apg_root);
                match git::reanchor_scan_meta(apg_root, &state) {
                    Ok(()) => reanchored = Some(state),
                    Err(e) => {
                        eprintln!(
                            "apg: warning: could not re-anchor scan_meta after auto-commit: {e:#}"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "apg: warning: mutation landed but auto-commit failed ({e:#}): the staleness gate will demand a scan before the next mutation"
                );
            }
        }
    }

    // 3. Projection delta AFTER the commit. A failure rolls the projection
    //    transaction back and reports failure; the committed file is the system
    //    of record and the next rebuild reproduces it.
    if let Some((deletes, after)) = delta {
        reingest_project_with(apg_root, &deletes, &after)?;
    }

    // 4. Reconcile the DB's own `Scan` row to the re-anchored graph.jsonl state.
    //    A transient (`apg/.trans/`) write never commits and never re-anchors
    //    line 1, so its DB `Scan` row must stay put — refresh only when the
    //    durable commit re-anchored a live DB. The DB is opened from scratch
    //    (the projection's own open has already closed); a failure degrades to a
    //    warning, exactly like the re-anchor it mirrors (the durable write
    //    already landed, and the next scan rebuilds).
    if let (Some(state), true) = (&reanchored, has_db) {
        match ArtifactDb::open(apg_root) {
            Ok(db) => {
                if let Err(e) = db.refresh_scan_row(
                    state.sha.as_deref(),
                    state.sha.as_ref().map(|_| state.clean),
                    state.content_key.as_deref(),
                ) {
                    eprintln!(
                        "apg: warning: could not refresh the DB Scan row after commit: {e:#}"
                    );
                }
            }
            Err(e) => {
                eprintln!("apg: warning: could not open the DB to refresh its Scan row: {e:#}");
            }
        }
    }
    Ok(())
}

pub struct ArtifactDb {
    pub db: Database,
}

/// True when `fqn` resolves to a node in the live graph (any kind). Used by
/// the test suite and the tool surface; `#[allow(dead_code)]` because the
/// shipping CLI paths test existence via label queries.
#[allow(dead_code)]
fn node_exists(db: &Database, fqn: &str) -> bool {
    count(
        db,
        &format!("MATCH (n {{fqn: {}}}) RETURN count(*)", lit(fqn)),
    ) > 0
}

/// Runs `RETURN count(*)` and returns the number. Public so the relocated e2e
/// integration crate (`tests/artifacts_e2e.rs`) reaches it as
/// `apg::artifacts::count`.
pub fn count(db: &Database, q: &str) -> i64 {
    Connection::new(db)
        .and_then(|c| c.query(q))
        .map(|r| {
            r.to_string()
                .lines()
                .last()
                .and_then(|l| l.trim().parse().ok())
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Single-quotes a value for a Cypher string literal (escapes `\`, `'`).
pub fn lit(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

impl ArtifactDb {
    pub fn open(apg_root: &Path) -> anyhow::Result<ArtifactDb> {
        let db_path = apg_root.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            anyhow::bail!(
                "{} does not exist — run `apg scan` first",
                db_path.display()
            );
        }
        let db = Database::new(&db_path, SystemConfig::default())?;
        Ok(ArtifactDb { db })
    }

    /// A fresh connection to the owned database (borrows `self`, so any
    /// returned rows must be consumed before the next call).
    pub fn conn(&self) -> anyhow::Result<Connection<'_>> {
        Ok(Connection::new(&self.db)?)
    }

    /// Runs a query and returns its formatted output. Used by the test suite
    /// (the relocated e2e crates reach it as `apg::artifacts::ArtifactDb::q`);
    /// the shipping CLI paths use label-typed queries via
    /// [`count`](Self::count) or the query subcommand.
    pub fn q(&self, query: &str) -> anyhow::Result<String> {
        Ok(self.conn()?.query(query)?.to_string())
    }

    /// Existence of any node at `fqn` in the live graph. Used by the test suite
    /// and the tool surface; `#[allow(dead_code)]` because the shipping CLI paths
    /// test existence via label queries.
    #[allow(dead_code)]
    pub fn has_node(&self, fqn: &str) -> bool {
        node_exists(&self.db, fqn)
    }

    /// The code-graph label of `fqn` (Function/Struct/File/Module/
    /// UnresolvedTarget), or `None` when it is not a code node.
    pub fn code_label(&self, fqn: &str) -> Option<&'static str> {
        for l in ["Function", "Struct", "File", "Module", "UnresolvedTarget"] {
            if count(
                &self.db,
                &format!("MATCH (n:{l} {{fqn: {}}}) RETURN count(*)", lit(fqn)),
            ) > 0
            {
                return Some(l);
            }
        }
        None
    }

    /// The **Implementation** label of `fqn` — one of the four Implementation
    /// tier labels (Module/File/Struct/Function), or `None`. Unlike
    /// [`code_label`](Self::code_label) this deliberately EXCLUDES
    /// `UnresolvedTarget`: an unresolved reference is not real code. Used by
    /// the apply gate's planned-node realization check — a planned node is
    /// realized only when a scanned Implementation node occupies its FQN
    /// (PlanCompletion-SPEC.md; a planned FQN that happens to match an
    /// UnresolvedTarget is not code).
    pub fn impl_label(&self, fqn: &str) -> Option<&'static str> {
        for l in ["Function", "Struct", "File", "Module"] {
            if count(
                &self.db,
                &format!("MATCH (n:{l} {{fqn: {}}}) RETURN count(*)", lit(fqn)),
            ) > 0
            {
                return Some(l);
            }
        }
        None
    }

    /// The DB label of any node — code or spec/plan — or `None` when `fqn`
    /// does not resolve. Unlike [`code_label`](Self::code_label) (code tables
    /// only), this covers every node table; `add_note` uses it to decide
    /// whether a `--on` target is an allowable `Details` target (R2).
    pub fn node_label(&self, fqn: &str) -> Option<&'static str> {
        load::node_labels().iter().copied().find(|l| {
            count(
                &self.db,
                &format!("MATCH (n:{l} {{fqn: {}}}) RETURN count(*)", lit(fqn)),
            ) > 0
        })
    }

    /// True when `fqn` is a `planned` Implementation node (a plan-writer-authored
    /// placeholder awaiting realization — GraphModel-SPEC.md). The placeholder
    /// node is gone (PHASE_02); pending anchors are detected by `status: planned`,
    /// never a separate kind.
    /// The label of a `planned` Implementation node at `fqn`, or `None`.
    pub fn is_planned(&self, fqn: &str) -> bool {
        for l in ["Struct", "Function", "File", "Module"] {
            if count(
                &self.db,
                &format!(
                    "MATCH (n:{l} {{fqn: {}}}) WHERE n.status = 'planned' RETURN count(*)",
                    lit(fqn)
                ),
            ) > 0
            {
                return true;
            }
        }
        false
    }

    /// The two code-reference universes `layers::validate_change_over` (and the
    /// direct `validate_change`) validate `implemented-by` targets against, read
    /// from the **live database this handle already holds** — the DB-side twin
    /// of [`crate::artifacts::code_universes_from_export`], so session admission
    /// never opens a second `db.lbug` handle nor reads `apg/.trans/graph.jsonl`:
    ///
    /// - `scanned` — the real code FQNs of the four Implementation labels
    ///   (`Module`/`File`/`Struct`/`Function`) whose `status` is not `planned`.
    /// - `planned` — the FQNs of those labels carrying `status = 'planned'` (a
    ///   plan-writer placeholder awaiting realization). The plan store's
    ///   `Record::PlannedNode` declarations are projected into exactly these rows
    ///   by the authoring paths, so the held DB reproduces the export reader's
    ///   union without touching the transient plan files.
    ///
    /// Unlike the free [`crate::artifacts::code_universes`] (which opens its own
    /// read-write handle), this borrows the caller's held handle, so the session
    /// amortizes ONE open across its whole life. `UnresolvedTarget` is
    /// deliberately excluded — an unresolved reference is not code.
    pub fn code_universes_from_db(&self) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
        let conn = self.conn()?;
        let mut scanned = BTreeSet::new();
        let mut planned = BTreeSet::new();
        for label in PLANNED_CODE_LABELS {
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

    /// DELETE-then-CREATE the single `Scan` row (fqn [`crate::schema::SCAN_HEAD`],
    /// `scan/HEAD`) on the **already-open** database, setting it to the same git
    /// state the caller just re-anchored `graph.jsonl`'s `scan_meta` lead to.
    ///
    /// This is the DB-side half of every durability point's reconciliation: a
    /// durable commit re-anchors `graph.jsonl` line 1 (`git::reanchor_scan_meta`,
    /// which never opens the DB), and this primitive brings the DB's OWN `Scan`
    /// row to the identical state so [`crate::git::db_recorded_scan`] and
    /// [`crate::git::recorded_scan`] can never disagree.
    ///
    /// The columns mirror the two existing writers exactly — the splice's step 9
    /// ([`crate::splice::apply`]) and [`crate::load::tables::build_load_files`]'s
    /// `Scan` table: `git_sha`/`content_key` are empty strings when `None`,
    /// `git_clean` renders `"true"`/`"false"` (empty when `None`), and
    /// `scanned_at` is **preserved** from the row being replaced — a metadata
    /// mutation re-anchors, it never re-scans, so the scan's own timestamp must
    /// survive.
    ///
    /// Runs on a fresh connection over the caller's handle, so the caller never
    /// reopens `db.lbug`.
    pub fn refresh_scan_row(
        &self,
        git_sha: Option<&str>,
        git_clean: Option<bool>,
        content_key: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn()?;
        let (_, rows) = query_rows(&conn, "MATCH (s:Scan) RETURN s.scanned_at AS scanned_at")?;
        let scanned_at = rows
            .first()
            .map(|row| cell(row, 0))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(crate::git::now_iso8601);
        conn.query("MATCH (s:Scan) DELETE s")?;
        conn.query(&format!(
            "CREATE (s:Scan {{fqn: {}, git_sha: {}, git_clean: {}, content_key: {}, scanned_at: {}}})",
            lit(crate::schema::SCAN_HEAD),
            lit(git_sha.unwrap_or("")),
            lit(&git_clean.map(|c| c.to_string()).unwrap_or_default()),
            lit(content_key.unwrap_or("")),
            lit(&scanned_at),
        ))?;
        Ok(())
    }

    /// Reconstruct the durable `apg/layers/**` node-file set from the **live
    /// database** — the inverse of the layers projection
    /// ([`reingest_layers_on`](Self::reingest_layers_on) / `merge_records`).
    ///
    /// Every durable authored node is read back from its node table (identity
    /// from the FQN segments `layer.type.name`, `body` from the `body` column,
    /// and the projected metadata columns back into the node-file `properties`
    /// map), and every authored edge is read back from its rel table into
    /// **both** halves: the source node's `out` edge and the target node's `in`
    /// edge. A code-endpoint edge (`implemented-by`, a code-targeted `details`)
    /// has only the source's `out` half, exactly as a node file carries it — a
    /// code node has no file to hold the in half.
    ///
    /// Only the durable layers (requirements, domain, solution, implementation,
    /// global) are reconstructed; the transient `.trans` node tables (`Plan`,
    /// `PlanPhase`, `Task`, `Feedback`) are not node files and are skipped. The
    /// result is ordered by `(layer, node_type, name)` and each node's edges by
    /// `(kind, endpoint)`, matching [`crate::layers::read_existing_nodes`]'s
    /// deterministic order.
    ///
    /// **Fidelity limit.** This is the exact inverse of what the schema
    /// *stores*: no authored rel table has a property column and the node tables
    /// only carry the named columns, so an edge is reconstructed with an empty
    /// `properties` map and node-file metadata the schema does not name (a
    /// `Group`'s arbitrary short-id, an `Entity`'s `kind`) is not reconstructed.
    /// A node file whose `properties` are the projected keys
    /// (`id`/`feature`/`kind`/`attribute`/`root`/`attaches-to`) round-trips.
    pub fn node_files_from_db(&self) -> anyhow::Result<Vec<NodeFile>> {
        let conn = self.conn()?;
        let mut by_fqn: BTreeMap<String, NodeFile> = BTreeMap::new();

        // One durable node table per node-file type the durable tree can hold.
        for (table, columns, body_col, props) in DURABLE_NODE_TABLES {
            collect_durable_nodes(&conn, table, columns, body_col, props, &mut by_fqn)?;
        }

        // Both halves of every authored edge, read back from the rel tables. A
        // rel row whose source is not a durable node (a code pair of the shared
        // Contains/Calls/Uses tables, or a transient Plan/Feedback source) is
        // skipped; a row whose target is not a durable node contributes only the
        // source's out half (a code endpoint has no node file).
        for (table, kind) in DURABLE_EDGE_TABLES {
            let (_, rows) = query_rows(
                &conn,
                &format!("MATCH (a)-[:{table}]->(b) RETURN a.fqn, b.fqn"),
            )?;
            for row in rows {
                let from = cell(&row, 0);
                let to = cell(&row, 1);
                if let Some(src) = by_fqn.get_mut(&from) {
                    src.out.push(OutEdge {
                        kind: kind.to_string(),
                        target: to.clone(),
                        properties: BTreeMap::new(),
                    });
                }
                if let Some(dst) = by_fqn.get_mut(&to) {
                    dst.in_edges.push(InEdge {
                        kind: kind.to_string(),
                        source: from.clone(),
                        properties: BTreeMap::new(),
                    });
                }
            }
        }

        let mut nodes: Vec<NodeFile> = by_fqn.into_values().collect();
        for node in &mut nodes {
            node.out
                .sort_by(|a, b| (&a.kind, &a.target).cmp(&(&b.kind, &b.target)));
            node.in_edges
                .sort_by(|a, b| (&a.kind, &a.source).cmp(&(&b.kind, &b.source)));
        }
        nodes.sort_by(|a, b| {
            (&a.layer, &a.node_type, &a.name).cmp(&(&b.layer, &b.node_type, &b.name))
        });
        Ok(nodes)
    }
}

/// One durable node table's reconstruction shape: `(DB table label, non-FQN
/// columns in `create_schema` order, the body column, the metadata columns and
/// the node-file `properties` key each feeds)`.
type DurableNodeTable = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Every durable node table — the `apg/layers/**` node-file layers
/// (requirements, domain, solution, implementation, global) — with the metadata
/// columns a node file's `properties` map carries. An `Entity`'s `kind` is
/// absent deliberately: the schema has no entity-kind column (the projection
/// does not carry it), so it cannot be reconstructed. The transient `.trans`
/// tables (`Feedback`, `Plan`, `PlanPhase`, `Task`) are not node files and are
/// absent.
const DURABLE_NODE_TABLES: &[DurableNodeTable] = &[
    (
        "Requirement",
        &["id", "title", "body", "feature"],
        "body",
        &[("id", "id"), ("feature", "feature")],
    ),
    ("Stakeholder", &["name", "body"], "body", &[]),
    ("User", &["name", "body"], "body", &[]),
    ("Note", &["body", "kind"], "body", &[("kind", "kind")]),
    (
        "Constraint",
        &["name", "body", "attaches_to"],
        "body",
        &[("attaches_to", "attaches-to")],
    ),
    (
        "DomainGroup",
        &["name", "attribute", "root", "body"],
        "body",
        &[("attribute", "attribute"), ("root", "root")],
    ),
    ("Entity", &["name", "body"], "body", &[]),
    ("Value", &["name", "body"], "body", &[]),
    ("Service", &["name", "body"], "body", &[]),
    ("System", &["name", "body"], "body", &[]),
    (
        "Container",
        &["name", "kind", "body"],
        "body",
        &[("kind", "kind")],
    ),
    ("Component", &["name", "body"], "body", &[]),
    ("Person", &["name", "body"], "body", &[]),
];

/// Every durable authored edge rel table and the node-file edge kind it
/// carries. The shared `Contains`/`Calls`/`Uses` tables also hold code rows; a
/// row whose source is not a durable node is skipped, so only authored pairs
/// are reconstructed.
const DURABLE_EDGE_TABLES: [(&str, &str); 11] = [
    ("Contains", "contains"),
    ("Drives", "drives"),
    ("RealisedBy", "realised-by"),
    ("SpecImplementedBy", "implemented-by"),
    ("Calls", "calls"),
    ("Publishes", "publishes"),
    ("Subscribes", "subscribes"),
    ("DependsOn", "depends-on"),
    ("Uses", "uses"),
    ("Represents", "represents"),
    ("Details", "details"),
];

/// Read one durable node table into `nodes`, keyed by its FQN: identity from
/// the FQN segments, `body` from `body_col`, and each metadata column back into
/// the node-file `properties` map when non-empty (an absent column projects to
/// the empty string, which round-trips to absent). A malformed FQN is a hard
/// error — the projection never writes one.
fn collect_durable_nodes(
    conn: &Connection,
    table: &str,
    columns: &[&str],
    body_col: &str,
    props: &[(&str, &str)],
    nodes: &mut BTreeMap<String, NodeFile>,
) -> anyhow::Result<()> {
    let projection = std::iter::once("n.fqn".to_string())
        .chain(columns.iter().map(|c| format!("n.`{c}`")))
        .collect::<Vec<_>>()
        .join(", ");
    let (_, rows) = query_rows(conn, &format!("MATCH (n:{table}) RETURN {projection}"))?;
    // Positions are positional, never header-name based: the engine names a
    // projected property `n.<col>`, so `fqn` is index 0 and `columns` follow in
    // order.
    let column_index = |column: &str| -> anyhow::Result<usize> {
        columns
            .iter()
            .position(|c| *c == column)
            .map(|i| i + 1)
            .ok_or_else(|| anyhow::anyhow!("{table}: no `{column}` column in the projection"))
    };
    let body_i = column_index(body_col)?;
    let prop_i: Vec<(usize, &str)> = props
        .iter()
        .map(|(column, key)| Ok((column_index(column)?, *key)))
        .collect::<anyhow::Result<_>>()?;

    for row in rows {
        let f = cell(&row, 0);
        let (layer, node_type, name) =
            parse_fqn(&f).map_err(|e| anyhow::anyhow!("{table} `{f}`: {e}"))?;
        let mut properties = BTreeMap::new();
        for (i, key) in &prop_i {
            let value = cell(&row, *i);
            if !value.is_empty() {
                properties.insert((*key).to_string(), value);
            }
        }
        nodes.insert(
            f,
            NodeFile {
                layer: layer.layer_dir().to_string(),
                node_type,
                name,
                body: cell(&row, body_i),
                properties,
                out: Vec::new(),
                in_edges: Vec::new(),
            },
        );
    }
    Ok(())
}
