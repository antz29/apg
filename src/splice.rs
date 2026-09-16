//! Win-C DB seed (phase-03 task-1): seed the working database from the previous
//! scan's `apg/.trans/db.lbug` by a **whole-file copy**.
//!
//! The splice mechanism is deliberately unambiguous — copy file → open copied
//! DB → apply the delta as DML → atomically publish:
//!
//! 1. **this module (task-1)** — resolve/validate the previous DB, copy it as a
//!    whole file to a temp sibling in the SAME directory (so the eventual
//!    publish is a same-filesystem `rename`, never a cross-device copy), and
//!    open the copy read-write;
//! 2. **task-2** — apply the delta to the copy as DML only (delete-then-insert
//!    of the affected rows). No parquet build and no `COPY` of the unaffected
//!    data: the whole-file copy already preserves every unaffected node/rel row,
//!    so the splice path runs no full-load pass;
//! 3. **task-3** — checkpoint+close the spliced copy, then atomically publish
//!    BOTH artifacts: the `db.lbug` rename over `target_path` and the fresh
//!    `graph.jsonl` rename over its `.trans/` sibling — with backups and
//!    rollback, so a partial swap can never leave a new DB paired with a stale
//!    export;
//! 4. **task-4** — the pipeline dispatch that decides seed-vs-full-load.
//!
//! This is seed-from-previous, not overlay-on-empty, per
//! `domain.note.cache-store-and-splice`.
//!
//! ## Invalidation → the existing full load
//!
//! A seed is only valid when the previous DB **exists** AND its
//! **schema/format/version** is compatible with this binary:
//!
//! * no `db.lbug` at all → [`SeedFallback::MissingPrevious`];
//! * a file this binary cannot open (a different LadybugDB storage format, or
//!   corruption) → [`SeedFallback::Unreadable`];
//! * a DB that opens but whose schema differs from this binary's
//!   [`crate::load::create_schema`] → [`SeedFallback::IncompatibleSchema`].
//!
//! Any of these hands the caller back to the existing
//! `remove + create_schema + copy_from` full load. The full load stays the
//! **correctness reference** (`domain.constraint.db-splice-equivalence`): a
//! spliced DB must answer identically to a full rebuild, so the seed is an
//! optimization that must be provably equivalent, never a second source of
//! truth.
//!
//! The expected schema is not hand-maintained. It is produced by running this
//! binary's own `create_schema` against a throwaway in-memory DB and
//! introspecting the result, so any future schema change automatically
//! invalidates seeds written by an older/newer binary with no list to update.

// The phase-03 surface is staged: the delta (task-2), the publish (task-3), and
// the pipeline dispatch (task-4) consume these entry points. Until they land the
// module is compiled but not yet wired into `run_pipeline` — the same staging
// `incremental` (phase-02) used before task-4.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use lbug::{Connection, Database, SystemConfig};

use crate::graph::{Graph, NodeKind};
use crate::load;
use crate::schema::SCAN_HEAD;

/// The previous `db.lbug` path under an `apg/` layout root.
pub fn db_path(apg_root: &Path) -> PathBuf {
    apg_root.join(crate::specs::TRANS).join("db.lbug")
}

/// Seeds the splice working DB from the database at `previous`.
///
/// Happy path: `previous` exists, opens, and its schema matches this binary's —
/// the file is whole-copied to a temp sibling in the same directory and the copy
/// is opened read-write. Every validation failure yields a [`SeedFallback`] and
/// the caller runs the existing full load. The previous DB is only ever read (it
/// is opened read-only during validation) and is never modified.
///
/// The caller (task-4) owns the disposition of the returned copy: pass it to
/// task-2/task-3, or [`discard`](SeededDb::discard) it when abandoning the
/// splice.
pub fn seed(previous: &Path) -> SeedDecision {
    if !previous.exists() {
        return SeedDecision::FullLoad(SeedFallback::MissingPrevious);
    }
    if let Err(fallback) = validate_compatible(previous) {
        return SeedDecision::FullLoad(fallback);
    }

    let temp_path = temp_sibling(previous);
    // A leftover at this exact pid+nanos path is impossible, but a clear first
    // guarantees `fs::copy` can never fail on one.
    let _ = std::fs::remove_file(&temp_path);
    if let Err(e) = std::fs::copy(previous, &temp_path) {
        return SeedDecision::FullLoad(SeedFallback::Unreadable(format!(
            "seed copy of {} failed: {e}",
            previous.display()
        )));
    }

    match Database::new(&temp_path, SystemConfig::default()) {
        Ok(db) => SeedDecision::Seed(SeededDb {
            db,
            temp_path,
            target_path: previous.to_path_buf(),
        }),
        Err(e) => {
            // Never leave a half-seeded temp behind for task-3 to publish.
            let _ = std::fs::remove_file(&temp_path);
            SeedDecision::FullLoad(SeedFallback::SeededCopyUnreadable(e.to_string()))
        }
    }
}

/// [`seed`] over an `apg/` layout root: resolves
/// `<apg_root>/.trans/db.lbug` and seeds from it.
pub fn seed_from_apg_root(apg_root: &Path) -> SeedDecision {
    seed(&db_path(apg_root))
}

/// The content-identity key recorded in `previous`'s single `Scan` row
/// (SCAN_HEAD), or `None` when the DB carries no `Scan` row or an empty key (a
/// pre-hardening DB). This is the identity of the tree the DB was BUILT from —
/// the seed's own account of itself.
pub fn recorded_content_key(previous: &Path) -> anyhow::Result<Option<String>> {
    let db = Database::new(previous, SystemConfig::default().read_only(true))?;
    let conn = Connection::new(&db)?;
    let (_, rows) = query_rows(&conn, "MATCH (s:Scan) RETURN s.content_key AS content_key")?;
    Ok(rows.first().map(|r| cell(r, 0)).filter(|s| !s.is_empty()))
}

/// The equivalence-guarded seed (feedback-101).
///
/// The delta/manifest are **shared** across worktrees
/// (`<git-common-dir>/apg/facts`) while the seeded DB is **local** to this
/// worktree. The splice is exact only when the LOCAL seed was built from the
/// SAME tree content the shared [`crate::delta::ScanRecord`] (and hence the
/// delta) was derived from. If another worktree (or main) scans in between, the
/// shared record advances past this worktree's DB: the manifest diff no longer
/// describes the local DB, an empty/partial target set leaves the stale seeded
/// rows in place, and the published DB is not a full rebuild — which the next
/// freshness fast-path then reuses. `content_key` folds the HEAD sha and the
/// working-tree/index/untracked content, so an equal key means the same tree
/// content; a mismatch (or a missing key on either side) is ineligible.
///
/// `expected` is the shared recorded key (`ScanRecord::content_key`) captured
/// BEFORE the completed scan rewrites it. The caller runs the existing full
/// load on any [`SeedFallback`], keeping it the correctness reference.
pub fn seed_checked(previous: &Path, expected: Option<&str>) -> SeedDecision {
    if !previous.exists() {
        return SeedDecision::FullLoad(SeedFallback::MissingPrevious);
    }
    let seed_key = match recorded_content_key(previous) {
        Ok(key) => key,
        Err(e) => return SeedDecision::FullLoad(SeedFallback::Unreadable(e.to_string())),
    };
    // A missing key on either side cannot be verified as equivalent.
    let current = matches!((seed_key.as_deref(), expected), (Some(s), Some(r)) if s == r);
    if !current {
        return SeedDecision::FullLoad(SeedFallback::StaleSeed {
            seed: seed_key.unwrap_or_else(|| "(none)".to_string()),
            recorded: expected.unwrap_or("(none)").to_string(),
        });
    }
    seed(previous)
}

/// Opens `previous` read-only and compares its structural fingerprint to this
/// binary's `create_schema`. A read failure or a fingerprint mismatch is the
/// invalidation. No temp file is created on this path.
fn validate_compatible(previous: &Path) -> Result<(), SeedFallback> {
    let db = Database::new(previous, SystemConfig::default().read_only(true))
        .map_err(|e| SeedFallback::Unreadable(e.to_string()))?;
    let conn = Connection::new(&db).map_err(|e| SeedFallback::Unreadable(e.to_string()))?;
    let actual = extract_schema(&conn).map_err(|e| SeedFallback::Unreadable(e.to_string()))?;
    let expected = expected_schema()
        .map_err(|e| SeedFallback::Unreadable(format!("cannot derive the expected schema: {e}")))?;
    if actual != expected {
        return Err(SeedFallback::IncompatibleSchema(diff_summary(
            &actual, &expected,
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Schema fingerprint
// ---------------------------------------------------------------------------

/// One column of a node/rel table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnShape {
    name: String,
    type_name: String,
    primary_key: bool,
}

/// One table's structural shape: its kind (`NODE`/`REL`), its ordered columns,
/// and — for a rel table — its declared `(from, to)` connections.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableShape {
    kind: String,
    columns: Vec<ColumnShape>,
    connections: Vec<(String, String)>,
}

/// The whole-DB structural fingerprint: table name → shape. A `BTreeMap` so the
/// engine's internal table ordering never affects the comparison.
type SchemaFingerprint = BTreeMap<String, TableShape>;

/// This binary's expected schema fingerprint: run its own `create_schema`
/// against a throwaway in-memory DB and introspect it. Deriving the expectation
/// from the very DDL the full load uses makes the check self-maintaining — a
/// schema change can never leave a stale expectation behind.
fn expected_schema() -> anyhow::Result<SchemaFingerprint> {
    let db = Database::in_memory(SystemConfig::default())?;
    let conn = Connection::new(&db)?;
    load::create_schema(&conn)?;
    extract_schema(&conn)
}

/// Reads the whole structural fingerprint of the open DB: every table's name,
/// kind (`NODE`/`REL`), columns (name/type/primary-key) and, for rel tables, its
/// declared `(from, to)` connections.
fn extract_schema(conn: &Connection) -> anyhow::Result<SchemaFingerprint> {
    let (names, rows) = query_rows(conn, "CALL show_tables() RETURN name, type")?;
    let name_i = column_index(&names, "name")?;
    let type_i = column_index(&names, "type")?;

    let mut out = BTreeMap::new();
    for row in rows {
        let name = cell(&row, name_i);
        let kind = cell(&row, type_i);
        let columns = table_columns(conn, &name)?;
        let connections = if kind == "REL" {
            table_connections(conn, &name)?
        } else {
            Vec::new()
        };
        out.insert(
            name,
            TableShape {
                kind,
                columns,
                connections,
            },
        );
    }
    Ok(out)
}

/// The ordered columns of `table` (`primary key` is absent on rel tables).
fn table_columns(conn: &Connection, table: &str) -> anyhow::Result<Vec<ColumnShape>> {
    let (names, rows) = query_rows(
        conn,
        &format!("CALL table_info('{}') RETURN *", cypher_escape(table)),
    )?;
    let name_i = column_index(&names, "name")?;
    let type_i = column_index(&names, "type")?;
    let pk_i = names.iter().position(|n| n == "primary key");

    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        columns.push(ColumnShape {
            name: cell(&row, name_i),
            type_name: cell(&row, type_i),
            primary_key: pk_i.is_some_and(|i| cell(&row, i) == "True"),
        });
    }
    Ok(columns)
}

/// The declared `(from, to)` connections of a rel `table`.
fn table_connections(conn: &Connection, table: &str) -> anyhow::Result<Vec<(String, String)>> {
    let (names, rows) = query_rows(
        conn,
        &format!("CALL show_connection('{}') RETURN *", cypher_escape(table)),
    )?;
    let from_i = column_index(&names, "source table name")?;
    let to_i = column_index(&names, "destination table name")?;
    Ok(rows
        .into_iter()
        .map(|r| (cell(&r, from_i), cell(&r, to_i)))
        .collect())
}

/// A compact human explanation of the fingerprint difference — the
/// missing/extra/changed table names only (a full column diff would be noise on
/// a scan log line).
fn diff_summary(actual: &SchemaFingerprint, expected: &SchemaFingerprint) -> String {
    let missing: Vec<&str> = expected
        .keys()
        .filter(|k| !actual.contains_key(*k))
        .map(String::as_str)
        .collect();
    let extra: Vec<&str> = actual
        .keys()
        .filter(|k| !expected.contains_key(*k))
        .map(String::as_str)
        .collect();
    let changed: Vec<&str> = expected
        .iter()
        .filter(|(k, v)| actual.get(*k).is_some_and(|a| a != *v))
        .map(|(k, _)| k.as_str())
        .collect();

    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing tables: {}", missing.join(", ")));
    }
    if !extra.is_empty() {
        parts.push(format!("unexpected tables: {}", extra.join(", ")));
    }
    if !changed.is_empty() {
        parts.push(format!("changed tables: {}", changed.join(", ")));
    }
    if parts.is_empty() {
        parts.push("schema differs".to_string());
    }
    parts.join("; ")
}

// ---------------------------------------------------------------------------
// Query plumbing
// ---------------------------------------------------------------------------

/// Runs `query` and returns its column names plus every row's cells as strings.
fn query_rows(conn: &Connection, query: &str) -> anyhow::Result<(Vec<String>, Vec<Vec<String>>)> {
    let result = conn.query(query)?;
    let names = result.get_column_names();
    let rows = result
        .map(|row| row.iter().map(|v| v.to_string()).collect())
        .collect();
    Ok((names, rows))
}

/// The index of column `want` in a result's header.
fn column_index(names: &[String], want: &str) -> anyhow::Result<usize> {
    names
        .iter()
        .position(|n| n == want)
        .ok_or_else(|| anyhow::anyhow!("query result is missing the `{want}` column: {names:?}"))
}

/// Cell `i` of `row`, or an empty string when the row is short.
fn cell(row: &[String], i: usize) -> String {
    row.get(i).cloned().unwrap_or_default()
}

/// Escapes a value for a single-quoted Cypher string literal.
fn cypher_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The temp sibling of `target`, in the SAME directory as `target` so task-3's
/// publish is an atomic, same-filesystem `rename`.
fn temp_sibling(target: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "db.lbug".to_string());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".{file}.seed-{}-{nanos}.tmp", std::process::id()))
}

// ---------------------------------------------------------------------------
// The exposed decision surface
// ---------------------------------------------------------------------------

/// A seeded working DB: the whole-file copy of the previous DB, open and ready
/// for task-2's DML delta.
pub struct SeededDb {
    /// The open copy. Task-2 opens a [`Connection`] on it and applies the delta.
    ///
    /// This handle must be DROPPED before task-3 renames `temp_path` over
    /// `target_path`: a live handle keeps the old inode open, so the rename
    /// would swap the directory entry without the open handle writing to the
    /// published file.
    pub db: Database,
    /// The temp sibling holding the copy; task-3 renames it over `target_path`.
    pub temp_path: PathBuf,
    /// The previous `db.lbug` the copy was seeded from, and the rename target.
    pub target_path: PathBuf,
}

impl SeededDb {
    /// A fresh connection to the seeded copy — task-2's DML surface.
    pub fn conn(&self) -> anyhow::Result<Connection<'_>> {
        Ok(Connection::new(&self.db)?)
    }

    /// Removes the temp copy — the abandon path when the splice is dropped
    /// before the task-3 publish (e.g. a delta-application failure falls back to
    /// the full load). A missing temp is not an error.
    pub fn discard(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.temp_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Why the seed was invalidated and the existing full load must run. Every
/// variant means the same thing to the caller: run
/// `remove + create_schema + copy_from`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedFallback {
    /// There is no previous `db.lbug` to seed from.
    MissingPrevious,
    /// The previous file could not be opened/read — a storage-format/version
    /// mismatch or corruption. Carries the engine error.
    Unreadable(String),
    /// The previous DB opened but its schema differs from this binary's
    /// `create_schema` — a schema/version mismatch. Carries a table summary.
    IncompatibleSchema(String),
    /// The previous DB was built from a **different tree** than the shared scan
    /// record the delta was derived from — another worktree (or main) scanned
    /// in between, so the shared manifest no longer describes this LOCAL DB.
    /// Seeding it and applying the delta would publish a DB that is not a full
    /// rebuild, so the caller runs the full load. Carries the seed's recorded
    /// content-identity key and the expected (shared) one (feedback-101).
    StaleSeed { seed: String, recorded: String },
    /// The whole-file copy succeeded but the copied DB could not be opened.
    SeededCopyUnreadable(String),
}

impl SeedFallback {
    /// A one-line human description for the scan log / dispatch seam.
    pub fn describe(&self) -> String {
        match self {
            SeedFallback::MissingPrevious => "no previous db.lbug to seed from".to_string(),
            SeedFallback::Unreadable(e) => {
                format!("previous db.lbug is not usable by this binary: {e}")
            }
            SeedFallback::IncompatibleSchema(d) => {
                format!("previous db.lbug schema is incompatible: {d}")
            }
            SeedFallback::StaleSeed { seed, recorded } => format!(
                "previous db.lbug was built from content {seed} but the shared scan record is {recorded} (another worktree scanned) — full load"
            ),
            SeedFallback::SeededCopyUnreadable(e) => {
                format!("seeded db.lbug copy could not be opened: {e}")
            }
        }
    }
}

/// The seed half's decision: splice from the seeded copy, or fall back to the
/// existing full load.
pub enum SeedDecision {
    /// The previous DB was copied, opened, and is ready for task-2's delta.
    Seed(SeededDb),
    /// The seed was invalidated — run the existing full load.
    FullLoad(SeedFallback),
}

// ---------------------------------------------------------------------------
// Delta application (phase-03 task-2)
// ---------------------------------------------------------------------------
//
// The seeded DB is the previous scan's `db.lbug`, whole-file copied and opened
// read-write (task-1). Task-2 applies **only the delta** as DML: the re-emitted
// target units are upserted in place, the units whose FQN disappears are
// detached, the shared `UnresolvedTarget` rows are lifecycle-managed by
// reference, and the single `Scan` row (SCAN_HEAD) is refreshed. No parquet
// build and no `COPY`: every unaffected row survives byte-for-byte from the
// seed, which is what makes a spliced DB equal a full rebuild
// (`domain.constraint.db-splice-equivalence`).
//
// ## The input is the win-B assembled graph, not the target-only spool
//
// `graph` is the full assembled graph (`ingest::ingest_with_reuse`'s output:
// the re-emitted target units PLUS the cached unaffected units). The splicer
// selects the **delta rows** from it:
//
// * a Struct/Function is a delta unit when its location path is in `targets`;
// * a File is a delta unit when its FQN (the absolute path) is in `targets`;
// * a Module is always re-upserted (there are few, and the module set is part
//   of the equivalence — a removed file can orphan its module);
// * an UnresolvedTarget is inserted only when a delta edge first references it.
//
// Reading the FULL assembled graph (rather than a target-only spool) is what
// keeps a target unit's edge to an **unaffected** unit: the frontend emits such
// a cross-cut edge with its canonical FQN, and the cached endpoint only exists
// in the assembled graph — a target-only graph would have pruned it in
// `finalize_graph` before the splicer ever saw it.
//
// ## Scheme (feedback-90, option (i): upsert in place + replace outgoing rels)
//
// For a **persisting** FQN (body-only or signature change):
//   1. delete every rel it AUTHORS (outgoing Calls/Uses/UnresolvedCall/
//      UnresolvedUse/Contains) — delete-before-insert holds for rels;
//   2. UPSERT the node in place (`MERGE … SET`, the `ArtifactDb::merge_node`
//      pattern) — never a node delete, so INCOMING Calls/Uses from units
//      OUTSIDE the re-emission target set survive untouched (a body-only change
//      re-emits only the changed file, so its callers are not in the delta);
//   3. MERGE its new outgoing rels from the delta (the `ArtifactDb::merge_edge`
//      rel-upsert pattern).
//
// For a **disappearing** FQN (removed file/module, or an overload re-suffix
// retiring an old FQN): delete its authored rels, then `DETACH DELETE` the
// node. The silent incoming-edge drop is safe ONLY here: a disappearing
// exported symbol is a signature change, so the reverse-dependency closure put
// every referrer in the re-emission target set, and their replacement outgoing
// rels are merged **before** the `DETACH DELETE` runs. No edge a full rebuild
// keeps is lost. DETACH-DELETEing a persisting FQN is forbidden.
//
// ## Authored/transient seed assumption
//
// The seed already holds the CURRENT authored/transient tables (Requirement/
// Entity/Note/Constraint/Plan/PlanPhase/Task/Feedback and their rels) by
// write-through: every durable/transient mutation synchronously projects into
// `db.lbug`. The splice writes no authored row — `graph` is the **code**
// assembly (scanner records spliced with cached fact units), never the layer/
// transient records, so a planned node can never be re-marked over a realized
// one. A missing/incompatible previous DB falls back to the full load
// (task-1/task-4).

/// The single `Scan` row (SCAN_HEAD) a spliced DB must carry. Mirrors the
/// `scan_meta` control record (`schema::Record::ScanMeta`) and the columns
/// `build_load_files` writes from a `Scan` node: `git_sha`/`git_clean` are
/// `None` outside a git repo (stored as empty strings), and `content_key` is
/// the phase-01 content-identity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRow {
    pub git_sha: Option<String>,
    pub git_clean: Option<bool>,
    pub content_key: Option<String>,
    pub scanned_at: String,
}

/// The delta a spliced DB must apply, expressed against the seeded copy.
///
/// `graph` is the win-B assembled graph (target units + cached unaffected
/// units, see the module comment); `targets` is the FULL phase-2 re-emission
/// target set — changed files ∪ reverse-dependency closure ∪ overload-group
/// peers — and is the **delete scope**; `removed_fqns` is the subtraction half
/// of the full-universe seam (`incremental::Prepared::removed_fqns`); `scan` is
/// the new SCAN_HEAD row.
pub struct SpliceDelta<'a> {
    pub graph: &'a Graph,
    pub targets: &'a BTreeSet<String>,
    pub removed_fqns: &'a BTreeSet<String>,
    pub scan: ScanRow,
}

/// What the delta application touched — logged by the pipeline and asserted by
/// the unit tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpliceReport {
    /// Nodes MERGE-upserted in place (delta code units + modules + first-
    /// referenced UnresolvedTargets).
    pub nodes_upserted: u64,
    /// Nodes DETACH-DELETEd (the disappeared units).
    pub nodes_deleted: u64,
    /// Outgoing rels deleted from the delta/delete units before re-insert.
    pub edges_deleted: u64,
    /// New outgoing rels merged from the delta.
    pub edges_merged: u64,
    /// UnresolvedTarget rows GC'd after the delta rels landed.
    pub unresolved_gc: u64,
    /// Whether the SCAN_HEAD row was refreshed.
    pub scan_refreshed: bool,
}

impl SeededDb {
    /// Applies `delta` to the seeded copy (task-2's DML half) on a fresh
    /// connection. Drop the returned report; the seeded DB is then ready for
    /// task-3's atomic publish.
    pub fn apply(&self, delta: &SpliceDelta<'_>) -> anyhow::Result<SpliceReport> {
        let conn = self.conn()?;
        apply(&conn, delta)
    }
}

/// Applies `delta` to the open (seeded) DB. Idempotent for a repeated identical
/// delta: an upsert re-sets the same props and a re-merge of a present rel is a
/// no-op. Runs as a sequence of DML statements; a mid-sequence failure leaves
/// the seed partially mutated, which the caller abandons via
/// `SeededDb::discard` and re-runs through the full load (task-4).
pub fn apply(conn: &Connection, delta: &SpliceDelta<'_>) -> anyhow::Result<SpliceReport> {
    let mut report = SpliceReport::default();

    // The module delete scope is bounded by the delta's target set: a seeded
    // Module is only re-decided when the delta reaches a File somewhere in its
    // subtree. The assembled graph cannot vouch for a module the delta never
    // touched — a skipped/emission-filtered language emits no global module
    // scaffolding, so its `Module -> Module` hierarchy and pure-intermediate
    // modules never reach the assembled graph. Deleting such a module (or
    // wiping its outgoing Contains rels) would drop rows a full rebuild keeps
    // (`domain.constraint.db-splice-equivalence`).
    let subtrees = module_file_subtrees(conn)?;
    let reached = |m: &str| {
        subtrees
            .get(m)
            .is_some_and(|files| files.iter().any(|f| delta.targets.contains(f)))
    };
    // A module can only be *disappearing* when EVERY file under it is in the
    // delta scope: any unchanged file keeps it alive in a full rebuild.
    let fully_reached = |m: &str| {
        subtrees.get(m).is_some_and(|files| {
            !files.is_empty() && files.iter().all(|f| delta.targets.contains(f))
        })
    };

    // --- 1. Select the delta's code units from the assembled graph ---------
    //
    // The seeded Module FQNs, fetched ONCE (also reused for the delete scope in
    // step 2): a module the seed does not own — a package/dir the previous scan
    // did not have — is always re-decided, because the seed can vouch for
    // neither its row nor its outgoing Contains rels.
    let seeded_modules: BTreeSet<String> = seed_modules(conn)?.into_iter().collect();
    let mut delta_funcs: Vec<String> = Vec::new();
    let mut delta_structs: Vec<String> = Vec::new();
    let mut delta_files: Vec<String> = Vec::new();
    let mut delta_modules: Vec<String> = Vec::new();
    // FQN -> label for every node the delta owns (code units + modules).
    let mut delta_labels: HashMap<String, &'static str> = HashMap::new();
    for (fqn, node) in &delta.graph.nodes {
        match node.kind {
            NodeKind::Module => {
                // A module is re-decided when the delta's target set reaches a
                // File in its SEED subtree, OR when the seed has no Module row
                // for it at all (a NEW package/dir: the assembled graph is the
                // only source of truth for its node and its Contains rels). An
                // otherwise-untouched module's seed row and outgoing Contains
                // rels are already exact, and the assembled graph may not even
                // carry them (a skipped language's scaffolding) — that guard is
                // preserved. A delta File/Module added UNDER an untouched module
                // is handled additively by the `Contains` merge in step 6.
                if reached(fqn) || !seeded_modules.contains(fqn) {
                    delta_modules.push(fqn.clone());
                    delta_labels.insert(fqn.clone(), "Module");
                }
            }
            NodeKind::Struct => {
                if location_in_targets(node, delta.targets) {
                    delta_structs.push(fqn.clone());
                    delta_labels.insert(fqn.clone(), "Struct");
                }
            }
            NodeKind::Function => {
                if location_in_targets(node, delta.targets) {
                    delta_funcs.push(fqn.clone());
                    delta_labels.insert(fqn.clone(), "Function");
                }
            }
            NodeKind::File if delta.targets.contains(fqn) => {
                delta_files.push(fqn.clone());
                delta_labels.insert(fqn.clone(), "File");
            }
            _ => {}
        }
    }

    // --- 2. The delete scope: seed units owned by the target set -----------
    //
    // The delete scope is the WHOLE re-emission target set, never just the
    // changed+removed files: an overload re-suffix retires the old FQN of a
    // unit in an *unchanged* peer file, and a cascaded dependent's new facts
    // can retire FQNs too. A seeded code unit whose location path is a target
    // path (or whose FQN directly names one, for a File) is therefore in scope;
    // it disappears iff the assembled graph does not re-emit it.
    let mut in_scope: BTreeMap<String, &'static str> = code_fqns_in_paths(conn, delta.targets)?;
    for m in &seeded_modules {
        // Only a module whose ENTIRE file subtree is in the delta can be
        // genuinely gone; an untouched module (or one with any reused file left)
        // is left exactly as the seed left it.
        if fully_reached(m) {
            in_scope.insert(m.clone(), "Module");
        }
    }
    for fqn in delta.removed_fqns {
        if !in_scope.contains_key(fqn)
            && let Some(label) = db_code_label(conn, fqn)?
        {
            in_scope.insert(fqn.clone(), label);
        }
    }

    let mut disappearing: BTreeMap<String, &'static str> = BTreeMap::new();
    for (fqn, label) in &in_scope {
        if !delta_labels.contains_key(fqn) {
            disappearing.insert(fqn.clone(), label);
        }
    }

    // --- 3. Delete every rel authored by a delta OR disappearing unit -------
    //
    // Bulk per (label, declared rel-set): an authored rel is deleted before its
    // node is upserted (persisting) or detached (disappearing), so the delta's
    // new rels are the only ones left. `edges_deleted` is the pre-count.
    let mut delete_funcs: BTreeSet<String> = delta_funcs.iter().cloned().collect();
    let mut delete_structs: BTreeSet<String> = delta_structs.iter().cloned().collect();
    let mut delete_files: BTreeSet<String> = delta_files.iter().cloned().collect();
    let mut delete_modules: BTreeSet<String> = delta_modules.iter().cloned().collect();
    for (fqn, label) in &disappearing {
        match *label {
            "Function" => {
                delete_funcs.insert(fqn.clone());
            }
            "Struct" => {
                delete_structs.insert(fqn.clone());
            }
            "File" => {
                delete_files.insert(fqn.clone());
            }
            "Module" => {
                delete_modules.insert(fqn.clone());
            }
            _ => {}
        }
    }
    report.edges_deleted +=
        delete_authored_rels(conn, "Function", AUTH_RELS_FUNCTION, &delete_funcs)?;
    report.edges_deleted +=
        delete_authored_rels(conn, "Struct", AUTH_RELS_STRUCT, &delete_structs)?;
    report.edges_deleted += delete_authored_rels(conn, "File", AUTH_RELS_CONTAINS, &delete_files)?;
    report.edges_deleted +=
        delete_authored_rels(conn, "Module", AUTH_RELS_CONTAINS, &delete_modules)?;

    // --- 4. Upsert the delta's nodes in place ------------------------------
    for fqn in &delta_funcs {
        upsert_node(conn, "Function", fqn, &delta.graph.nodes[fqn])?;
        report.nodes_upserted += 1;
    }
    for fqn in &delta_structs {
        upsert_node(conn, "Struct", fqn, &delta.graph.nodes[fqn])?;
        report.nodes_upserted += 1;
    }
    for fqn in &delta_files {
        upsert_node(conn, "File", fqn, &delta.graph.nodes[fqn])?;
        report.nodes_upserted += 1;
    }
    for fqn in &delta_modules {
        if let Some(node) = delta.graph.nodes.get(fqn) {
            upsert_node(conn, "Module", fqn, node)?;
            report.nodes_upserted += 1;
        }
    }

    // --- 5. Insert every first-referenced UnresolvedTarget, before its edge -
    let mut delta_unresolved: BTreeSet<String> = BTreeSet::new();
    for (from, to, _) in &delta.graph.unresolved_calls {
        if delta_labels.contains_key(from)
            && delta
                .graph
                .nodes
                .get(to)
                .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget)
        {
            delta_unresolved.insert(to.clone());
        }
    }
    for (from, to) in &delta.graph.unresolved_uses {
        if delta_labels.contains_key(from)
            && delta
                .graph
                .nodes
                .get(to)
                .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget)
        {
            delta_unresolved.insert(to.clone());
        }
    }
    for fqn in &delta_unresolved {
        if let Some(node) = delta.graph.nodes.get(fqn) {
            upsert_node(conn, "UnresolvedTarget", fqn, node)?;
            report.nodes_upserted += 1;
        }
    }

    // --- 6. MERGE the delta's new outgoing rels ----------------------------
    let mut resolved: HashMap<String, Option<&'static str>> = HashMap::new();
    let mut merge = |table: &str, from: &str, to: &str, target_type: &str| -> anyhow::Result<()> {
        // The delta authors an edge when it owns the SOURCE unit. `Contains` is
        // the one exception (feedback-118): a seeded Module the delta did NOT
        // re-decide still authors `Module -> File` / `Module -> Module` to a
        // child the delta ADDED — a full rebuild carries that edge and the seed
        // cannot, so it must be merged ADDITIVELY (nothing is deleted for that
        // module, so its pre-existing Contains rels stay untouched).
        let from_label = match delta_labels.get(from).copied() {
            Some(label) => label,
            None if table == "Contains" && delta_labels.contains_key(to) => {
                match endpoint_label(conn, from, delta.graph, &mut resolved)? {
                    Some(label) => label,
                    // An unknown/unseeded source: the full load prunes it too.
                    None => return Ok(()),
                }
            }
            // An edge authored outside the delta is untouched.
            None => return Ok(()),
        };
        // An edge into a unit that is being detached would be dropped by the
        // DETACH DELETE anyway; a full rebuild prunes it as dangling.
        if disappearing.contains_key(to) {
            return Ok(());
        }
        let to_label = endpoint_label(conn, to, delta.graph, &mut resolved)?;
        let Some(to_label) = to_label else {
            return Ok(()); // dangling endpoint — the full load prunes it too
        };
        if !pair_allowed(table, from_label, to_label) {
            return Ok(());
        }
        if table == "UnresolvedCall" {
            conn.query(&format!(
                "MATCH (a:{from_label} {{fqn: {}}}), (b:{to_label} {{fqn: {}}}) \
                 MERGE (a)-[r:UnresolvedCall]->(b) SET r.target_type = {}",
                lit(from),
                lit(to),
                lit(target_type)
            ))?;
        } else {
            conn.query(&format!(
                "MATCH (a:{from_label} {{fqn: {}}}), (b:{to_label} {{fqn: {}}}) MERGE (a)-[:{table}]->(b)",
                lit(from),
                lit(to)
            ))?;
        }
        report.edges_merged += 1;
        Ok(())
    };
    for (from, to) in &delta.graph.contains {
        merge("Contains", from, to, "")?;
    }
    for (from, to) in &delta.graph.calls {
        merge("Calls", from, to, "")?;
    }
    for (from, to) in &delta.graph.uses {
        merge("Uses", from, to, "")?;
    }
    for (from, to, tt) in &delta.graph.unresolved_calls {
        merge("UnresolvedCall", from, to, tt)?;
    }
    for (from, to) in &delta.graph.unresolved_uses {
        merge("UnresolvedUse", from, to, "")?;
    }

    // --- 7. DETACH DELETE the disappeared units ----------------------------
    //
    // After every replacement rel has been merged (step 6): the only edges left
    // pointing at a disappearing node are stale incoming edges whose referrer
    // either re-emitted a replacement or was never in the reverse closure.
    for (label, fqns) in group_by_label(&disappearing) {
        detach_delete(conn, label, &fqns)?;
        report.nodes_deleted += fqns.len() as u64;
    }

    // --- 8. GC the shared UnresolvedTarget rows by reference ---------------
    //
    // UnresolvedTarget is deduplicated by FQN and SHARED across units, never
    // unit-owned: after the delta rels land, a target with no surviving
    // UnresolvedCall/UnresolvedUse edge is unreferenced by ANY unit.
    report.unresolved_gc = count(
        conn,
        "MATCH (u:UnresolvedTarget) WHERE NOT (u)<-[:UnresolvedCall]-() \
         AND NOT (u)<-[:UnresolvedUse]-() RETURN count(*)",
    )? as u64;
    conn.query(
        "MATCH (u:UnresolvedTarget) WHERE NOT (u)<-[:UnresolvedCall]-() \
         AND NOT (u)<-[:UnresolvedUse]-() DETACH DELETE u",
    )?;

    // --- 9. Refresh the single Scan row (SCAN_HEAD) ------------------------
    //
    // A full load rewrites the Scan node from the scan_meta record; the splice
    // must DELETE the seeded row and INSERT the new one, or the previous scan's
    // git state survives and the spliced DB disagrees with a full rebuild and
    // with graph.jsonl line 1.
    conn.query("MATCH (s:Scan) DELETE s")?;
    conn.query(&format!(
        "CREATE (s:Scan {{fqn: {}, git_sha: {}, git_clean: {}, content_key: {}, scanned_at: {}}})",
        lit(SCAN_HEAD),
        lit(delta.scan.git_sha.as_deref().unwrap_or("")),
        lit(&delta
            .scan
            .git_clean
            .map(|c| c.to_string())
            .unwrap_or_default()),
        lit(delta.scan.content_key.as_deref().unwrap_or("")),
        lit(&delta.scan.scanned_at),
    ))?;
    report.scan_refreshed = true;

    Ok(report)
}

/// The outgoing rel sets a code node authors, per its label. Each type is
/// declared `FROM` that label in the schema (`create_schema`), so the Cypher
/// never names an undeclared pair.
const AUTH_RELS_FUNCTION: &str = "Calls|Uses|UnresolvedCall|UnresolvedUse";
const AUTH_RELS_STRUCT: &str = "Uses|UnresolvedUse|Contains";
const AUTH_RELS_CONTAINS: &str = "Contains";

/// True when `node`'s location path is one of the delete-scope target paths.
fn location_in_targets(node: &crate::graph::Node, targets: &BTreeSet<String>) -> bool {
    node.location
        .as_ref()
        .is_some_and(|l| targets.contains(&l.path.to_string_lossy().into_owned()))
}

/// Single-quotes a value for a Cypher string literal.
fn lit(value: &str) -> String {
    format!("'{}'", cypher_escape(value))
}

/// A Cypher list literal of single-quoted strings.
fn literal_list(values: &BTreeSet<String>) -> String {
    values.iter().map(|v| lit(v)).collect::<Vec<_>>().join(", ")
}

/// Every seeded Struct/Function located in one of `targets`, plus every seeded
/// File whose FQN is a target path, with its label.
fn code_fqns_in_paths(
    conn: &Connection,
    targets: &BTreeSet<String>,
) -> anyhow::Result<BTreeMap<String, &'static str>> {
    let mut out = BTreeMap::new();
    if targets.is_empty() {
        return Ok(out);
    }
    let list = literal_list(targets);
    for label in ["Struct", "Function"] {
        let (_, rows) = query_rows(
            conn,
            &format!("MATCH (n:{label}) WHERE n.path IN [{list}] RETURN n.fqn AS fqn"),
        )?;
        for row in rows {
            out.insert(cell(&row, 0), label);
        }
    }
    let (_, rows) = query_rows(
        conn,
        &format!("MATCH (n:File) WHERE n.fqn IN [{list}] RETURN n.fqn AS fqn"),
    )?;
    for row in rows {
        out.insert(cell(&row, 0), "File");
    }
    Ok(out)
}

/// Every **real** seeded Module FQN — `planned` placeholder modules are
/// transient records the splice never owns (the authored/transient seed
/// assumption), so they are excluded from the delete scope.
fn seed_modules(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let (_, rows) = query_rows(
        conn,
        "MATCH (n:Module) WHERE n.status IS NULL OR n.status <> 'planned' RETURN n.fqn AS fqn",
    )?;
    Ok(rows.iter().map(|r| cell(r, 0)).collect())
}

/// The File FQNs transitively contained under each seeded Module — the module's
/// whole file subtree through the `Module -> Module` hierarchy plus its direct
/// `Module -> File` children. The splicer uses it to decide whether the delta's
/// re-emission target set reaches a module's content: a module whose subtree is
/// outside the target set is untouched and must survive the splice verbatim.
fn module_file_subtrees(conn: &Connection) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let (_, rows) = query_rows(
        conn,
        "MATCH (p:Module)-[:Contains]->(c:Module) RETURN p.fqn AS p, c.fqn AS c",
    )?;
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        children
            .entry(cell(&row, 0))
            .or_default()
            .push(cell(&row, 1));
    }
    let (_, rows) = query_rows(
        conn,
        "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn AS m, f.fqn AS f",
    )?;
    let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        direct
            .entry(cell(&row, 0))
            .or_default()
            .insert(cell(&row, 1));
    }

    // Every module that names a child or owns a file directly. A module with no
    // direct file (a pure-intermediate package node) still appears as a parent.
    let mut modules: BTreeSet<String> = children.keys().cloned().collect();
    for kids in children.values() {
        modules.extend(kids.iter().cloned());
    }
    modules.extend(direct.keys().cloned());

    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in &modules {
        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = vec![m.clone()];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(cur) = stack.pop() {
            // `Contains` is acyclic, but guard the walk anyway so a malformed
            // seed can never loop.
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(fs) = direct.get(&cur) {
                files.extend(fs.iter().cloned());
            }
            if let Some(kids) = children.get(&cur) {
                stack.extend(kids.iter().cloned());
            }
        }
        out.insert(m.clone(), files);
    }
    Ok(out)
}

/// The label of a code node at `fqn`, or `None`. Excludes `planned`
/// placeholders on the four Implementation labels.
fn db_code_label(conn: &Connection, fqn: &str) -> anyhow::Result<Option<&'static str>> {
    for label in ["Function", "Struct", "File", "Module"] {
        let n = count(
            conn,
            &format!(
                "MATCH (n:{label} {{fqn: {}}}) WHERE n.status IS NULL OR n.status <> 'planned' RETURN count(*)",
                lit(fqn)
            ),
        )?;
        if n > 0 {
            return Ok(Some(label));
        }
    }
    Ok(None)
}

/// The DB label of an edge endpoint: the assembled graph's node when present,
/// else the seeded DB (a cached/unaffected endpoint).
fn endpoint_label(
    conn: &Connection,
    fqn: &str,
    graph: &Graph,
    cache: &mut HashMap<String, Option<&'static str>>,
) -> anyhow::Result<Option<&'static str>> {
    if let Some(node) = graph.nodes.get(fqn) {
        return Ok(node_label_of(node.kind));
    }
    if let Some(hit) = cache.get(fqn) {
        return Ok(*hit);
    }
    let label = db_code_label(conn, fqn)?;
    cache.insert(fqn.to_string(), label);
    Ok(label)
}

/// The node-table label of a code kind, or `None` for a non-code kind.
fn node_label_of(kind: NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::Module => Some("Module"),
        NodeKind::Struct => Some("Struct"),
        NodeKind::Function => Some("Function"),
        NodeKind::File => Some("File"),
        NodeKind::UnresolvedTarget => Some("UnresolvedTarget"),
        _ => None,
    }
}

/// The scanned **code** rel-table `(table, from, to)` triples the delta
/// re-merges. [`load::rel_table_pairs`] deliberately omits these (see its
/// `AUTHORED_REL_PAIRS` comment): it exists for the write-through merge guard,
/// which never re-merges scanned code edges — those are written by the load
/// path. The splicer is the exception: it re-emits scanned code edges as DML,
/// so it must admit the pairs the schema declares in `CREATE REL TABLE`
/// (`Calls`/`Uses`/`UnresolvedCall`/`UnresolvedUse`). The `Contains` code pairs
/// need no entry here — they already appear in `rel_table_pairs` via
/// `contains_pairs`.
const CODE_REL_PAIRS: [(&str, &str, &str); 6] = [
    ("Calls", "Function", "Function"),
    ("Uses", "Function", "Struct"),
    ("Uses", "Struct", "Struct"),
    ("UnresolvedCall", "Function", "UnresolvedTarget"),
    ("UnresolvedUse", "Function", "UnresolvedTarget"),
    ("UnresolvedUse", "Struct", "UnresolvedTarget"),
];

/// Whether the schema's rel table `table` declares an edge between the two node
/// labels — [`load::rel_table_pairs`] plus the scanned code pairs
/// ([`CODE_REL_PAIRS`]) — so an undeclared pair is skipped rather than fed to a
/// binder exception.
fn pair_allowed(table: &str, from: &str, to: &str) -> bool {
    CODE_REL_PAIRS
        .iter()
        .any(|(t, a, b)| *t == table && *a == from && *b == to)
        || load::rel_table_pairs()
            .iter()
            .any(|(t, a, b)| *t == table && *a == from && *b == to)
}

/// Counts and deletes the rels the `fqns` author from `label`. Returns the
/// number deleted.
fn delete_authored_rels(
    conn: &Connection,
    label: &str,
    rels: &str,
    fqns: &BTreeSet<String>,
) -> anyhow::Result<u64> {
    if fqns.is_empty() {
        return Ok(0);
    }
    let list = literal_list(fqns);
    let n = count(
        conn,
        &format!("MATCH (n:{label})-[r:{rels}]->() WHERE n.fqn IN [{list}] RETURN count(*)"),
    )?;
    conn.query(&format!(
        "MATCH (n:{label})-[r:{rels}]->() WHERE n.fqn IN [{list}] DELETE r"
    ))?;
    Ok(n as u64)
}

/// Upserts one code node in place: `MERGE (n:Label {fqn}) SET props` (the
/// `ArtifactDb::merge_node` pattern), with the property set `build_load_files`
/// writes for that label. Never deletes the node, so incoming edges survive.
fn upsert_node(
    conn: &Connection,
    label: &str,
    fqn: &str,
    node: &crate::graph::Node,
) -> anyhow::Result<()> {
    // A `planned` placeholder must never overwrite a realized module: a
    // plan JSONL keeps its planned records after a realization scan. (The code
    // assembly has no planned nodes; this is the same backstop `merge_records`
    // applies.)
    let status = lit(node.status.as_deref().unwrap_or(""));
    let stmt = match node.kind {
        NodeKind::Module => {
            if node.status.as_deref() == Some("planned") {
                let realized = count(
                    conn,
                    &format!(
                        "MATCH (n:Module {{fqn: {}}}) WHERE n.status IS NULL OR n.status <> 'planned' RETURN count(*)",
                        lit(fqn)
                    ),
                )?;
                if realized > 0 {
                    return Ok(());
                }
            }
            format!(
                "MERGE (n:Module {{fqn: {}}}) SET n.status = {status}",
                lit(fqn)
            )
        }
        NodeKind::Struct | NodeKind::Function => {
            let (path, start, end, sl, el) = span(node);
            format!(
                "MERGE (n:{label} {{fqn: {}}}) SET n.path = {}, n.start = {start}, n.`end` = {end}, \
                 n.start_line = {sl}, n.end_line = {el}, n.code_type = {}, n.status = {status}",
                lit(fqn),
                lit(&path),
                lit(&node.code_type),
            )
        }
        NodeKind::File => {
            let (_, _, _, sl, el) = span(node);
            format!(
                "MERGE (n:File {{fqn: {}}}) SET n.start_line = {sl}, n.end_line = {el}, \
                 n.code_type = {}, n.status = {status}",
                lit(fqn),
                lit(&node.code_type),
            )
        }
        NodeKind::UnresolvedTarget => format!(
            "MERGE (n:UnresolvedTarget {{fqn: {}}}) SET n.category = {}",
            lit(fqn),
            lit(node.category.as_deref().unwrap_or(""))
        ),
        _ => return Ok(()),
    };
    conn.query(&stmt)?;
    Ok(())
}

/// The (path, start, end, start_line, end_line) location columns of a
/// Struct/Function, defaulted to empty/0 when it carries no location.
fn span(node: &crate::graph::Node) -> (String, u32, u32, u32, u32) {
    match node.location.as_ref() {
        Some(l) => (
            l.path.to_string_lossy().into_owned(),
            l.start,
            l.end,
            l.start_line,
            l.end_line,
        ),
        None => (String::new(), 0, 0, 0, 0),
    }
}

/// `DETACH DELETE`s every node of `label` in `fqns`.
fn detach_delete(conn: &Connection, label: &str, fqns: &[String]) -> anyhow::Result<()> {
    if fqns.is_empty() {
        return Ok(());
    }
    let set: BTreeSet<String> = fqns.iter().cloned().collect();
    conn.query(&format!(
        "MATCH (n:{label}) WHERE n.fqn IN [{}] DETACH DELETE n",
        literal_list(&set)
    ))?;
    Ok(())
}

/// Groups a labelled set by its label.
fn group_by_label(set: &BTreeMap<String, &'static str>) -> Vec<(&'static str, Vec<String>)> {
    let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (fqn, label) in set {
        groups.entry(label).or_default().push(fqn.clone());
    }
    groups.into_iter().collect()
}

/// Runs `RETURN count(*)` and returns the number (0 on a malformed result).
fn count(conn: &Connection, query: &str) -> anyhow::Result<i64> {
    let (_, rows) = query_rows(conn, query)?;
    Ok(rows
        .first()
        .and_then(|r| r.first())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Atomic publish of the two artifacts (phase-03 task-3)
// ---------------------------------------------------------------------------
//
// The splice produces TWO coupled artifacts and both must flip together:
// `apg/.trans/db.lbug` (the query index) and `apg/.trans/graph.jsonl` (the
// sole code-identity/freshness/validation source — `git.rs` and
// `artifacts.rs`). A new DB beside a stale export (or vice versa) is a
// corrupt pair, so the publish is ordered to make that impossible on any
// *reported* failure:
//
// 1. **Nothing published yet** — the delta has already been applied to the
//    seeded temp copy (task-2); an earlier failure (seed / delta / export
//    build) leaves the previous `db.lbug` byte-identical.
// 2. **Checkpoint + close the spliced copy**, then fsync it. `CHECKPOINT`
//    flushes the WAL into the main file; dropping the handle closes it so no
//    `.wal`/`.shm` sidecar is left behind. A live handle would also keep the
//    temp inode open, making the rename publish the wrong file.
// 3. **Build the new export from the P2-assembled in-memory [`Graph`]** via
//    the existing tested [`load::write_graph_jsonl`] into a same-directory
//    temp, then fsync it. Never a DB→jsonl projection: `write_graph_jsonl`
//    single-sources the export format.
// 4. **Back up both targets** in the same directory, then swap: rename the
//    temp DB over `db.lbug`, then the temp export over `graph.jsonl`.
// 5. **fsync the containing directory** so both renames are durable, then
//    remove the backups.
//
// **Rollback**: if the `graph.jsonl` rename fails after the `db.lbug` rename
// succeeded, restore `db.lbug` from its backup so BOTH targets return to the
// previous state. If that restore itself fails, leave the backups in place
// and fail loudly with the manual recovery path — never a silently mixed
// pair.

/// The `graph.jsonl` export path under an `apg/` layout root — the same
/// `.trans/` directory as [`db_path`], so the export rename is same-filesystem.
pub fn export_path(apg_root: &Path) -> PathBuf {
    apg_root.join(crate::specs::TRANS).join("graph.jsonl")
}

/// Publishes the splice's two artifacts atomically (phase-03 task-3).
///
/// Consumes the [`SeededDb`] (whose delta must already be applied) and the
/// P2-assembled in-memory `graph`. On success `target_path` holds the spliced
/// DB and `export` holds `graph`'s [`load::write_graph_jsonl`] rendering; on a
/// reported failure both targets are the previous bytes. `export` should be
/// [`export_path`] — a sibling of `target_path` in `.trans/`.
pub fn publish(seeded: SeededDb, graph: &Graph, export: &Path) -> anyhow::Result<()> {
    let SeededDb {
        db,
        temp_path,
        target_path,
    } = seeded;

    // (2) Checkpoint + close + fsync the spliced DB. Only now is the temp DB a
    // self-contained, durable file ready to be renamed into place.
    if let Err(e) = checkpoint_close_and_fsync(db, &temp_path) {
        remove_quietly(&temp_path);
        return Err(e);
    }

    // (3) Build the export from the in-memory graph into a same-directory temp
    // and fsync it. No target is touched on a build failure.
    let export_temp = transient_sibling(export, "graph", "tmp");
    let built = (|| -> anyhow::Result<()> {
        #[cfg(test)]
        fire_publish_hook(PublishStage::BeforeExportWrite)?;
        load::write_graph_jsonl(graph, &export_temp)?;
        fsync_file(&export_temp)?;
        Ok(())
    })();
    if let Err(e) = built {
        remove_quietly(&export_temp);
        remove_quietly(&temp_path);
        return Err(e);
    }

    // (4) Save backups of both current targets, then swap DB first, export
    // second. `db_backup`/`export_backup` are same-directory snapshots.
    let db_backup = match save_backup(&target_path) {
        Ok(b) => b,
        Err(e) => {
            remove_quietly(&export_temp);
            remove_quietly(&temp_path);
            return Err(anyhow::anyhow!(
                "could not back up {} before the publish: {e}",
                target_path.display()
            ));
        }
    };
    let export_backup = match save_backup(export) {
        Ok(b) => b,
        Err(e) => {
            remove_quietly(&export_temp);
            remove_quietly(&temp_path);
            remove_quietly_opt(db_backup.as_deref());
            return Err(anyhow::anyhow!(
                "could not back up {} before the publish: {e}",
                export.display()
            ));
        }
    };

    if let Err(e) = std::fs::rename(&temp_path, &target_path) {
        // The DB rename is all-or-nothing, so nothing has been published;
        // drop every transient and leave both targets at their previous bytes.
        remove_quietly(&export_temp);
        remove_quietly(&temp_path);
        remove_quietly_opt(db_backup.as_deref());
        remove_quietly_opt(export_backup.as_deref());
        return Err(anyhow::anyhow!(
            "could not rename the spliced db over {}: {e}",
            target_path.display()
        ));
    }

    // The DB is now new. Swap the export; on failure roll the DB back so the
    // pair is never new-DB + stale-export.
    let swapped = (|| -> anyhow::Result<()> {
        #[cfg(test)]
        fire_publish_hook(PublishStage::BeforeExportRename)?;
        std::fs::rename(&export_temp, export)?;
        Ok(())
    })();
    if let Err(e) = swapped {
        return rollback_export_swap(
            db_backup.as_deref(),
            &target_path,
            export_backup.as_deref(),
            &export_temp,
            e,
        );
    }

    // (5) Make both renames durable, then remove the backups.
    for dir in publish_dirs(&target_path, export) {
        fsync_dir(&dir)?;
    }
    remove_quietly_opt(db_backup.as_deref());
    remove_quietly_opt(export_backup.as_deref());
    Ok(())
}

/// Flushes the spliced DB's WAL into its main file and CLOSES it: run
/// `CHECKPOINT` on a fresh connection, drop the connection and the
/// [`Database`], prove no `.wal`/`.shm` sidecar survives, then fsync the
/// closed file.
fn checkpoint_close_and_fsync(db: Database, temp_path: &Path) -> anyhow::Result<()> {
    {
        let conn = Connection::new(&db)?;
        conn.query("CHECKPOINT")
            .map_err(|e| anyhow::anyhow!("CHECKPOINT of the spliced db failed: {e}"))?;
    }
    // A live handle pins the temp inode; the rename must publish the file this
    // handle wrote, so close before publishing.
    drop(db);

    for suffix in [".wal", ".shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", temp_path.display()));
        if sidecar.exists() {
            anyhow::bail!(
                "spliced db {} still has a {suffix} sidecar after CHECKPOINT+close — refusing to publish an unflushed database",
                temp_path.display()
            );
        }
    }
    fsync_file(temp_path)?;
    Ok(())
}

/// Saves `target` as a same-directory backup, returning its path (`None` when
/// `target` does not exist). A hard link is preferred: the closed previous
/// artifact is immutable, so the link is a true O(1) snapshot; a byte copy is
/// the fallback when the filesystem refuses a link.
fn save_backup(target: &Path) -> std::io::Result<Option<PathBuf>> {
    if !target.exists() {
        return Ok(None);
    }
    let backup = transient_sibling(target, "prev", "bak");
    let _ = std::fs::remove_file(&backup);
    match std::fs::hard_link(target, &backup) {
        Ok(()) => Ok(Some(backup)),
        Err(_) => {
            std::fs::copy(target, &backup)?;
            Ok(Some(backup))
        }
    }
}

/// The export rename failed after the DB was already swapped: restore the DB
/// from its backup so BOTH targets return to the previous state. If the
/// restore itself fails, leave the backups in place and fail loudly with the
/// manual recovery path (never a new DB paired with a stale export).
fn rollback_export_swap(
    db_backup: Option<&Path>,
    target_path: &Path,
    export_backup: Option<&Path>,
    export_temp: &Path,
    cause: anyhow::Error,
) -> anyhow::Result<()> {
    remove_quietly(export_temp);
    let Some(backup) = db_backup else {
        // The seed requires a previous DB, so there is normally always a
        // backup; defensive only.
        remove_quietly_opt(export_backup);
        return Err(cause);
    };
    match std::fs::rename(backup, target_path) {
        Ok(()) => {
            // The export target was never swapped (its rename is
            // all-or-nothing), so the previous export is still in place and
            // `export_backup` is a redundant copy.
            remove_quietly_opt(export_backup);
            for dir in publish_dirs(target_path, target_path) {
                let _ = fsync_dir(&dir);
            }
            Err(cause.context(
                "the graph.jsonl swap failed after the db.lbug swap; db.lbug was rolled back to its previous bytes",
            ))
        }
        Err(restore_err) => anyhow::bail!(
            "publish failed ({cause:#}) and rolling {} back from its backup failed ({restore_err}); \
             the previous db.lbug is preserved at {} and the previous graph.jsonl at {} — recover with \
             `mv '{}' '{}'` and re-run the scan",
            target_path.display(),
            backup.display(),
            export_backup
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string()),
            backup.display(),
            target_path.display(),
        ),
    }
}

/// A unique same-directory sibling of `target` for a transient `tag`/`ext`
/// file (publish temps, backups). Kept in `target`'s parent so every rename
/// this module performs is an atomic same-filesystem rename.
fn transient_sibling(target: &Path, tag: &str, ext: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_string());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(
        ".{file}.{tag}-{}-{nanos}.{ext}",
        std::process::id()
    ))
}

/// fsyncs `path` (opened for write for portable `sync_all`), forcing its bytes
/// to stable storage before a rename publishes it.
fn fsync_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()
}

/// fsyncs a directory entry, making a rename within it durable.
fn fsync_dir(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// The distinct parent directories of the two published targets (one, in the
/// normal `.trans/` layout).
fn publish_dirs(a: &Path, b: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for p in [a, b] {
        let d = p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

/// Best-effort removal of a transient file; a missing file is fine.
fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// [`remove_quietly`] over an optional path.
fn remove_quietly_opt(path: Option<&Path>) {
    if let Some(p) = path {
        remove_quietly(p);
    }
}

// A test-only one-shot injection fired at a chosen publish stage, so the
// rollback/early-failure paths can be exercised deterministically (the
// `fire_projection_hook` pattern in `artifacts.rs`).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PublishStage {
    /// Before the new export temp is written — models an export-build failure.
    BeforeExportWrite,
    /// After the DB swap, before the export swap — models the partial-failure
    /// rollback trigger.
    BeforeExportRename,
}

#[cfg(test)]
type PublishHook = Box<dyn FnOnce() -> anyhow::Result<()>>;

#[cfg(test)]
thread_local! {
    static PUBLISH_HOOK: std::cell::RefCell<Option<(PublishStage, PublishHook)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn install_publish_hook(
    stage: PublishStage,
    hook: impl FnOnce() -> anyhow::Result<()> + 'static,
) {
    PUBLISH_HOOK.with(|c| *c.borrow_mut() = Some((stage, Box::new(hook))));
}

#[cfg(test)]
fn fire_publish_hook(stage: PublishStage) -> anyhow::Result<()> {
    PUBLISH_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.as_ref().is_some_and(|(s, _)| *s == stage) {
            match slot.take() {
                Some((_, hook)) => hook(),
                None => Ok(()),
            }
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};

    /// A scratch directory unique to `name` (tests run in parallel threads of
    /// one process, so the test name disambiguates).
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("apg-splice-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal graph exercising several node tables and every code rel table,
    /// so a seeded copy has non-trivial rows to preserve.
    fn fixture_graph() -> Graph {
        let mut g = Graph::default();
        let node = |kind: NodeKind, loc: Option<Location>, cat: Option<&str>| Node {
            kind,
            location: loc,
            category: cat.map(str::to_string),
            code_type: "src".to_string(),
            ..Node::default()
        };
        g.nodes
            .insert("mod".to_string(), node(NodeKind::Module, None, None));
        g.nodes.insert(
            "/x/a.go".to_string(),
            node(
                NodeKind::File,
                Some(Location {
                    path: "/x/a.go".into(),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 80,
                }),
                None,
            ),
        );
        g.nodes.insert(
            "mod.A".to_string(),
            node(
                NodeKind::Struct,
                Some(Location {
                    path: "/x/a.go".into(),
                    start: 0,
                    end: 50,
                    start_line: 1,
                    end_line: 50,
                }),
                None,
            ),
        );
        g.nodes.insert(
            "mod.A.f".to_string(),
            node(
                NodeKind::Function,
                Some(Location {
                    path: "/x/a.go".into(),
                    start: 1,
                    end: 49,
                    start_line: 2,
                    end_line: 49,
                }),
                None,
            ),
        );
        g.nodes.insert(
            "ext.Foo".to_string(),
            node(NodeKind::UnresolvedTarget, None, Some("external")),
        );
        g.contains
            .insert(("mod".to_string(), "/x/a.go".to_string()));
        g.contains
            .insert(("/x/a.go".to_string(), "mod.A".to_string()));
        g.contains
            .insert(("/x/a.go".to_string(), "mod.A.f".to_string()));
        g.contains
            .insert(("mod.A".to_string(), "mod.A.f".to_string()));
        g.calls
            .insert(("mod.A.f".to_string(), "mod.A.f".to_string()));
        g.uses.insert(("mod.A.f".to_string(), "mod.A".to_string()));
        g.unresolved_calls
            .insert(("mod.A.f".to_string(), "ext.Foo".to_string(), String::new()));
        g.unresolved_uses
            .insert(("mod.A.f".to_string(), "ext.Foo".to_string()));
        g
    }

    /// Builds a real on-disk DB at `path` through the same
    /// `create_schema + copy_from` full load the scan path uses, so the seed
    /// under test sees a genuine previous database.
    fn build_db(path: &Path, graph: &Graph) {
        let ldir = path.parent().unwrap().join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        load::build_load_files(graph, &ldir).unwrap();
        let db = Database::new(path, SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        load::create_schema(&conn).unwrap();
        load::copy_from(&conn, &ldir).unwrap();
        drop(conn);
        drop(db);
    }

    /// Per-table row counts of an open DB — the data half of the "preserved"
    /// proof (the schema half is [`extract_schema`]).
    fn row_counts(db: &Database) -> BTreeMap<String, i64> {
        let conn = Connection::new(db).unwrap();
        let (names, rows) = query_rows(&conn, "CALL show_tables() RETURN name, type").unwrap();
        let name_i = column_index(&names, "name").unwrap();
        let type_i = column_index(&names, "type").unwrap();
        let mut out = BTreeMap::new();
        for row in rows {
            let table = cell(&row, name_i);
            let kind = cell(&row, type_i);
            let q = if kind == "REL" {
                format!("MATCH ()-[r:{table}]->() RETURN count(*)")
            } else {
                format!("MATCH (n:{table}) RETURN count(*)")
            };
            let (_, counts) = query_rows(&conn, &q).unwrap();
            let n = counts
                .first()
                .and_then(|r| r.first())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(-1);
            out.insert(table, n);
        }
        out
    }

    /// The `.seed-…tmp` siblings of `target`, used to prove no temp is leaked on
    /// a fallback.
    fn seed_temps(target: &Path) -> Vec<PathBuf> {
        let dir = target.parent().unwrap();
        let prefix = format!(".{}.seed-", target.file_name().unwrap().to_string_lossy());
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
            })
            .collect()
    }

    /// Every table's row count in a DB file opened read-only — the equivalence
    /// oracle's per-label NODE counts AND per-rel-type COUNTS in one map
    /// (`row_counts` walks every table `show_tables()` reports, nodes and RELs
    /// alike).
    fn table_counts(path: &Path) -> BTreeMap<String, i64> {
        let db = Database::new(path, SystemConfig::default().read_only(true)).unwrap();
        let counts = row_counts(&db);
        drop(db);
        counts
    }

    /// The `UnresolvedTarget` rows as `(fqn, category)` — the oracle's unresolved
    /// set by FQN WITH its category (folded per-category counts come from this).
    fn unresolved_rows(path: &Path) -> BTreeSet<(String, String)> {
        let db = Database::new(path, SystemConfig::default().read_only(true)).unwrap();
        let conn = Connection::new(&db).unwrap();
        let (names, rows) = query_rows(
            &conn,
            "MATCH (n:UnresolvedTarget) RETURN n.fqn AS fqn, n.category AS category",
        )
        .unwrap();
        let f = column_index(&names, "fqn").unwrap();
        let c = column_index(&names, "category").unwrap();
        let out = rows.iter().map(|r| (cell(r, f), cell(r, c))).collect();
        drop(conn);
        drop(db);
        out
    }

    /// The per-category counts of an unresolved `(fqn, category)` set.
    fn category_counts(rows: &BTreeSet<(String, String)>) -> BTreeMap<String, usize> {
        let mut out: BTreeMap<String, usize> = BTreeMap::new();
        for (_, category) in rows {
            *out.entry(category.clone()).or_default() += 1;
        }
        out
    }

    /// The single `Scan` row of a DB file opened read-only, as
    /// `(git_sha, git_clean, content_key, scanned_at)`.
    fn scan_row_of(path: &Path) -> (String, String, String, String) {
        let db = Database::new(path, SystemConfig::default().read_only(true)).unwrap();
        let conn = Connection::new(&db).unwrap();
        let (_, rows) = query_rows(
            &conn,
            "MATCH (s:Scan) RETURN s.git_sha AS sha, s.git_clean AS clean, \
             s.content_key AS key, s.scanned_at AS at",
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "exactly one Scan row");
        let out = (
            cell(&rows[0], 0),
            cell(&rows[0], 1),
            cell(&rows[0], 2),
            cell(&rows[0], 3),
        );
        drop(conn);
        drop(db);
        out
    }

    /// e2e tier -- real I/O: every test here builds/copies real `db.lbug` files,
    /// writes `graph.jsonl` and node files under the temp dir. Each is
    /// `#[ignore]`d, so a plain `cargo test` never runs one; the only entry
    /// point is the named guard `cargo test-e2e`
    /// (= `cargo test tests::e2e:: -- --ignored`).
    mod e2e {
        use super::*;

        /// (a) A seed copy preserves every table / every row of the previous DB.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn seed_copy_preserves_every_table_and_row() {
            let dir = scratch("preserve");
            let prev = dir.join("db.lbug");
            build_db(&prev, &fixture_graph());

            let before_schema = {
                let db = Database::new(&prev, SystemConfig::default().read_only(true)).unwrap();
                let conn = Connection::new(&db).unwrap();
                extract_schema(&conn).unwrap()
            };
            let before_counts = {
                let db = Database::new(&prev, SystemConfig::default().read_only(true)).unwrap();
                row_counts(&db)
            };

            let seeded = match seed(&prev) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => {
                    panic!(
                        "expected a seed from a valid previous DB, got: {}",
                        f.describe()
                    )
                }
            };

            // Same-directory temp sibling (task-3's rename stays on one filesystem),
            // distinct from the target, and the previous file is untouched.
            assert_eq!(seeded.temp_path.parent(), prev.parent());
            assert_ne!(seeded.temp_path, prev);
            assert!(seeded.temp_path.exists());
            assert!(prev.exists());

            // The whole file is preserved — byte-for-byte.
            assert_eq!(
                std::fs::read(&prev).unwrap(),
                std::fs::read(&seeded.temp_path).unwrap(),
                "the seed must be a whole-file copy"
            );

            // The opened copy answers the same schema and the same row counts.
            let conn = seeded.conn().unwrap();
            let after_schema = extract_schema(&conn).unwrap();
            assert_eq!(before_schema, after_schema, "schema preserved by the seed");
            drop(conn);
            let after_counts = row_counts(&seeded.db);
            assert_eq!(
                before_counts, after_counts,
                "every table/row preserved by the seed"
            );
            assert!(
                after_counts.get("UnresolvedTarget") == Some(&1),
                "the fixture's unresolved target row must survive: {after_counts:?}"
            );
            assert!(
                after_counts.get("Function") == Some(&1),
                "the fixture's function row must survive: {after_counts:?}"
            );

            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// (b) A missing previous DB invalidates the seed (fallback), and no temp is
        /// created.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn missing_previous_db_falls_back_to_full_load() {
            let dir = scratch("missing");
            let prev = dir.join("db.lbug");
            assert!(!prev.exists());

            match seed(&prev) {
                SeedDecision::FullLoad(SeedFallback::MissingPrevious) => {}
                SeedDecision::FullLoad(other) => {
                    panic!("expected MissingPrevious, got: {}", other.describe())
                }
                SeedDecision::Seed(_) => panic!("a missing previous DB must never seed"),
            }
            assert!(seed_temps(&prev).is_empty(), "no temp on the missing path");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// (c) An incompatible schema invalidates the seed (fallback): the previous
        /// DB opens but its tables differ from this binary's `create_schema`.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn incompatible_schema_falls_back_to_full_load() {
            let dir = scratch("schema");
            let prev = dir.join("db.lbug");
            {
                let db = Database::new(&prev, SystemConfig::default()).unwrap();
                let conn = Connection::new(&db).unwrap();
                // Deliberately wrong: one table, none of the expected ones.
                conn.query("CREATE NODE TABLE Widget(fqn STRING PRIMARY KEY, extra INT64)")
                    .unwrap();
                drop(conn);
                drop(db);
            }

            match seed(&prev) {
                SeedDecision::FullLoad(SeedFallback::IncompatibleSchema(_)) => {}
                SeedDecision::FullLoad(other) => {
                    panic!("expected IncompatibleSchema, got: {}", other.describe())
                }
                SeedDecision::Seed(_) => panic!("a schema mismatch must never seed"),
            }
            assert!(seed_temps(&prev).is_empty(), "no temp on the schema path");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// (c) A storage-format / version mismatch (a file that is not a database)
        /// invalidates the seed (fallback), and no temp is left behind.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn unreadable_previous_db_falls_back_to_full_load() {
            let dir = scratch("format");
            let prev = dir.join("db.lbug");
            std::fs::write(&prev, b"this is definitely not a db").unwrap();

            match seed(&prev) {
                SeedDecision::FullLoad(SeedFallback::Unreadable(_)) => {}
                SeedDecision::FullLoad(other) => {
                    panic!("expected Unreadable, got: {}", other.describe())
                }
                SeedDecision::Seed(_) => panic!("a non-database file must never seed"),
            }
            assert!(seed_temps(&prev).is_empty(), "no temp on the format path");
            let _ = std::fs::remove_dir_all(&dir);
        }

        // -----------------------------------------------------------------------
        // Delta application (phase-03 task-2)
        // -----------------------------------------------------------------------

        /// The equivalence proof (`domain.constraint.db-splice-equivalence`): a
        /// spliced DB's code node/edge/UnresolvedTarget/Scan sets equal a full
        /// rebuild's, while covering persist / disappear / unresolved-GC / Scan
        /// refresh in one delta.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn delta_application_matches_a_full_rebuild() {
            let dir = scratch("delta");
            let prev_path = dir.join("db.lbug");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();

            let previous = previous_graph(&a, &b, &c);
            build_db(&prev_path, &previous);

            let seeded = match seed(&prev_path) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
            };

            let assembled = assembled_graph(&a, &b);
            // The delete scope for a body-only change to a.go plus the removal of
            // c.go: the changed file and the removed file. b.go is cut off (its
            // caller edge must survive the splice untouched).
            let targets: BTreeSet<String> = [a.clone(), c.clone()].into_iter().collect();
            let removed: BTreeSet<String> = BTreeSet::new();
            let delta = SpliceDelta {
                graph: &assembled,
                targets: &targets,
                removed_fqns: &removed,
                scan: ScanRow {
                    git_sha: Some("newsha".into()),
                    git_clean: Some(true),
                    content_key: Some("newkey".into()),
                    scanned_at: "2026-01-02T00:00:00Z".into(),
                },
            };

            let report = seeded.apply(&delta).unwrap();
            assert!(
                report.nodes_upserted >= 6,
                "persist+new units upserted: {report:?}"
            );
            assert_eq!(
                report.nodes_deleted, 3,
                "c.go, m.C.q, m.C disappear: {report:?}"
            );
            assert!(
                report.edges_deleted >= 8,
                "authored rels replaced: {report:?}"
            );
            assert!(report.edges_merged >= 8, "delta rels re-merged: {report:?}");
            assert_eq!(
                report.unresolved_gc, 1,
                "ext.Old is now unreferenced: {report:?}"
            );
            assert!(report.scan_refreshed);

            // The full-rebuild reference: the same assembled graph loaded whole.
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &assembled);

            let spliced = code_snapshot(&seeded.db);
            let expected = {
                let db =
                    Database::new(&expected_path, SystemConfig::default().read_only(true)).unwrap();
                let snap = code_snapshot(&db);
                drop(db);
                snap
            };
            assert_eq!(
                spliced, expected,
                "a spliced DB must answer identically to a full rebuild"
            );

            // Spot-check the three cases that motivate the scheme.
            assert!(
                spliced.contains("Calls:m.B.g->m.A.f"),
                "a caller OUTSIDE the delta must keep its edge to a persisting FQN"
            );
            assert!(
                !spliced.contains("Function:m.C.q:") && !spliced.contains("Module:m.C:"),
                "the removed file's unit and its orphaned module must be gone"
            );
            assert!(
                spliced.contains("UnresolvedTarget:ext.New:stdlib")
                    && !spliced
                        .iter()
                        .any(|s| s.starts_with("UnresolvedTarget:ext.Old")),
                "the shared UnresolvedTarget rows must be insert-then-GC'd"
            );
            assert!(
                spliced.contains("Scan:scan/HEAD|newsha|true|newkey|2026-01-02T00:00:00Z"),
                "the seeded Scan row must be refreshed, not preserved"
            );

            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// feedback-101: the splice seeds the LOCAL `db.lbug` while the
        /// manifest/delta live in the SHARED store (`<git-common-dir>/apg/facts`).
        /// When another worktree scans in between, the shared `scan.json`/manifest
        /// advances past this worktree's DB; the empty-target case then upserts no
        /// code unit and the stale seeded rows survive — the published DB is NOT a
        /// full rebuild, and the next freshness fast-path reuses it. The
        /// equivalence-guarded seed must refuse such a DB and hand the caller to the
        /// full load.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn stale_local_seed_is_refused_when_another_worktree_advanced_the_store() {
            use crate::cache::{CacheKey, Manifest, ScanConfigKey};
            use crate::delta::ScanRecord;

            let dir = scratch("stale-seed");
            let prev_path = dir.join("db.lbug");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();

            // This worktree's LOCAL DB: built by its last scan at content "oldkey".
            build_db(&prev_path, &previous_graph(&a, &b, &c));

            // The ASSEMBLED graph for this worktree's CURRENT tree, and its full
            // rebuild (the same graph loaded whole) — the correctness reference.
            let assembled = assembled_graph(&a, &b);
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &assembled);
            let expected = published_snapshot(&expected_path);

            // Another worktree (B) scanned in between. B's tree content equals this
            // worktree's current tree, so the SHARED scan record carries the current
            // key ("newkey") and the shared-manifest diff against the current tree
            // is EMPTY — the feedback's empty-target case.
            let store = dir.join("facts");
            std::fs::create_dir_all(&store).unwrap();
            let key = CacheKey::compute(&ScanConfigKey::default());
            ScanRecord {
                sha: "newsha".into(),
                cache_key: key,
                manifest: Manifest::default(),
                content_key: Some("newkey".into()),
            }
            .save(&store)
            .unwrap();
            let recorded = ScanRecord::load(&store).unwrap();

            // The unguarded splice — the pre-fix behaviour — applies the
            // empty-target delta: no code unit is upserted, so the stale seeded
            // rows survive and the export is the current full tree. The DB therefore
            // DIVERGES from a full rebuild: exactly the bug this guard prevents.
            {
                let seeded = match seed(&prev_path) {
                    SeedDecision::Seed(s) => s,
                    SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
                };
                let targets: BTreeSet<String> = BTreeSet::new();
                let removed: BTreeSet<String> = BTreeSet::new();
                seeded
                    .apply(&SpliceDelta {
                        graph: &assembled,
                        targets: &targets,
                        removed_fqns: &removed,
                        scan: ScanRow {
                            git_sha: Some("newsha".into()),
                            git_clean: Some(true),
                            content_key: Some("newkey".into()),
                            scanned_at: "2026-01-02T00:00:00Z".into(),
                        },
                    })
                    .unwrap();
                assert_ne!(
                    code_snapshot(&seeded.db),
                    expected,
                    "an empty-target splice of a stale seed must diverge from a full rebuild"
                );
                let temp = seeded.temp_path.clone();
                drop(seeded);
                std::fs::remove_file(&temp).ok();
            }

            // The fix: the shared recorded key ("newkey") does not match the local
            // seed's own key ("oldkey"), so the splice is REFUSED — the caller runs
            // the full load, which is the correctness reference.
            match seed_checked(&prev_path, recorded.content_key.as_deref()) {
                SeedDecision::FullLoad(SeedFallback::StaleSeed { seed, recorded }) => {
                    assert_eq!(seed, "oldkey");
                    assert_eq!(recorded, "newkey");
                }
                SeedDecision::FullLoad(other) => {
                    panic!("expected StaleSeed, got: {}", other.describe())
                }
                SeedDecision::Seed(_) => panic!("a stale local seed must never seed"),
            }

            // The common single-worktree case still seeds: the shared recorded key
            // IS the local DB's own key.
            match seed_checked(&prev_path, Some("oldkey")) {
                SeedDecision::Seed(s) => s.discard().unwrap(),
                SeedDecision::FullLoad(f) => {
                    panic!("a current local seed must still splice: {}", f.describe())
                }
            }

            // A missing key on either side is ineligible — equivalence cannot be
            // verified.
            assert!(matches!(
                seed_checked(&prev_path, None),
                SeedDecision::FullLoad(SeedFallback::StaleSeed { .. })
            ));

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The win-C spawn skip must not delete a skipped language's global module
        /// scaffolding (feedback-100). The assembled graph is MISSING the skipped
        /// language's pure-intermediate modules and every `Module -> Module`
        /// hierarchy edge — exactly what a partial scan that skips that language
        /// produces — yet the spliced DB must equal a full rebuild of the TRUE tree
        /// (same node set incl. modules, per-rel-type Contains counts, the
        /// UnresolvedTarget set, and the Scan row). Before the fix the splice
        /// treated every seeded module as in scope and `DETACH DELETE`d the
        /// scaffolding the assembled graph could not vouch for.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn module_scaffolding_survives_a_partial_scan_that_skips_a_language() {
            let dir = scratch("skipped-lang");
            let prev_path = dir.join("db.lbug");
            let changed = "/x/go/changed.go".to_string();
            let skipped = "/x/csharp/Tests.cs".to_string();

            build_db(&prev_path, &multi_lang_previous(&changed, &skipped));
            let seeded = match seed(&prev_path) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
            };

            // The win-B assembled graph: the changed language re-emitted, the
            // skipped language's cached file facts only (no global scaffolding).
            let assembled = multi_lang_assembled(&changed, &skipped);
            // The delete scope is exactly the changed language's file.
            let targets: BTreeSet<String> = [changed.clone()].into_iter().collect();
            let removed: BTreeSet<String> = BTreeSet::new();
            let report = seeded
                .apply(&SpliceDelta {
                    graph: &assembled,
                    targets: &targets,
                    removed_fqns: &removed,
                    scan: ScanRow {
                        git_sha: Some("newsha".into()),
                        git_clean: Some(true),
                        content_key: Some("newkey".into()),
                        scanned_at: "2026-01-02T00:00:00Z".into(),
                    },
                })
                .unwrap();

            // The skipped language is untouched, so nothing disappears.
            assert_eq!(
                report.nodes_deleted, 0,
                "an untouched skipped language has no disappearing units: {report:?}"
            );
            let spliced = code_snapshot(&seeded.db);
            for row in [
                "Module:Apg:",
                "Module:Apg.CsharpFrontend:",
                "Module:Apg.CsharpFrontend.Tests:",
                "Contains:Apg->Apg.CsharpFrontend",
                "Contains:Apg.CsharpFrontend->Apg.CsharpFrontend.Tests",
            ] {
                assert!(
                    spliced.contains(row),
                    "the skipped language's scaffolding must survive: {row}\n{spliced:?}"
                );
            }

            // The full-rebuild reference: the TRUE new tree, loaded whole — NOT the
            // same assembled graph (which would make the oracle miss the bug).
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &multi_lang_new(&changed, &skipped));
            let expected = {
                let db =
                    Database::new(&expected_path, SystemConfig::default().read_only(true)).unwrap();
                let snap = code_snapshot(&db);
                drop(db);
                snap
            };
            assert_eq!(
                spliced, expected,
                "a spliced DB must answer identically to a full rebuild"
            );

            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The EXPORT half of the partial-skip oracle (feedback-102): a partial
        /// scan that spawns one language and skips another must render a
        /// `graph.jsonl` byte-equal to a full rebuild's. The win-B assembly is the
        /// export source, so the skipped language's global scaffolding —
        /// pure-intermediate modules and every `Module -> Module` edge, which no
        /// per-file fact unit carries — must be replayed from the store into that
        /// assembly. Before the fix the assembled graph structurally could not carry
        /// it and the export differed even though the spliced DB matched.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn partial_scan_export_matches_a_full_rebuild() {
            use crate::cache::{
                CacheKey, FactStore, FileFragment, ModuleScaffolding, ScanConfigKey,
            };
            use crate::ingest::{IngestOptions, Reuse, ingest_with_reuse};
            use crate::schema::Record;

            let dir = scratch("export-equiv");
            let changed = "/x/go/changed.go".to_string();
            let skipped = "/x/csharp/Tests.cs".to_string();
            let cache_key = CacheKey::compute(&ScanConfigKey::default());

            // The last FULL scan's graph: both languages' complete scaffolding.
            let previous = multi_lang_previous(&changed, &skipped);
            let mut store = FactStore::at(dir.join("facts"));
            let frag = FileFragment::from_graph(
                &previous,
                &skipped,
                "csharp/Tests.cs",
                "oid-skipped",
                "csharp",
            );
            store.put(&frag, "/x", &cache_key).unwrap();
            let scaffolding = ModuleScaffolding::extract(&previous, Path::new("/x"));
            store.put_scaffolding_all(&scaffolding, &cache_key).unwrap();

            // The win-B assembly: only the changed language re-emits facts; the
            // skipped file comes from the cache and its scaffolding from the store.
            let reuse = Reuse {
                store: &store,
                cache_key: &cache_key,
                files: vec![(
                    "csharp/Tests.cs".to_string(),
                    "csharp".to_string(),
                    "oid-skipped".to_string(),
                )],
                reader_root: "/x".to_string(),
                skipped_langs: ["csharp".to_string()].into_iter().collect(),
            };
            let records = vec![
                Record::ScanMeta {
                    git_sha: Some("newsha".into()),
                    git_clean: Some(true),
                    content_key: Some("newkey".into()),
                    scanned_at: "2026-01-02T00:00:00Z".into(),
                },
                Record::LangSwitch {
                    language: "go".into(),
                },
                Record::Module {
                    fqn: "godemo".into(),
                },
                Record::Module {
                    fqn: "godemo/changed".into(),
                },
                Record::File {
                    path: changed.clone(),
                    parent: "godemo/changed".into(),
                    start_line: 1,
                    end_line: 30,
                },
                Record::Struct {
                    id: "s1".into(),
                    parent: "godemo.changed".into(),
                    name: "S".into(),
                    path: changed.clone(),
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 30,
                },
                Record::Function {
                    id: "f1".into(),
                    parent: "godemo.changed.S".into(),
                    name: "f".into(),
                    params: Vec::new(),
                    file: changed.clone(),
                    path: changed.clone(),
                    start: 0,
                    end: 1,
                    start_line: 2,
                    end_line: 10,
                },
                Record::Function {
                    id: "f2".into(),
                    parent: "godemo.changed.S".into(),
                    name: "h".into(),
                    params: Vec::new(),
                    file: changed.clone(),
                    path: changed.clone(),
                    start: 0,
                    end: 1,
                    start_line: 12,
                    end_line: 20,
                },
                Record::Contains {
                    from: "godemo".into(),
                    to: "godemo/changed".into(),
                },
                Record::Contains {
                    from: "s1".into(),
                    to: "f1".into(),
                },
                Record::Contains {
                    from: "s1".into(),
                    to: "f2".into(),
                },
            ];
            let (assembled, _) = ingest_with_reuse(
                records,
                &IngestOptions {
                    blacklist: &[],
                    language: "go",
                    config: None,
                },
                Some(&reuse),
            );

            // The TRUE new tree, loaded whole — the full-rebuild reference.
            let reference = multi_lang_new(&changed, &skipped);
            let assembled_path = dir.join("assembled.jsonl");
            let reference_path = dir.join("reference.jsonl");
            load::write_graph_jsonl(&assembled, &assembled_path).unwrap();
            load::write_graph_jsonl(&reference, &reference_path).unwrap();
            let assembled_jsonl = std::fs::read_to_string(&assembled_path).unwrap();
            let reference_jsonl = std::fs::read_to_string(&reference_path).unwrap();
            // `Graph`'s node/edge maps are unordered (HashMap/HashSet), so line
            // order is not part of the export's contract; equality is the SET of
            // records. The `scan_meta` control record still leads line 1.
            let canonical =
                |text: &str| -> BTreeSet<String> { text.lines().map(str::to_string).collect() };
            assert_eq!(
                canonical(&assembled_jsonl),
                canonical(&reference_jsonl),
                "a partial scan that skips a language must export a full rebuild's graph.jsonl"
            );
            assert!(assembled_jsonl.starts_with("{\"type\":\"scan_meta\""));
            assert!(reference_jsonl.starts_with("{\"type\":\"scan_meta\""));
            // The skipped language's scaffolding is present in the export itself.
            for needle in [
                "\"type\":\"module\",\"fqn\":\"Apg\"",
                "\"type\":\"module\",\"fqn\":\"Apg.CsharpFrontend\"",
                "\"type\":\"contains\",\"from\":\"Apg\",\"to\":\"Apg.CsharpFrontend\"",
            ] {
                assert!(
                    assembled_jsonl.contains(needle),
                    "the export must carry the skipped language's scaffolding: {needle}"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// feedback-103: a **spawned** language's global `Module -> Module`
        /// scaffolding must reach the EXPORT even when its per-file facts are
        /// filtered to a target set. `partial_scan_export_matches_a_full_rebuild`
        /// covers the SKIPPED language (its scaffolding is replayed from the store);
        /// this covers the SPAWNED one, where `Reuse.skipped_langs` is EMPTY so pass
        /// 2b replays nothing — the scaffolding can only come from the scanned
        /// stream. The condition is Java's: a multi-package tree (`pkg`, `pkg.a`
        /// unchanged, `pkg.b` changed, `pkg.c` unchanged) where the targeted scan
        /// re-emits only `pkg.b`'s per-file facts, yet the walk covers every package.
        /// The fixed scanner emits the global package hierarchy for every walked
        /// package; before the fix it emitted only the target package's, so the
        /// assembled export lacked `pkg -> pkg.a` while Java was not in
        /// `skipped_langs`, and the export diverged from a full rebuild.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn java_targeted_scan_scaffolding_reaches_the_export() {
            use crate::cache::{CacheKey, FactStore, FileFragment, ScanConfigKey};
            use crate::ingest::{IngestOptions, Reuse, ingest_with_reuse};
            use crate::schema::Record;

            let dir = scratch("java-targeted-export");
            let a = "/x/java/pkg/a/A.java".to_string();
            let b = "/x/java/pkg/b/B.java".to_string();
            let c = "/x/java/pkg/c/C.java".to_string();
            let cache_key = CacheKey::compute(&ScanConfigKey::default());

            // The TRUE new tree: the full-rebuild reference AND the source of the
            // unchanged files' cached per-file units.
            let reference = java_pkg_tree(&a, &b, &c);
            let mut store = FactStore::at(dir.join("facts"));
            for (abs, rel, oid) in [
                (a.as_str(), "java/pkg/a/A.java", "oid-a"),
                (c.as_str(), "java/pkg/c/C.java", "oid-c"),
            ] {
                let frag = FileFragment::from_graph(&reference, abs, rel, oid, "java");
                store.put(&frag, "/x", &cache_key).unwrap();
            }
            // Deliberately NO stored scaffolding: pass 2b only replays for a SKIPPED
            // language, and `skipped_langs` below is empty.

            // The stream the FIXED Java frontend emits for a targeted scan of
            // `pkg/b/B.java` ONLY: the global package hierarchy for every walked
            // package, then the target file's per-file facts.
            let scaffolding = || {
                vec![
                    Record::Module { fqn: "pkg".into() },
                    Record::Module {
                        fqn: "pkg.a".into(),
                    },
                    Record::Module {
                        fqn: "pkg.b".into(),
                    },
                    Record::Module {
                        fqn: "pkg.c".into(),
                    },
                    Record::Contains {
                        from: "pkg".into(),
                        to: "pkg.a".into(),
                    },
                    Record::Contains {
                        from: "pkg".into(),
                        to: "pkg.b".into(),
                    },
                    Record::Contains {
                        from: "pkg".into(),
                        to: "pkg.c".into(),
                    },
                ]
            };
            let target_facts = || {
                vec![
                    Record::LangSwitch {
                        language: "java".into(),
                    },
                    Record::File {
                        path: b.clone(),
                        parent: "pkg.b".into(),
                        start_line: 1,
                        end_line: 20,
                    },
                    Record::Struct {
                        id: "sb".into(),
                        parent: "pkg.b".into(),
                        name: "B".into(),
                        path: b.clone(),
                        start: 0,
                        end: 1,
                        start_line: 1,
                        end_line: 20,
                    },
                    Record::Function {
                        id: "fb".into(),
                        parent: "pkg.b.B".into(),
                        name: "f".into(),
                        params: Vec::new(),
                        file: b.clone(),
                        path: b.clone(),
                        start: 0,
                        end: 1,
                        start_line: 2,
                        end_line: 10,
                    },
                    Record::Contains {
                        from: "sb".into(),
                        to: "fb".into(),
                    },
                ]
            };
            let scan_meta = || Record::ScanMeta {
                git_sha: Some("newsha".into()),
                git_clean: Some(true),
                content_key: Some("newkey".into()),
                scanned_at: "2026-01-02T00:00:00Z".into(),
            };

            // The spawned-language reuse: the unchanged Java FILES are spliced from
            // the cache, but Java is NOT in `skipped_langs` (it was spawned), so the
            // store's scaffolding is never replayed.
            let reuse = Reuse {
                store: &store,
                cache_key: &cache_key,
                files: vec![
                    (
                        "java/pkg/a/A.java".to_string(),
                        "java".to_string(),
                        "oid-a".to_string(),
                    ),
                    (
                        "java/pkg/c/C.java".to_string(),
                        "java".to_string(),
                        "oid-c".to_string(),
                    ),
                ],
                reader_root: "/x".to_string(),
                skipped_langs: BTreeSet::new(),
            };
            assert!(
                reuse.skipped_langs.is_empty(),
                "Java is spawned, not skipped: pass 2b must replay nothing"
            );

            let opts = IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
            };
            let mut fixed: Vec<Record> = vec![scan_meta()];
            fixed.extend(scaffolding());
            fixed.extend(target_facts());
            let (assembled, _) = ingest_with_reuse(fixed, &opts, Some(&reuse));

            let assembled_path = dir.join("assembled.jsonl");
            let reference_path = dir.join("reference.jsonl");
            load::write_graph_jsonl(&assembled, &assembled_path).unwrap();
            load::write_graph_jsonl(&reference, &reference_path).unwrap();
            let assembled_jsonl = std::fs::read_to_string(&assembled_path).unwrap();
            let reference_jsonl = std::fs::read_to_string(&reference_path).unwrap();
            let canonical =
                |text: &str| -> BTreeSet<String> { text.lines().map(str::to_string).collect() };

            // The scaffolding is in the export itself...
            assert!(
                assembled_jsonl.contains("\"type\":\"module\",\"fqn\":\"pkg.a\""),
                "the export must carry the unchanged package's module record"
            );
            assert!(
                assembled_jsonl.contains("\"type\":\"contains\",\"from\":\"pkg\",\"to\":\"pkg.a\""),
                "the export must carry the spawned stream's pkg -> pkg.a scaffolding:\n{assembled_jsonl}"
            );
            // ...and the whole export equals a full rebuild's.
            assert_eq!(
                canonical(&assembled_jsonl),
                canonical(&reference_jsonl),
                "a spawned targeted Java scan must export a full rebuild's graph.jsonl"
            );
            assert!(assembled_jsonl.starts_with("{\"type\":\"scan_meta\""));

            // Sensitivity / non-blindness: re-assemble the stream the PRE-FIX
            // scanner emitted — the target package's hierarchy only. `skipped_langs`
            // is STILL empty (Java is spawned), so nothing can recover the missing
            // hierarchy: the export lacks the `pkg -> pkg.a` edge even though `pkg.a`
            // itself survives as the reused file's cached direct-parent module, and
            // it diverges from the full rebuild. The hierarchy EDGE — not the module
            // record — is the discriminating assertion above.
            let mut pre_fix: Vec<Record> = vec![scan_meta()];
            pre_fix.push(Record::Module { fqn: "pkg".into() });
            pre_fix.push(Record::Module {
                fqn: "pkg.b".into(),
            });
            pre_fix.push(Record::Contains {
                from: "pkg".into(),
                to: "pkg.b".into(),
            });
            pre_fix.extend(target_facts());
            let (before_fix, _) = ingest_with_reuse(pre_fix, &opts, Some(&reuse));
            let before_path = dir.join("before-fix.jsonl");
            load::write_graph_jsonl(&before_fix, &before_path).unwrap();
            let before_jsonl = std::fs::read_to_string(&before_path).unwrap();
            assert!(
                before_jsonl.contains("\"type\":\"module\",\"fqn\":\"pkg.a\""),
                "the reused file's cached direct-parent module survives even pre-fix"
            );
            assert!(
                !before_jsonl.contains("\"type\":\"contains\",\"from\":\"pkg\",\"to\":\"pkg.a\""),
                "the pre-fix stream cannot carry pkg -> pkg.a:\n{before_jsonl}"
            );
            assert_ne!(
                canonical(&before_jsonl),
                canonical(&reference_jsonl),
                "the pre-fix stream's export must diverge from a full rebuild"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A removed file named only by `removed_fqns` (not by a target path) is
        /// still detached — the subtraction half of the full-universe seam.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn removed_fqns_are_detached_even_outside_the_target_paths() {
            let dir = scratch("removed-fqns");
            let prev_path = dir.join("db.lbug");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));

            let seeded = match seed(&prev_path) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
            };
            let assembled = assembled_graph(&a, &b);
            let targets: BTreeSet<String> = [a.clone()].into_iter().collect();
            let removed: BTreeSet<String> = ["m.C.q".to_string()].into_iter().collect();
            seeded
                .apply(&SpliceDelta {
                    graph: &assembled,
                    targets: &targets,
                    removed_fqns: &removed,
                    scan: ScanRow {
                        git_sha: None,
                        git_clean: None,
                        content_key: None,
                        scanned_at: "2026-01-02T00:00:00Z".into(),
                    },
                })
                .unwrap();

            let snap = code_snapshot(&seeded.db);
            assert!(
                !snap.contains("Function:m.C.q:"),
                "an FQN named by removed_fqns must be detached: {snap:?}"
            );
            // An empty git state writes empty strings, exactly as the full load does.
            assert!(
                snap.contains("Scan:scan/HEAD||||2026-01-02T00:00:00Z"),
                "a non-git scan's Scan row is all-empty but scanned_at: {snap:?}"
            );
            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }

        // -----------------------------------------------------------------------
        // Atomic publish + rollback (phase-03 task-3)
        // -----------------------------------------------------------------------

        /// The happy path: both artifacts flip, the export is `write_graph_jsonl`'s
        /// rendering of the in-memory graph, the DB answers the spliced snapshot,
        /// and no temp/backup/WAL debris is left behind.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn publish_swaps_both_artifacts_and_leaves_no_debris() {
            let dir = scratch("publish-ok");
            let prev_path = dir.join("db.lbug");
            let export = dir.join("graph.jsonl");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));
            std::fs::write(&export, b"previous export\n").unwrap();

            let (seeded, assembled) = seed_and_splice(&prev_path, &a, &b, &c);
            publish(seeded, &assembled, &export).unwrap();

            // The export is exactly the existing writer's rendering of the graph —
            // written from memory, never projected back out of the DB.
            let reference = dir.join("reference.jsonl");
            load::write_graph_jsonl(&assembled, &reference).unwrap();
            assert_eq!(
                std::fs::read(&export).unwrap(),
                std::fs::read(&reference).unwrap(),
                "the published export must be `write_graph_jsonl`'s output"
            );

            let snap = published_snapshot(&prev_path);
            assert!(
                snap.contains("Function:m.A.h:"),
                "the added unit is present: {snap:?}"
            );
            assert!(
                snap.contains("Function:m.A.f:"),
                "the changed unit persists: {snap:?}"
            );
            assert!(
                !snap.contains("Function:m.C.q:"),
                "the removed unit is gone: {snap:?}"
            );

            assert!(
                transient_entries(&dir).is_empty(),
                "no temp/backup debris: {:?}",
                transient_entries(&dir)
            );
            assert!(
                !dir.join("db.lbug.wal").exists() && !dir.join("db.lbug.shm").exists(),
                "the closed DB must have no WAL/shm sidecar"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A failure after the DB swap (the export rename) restores `db.lbug` from
        /// its backup, so both targets return to the previous bytes and no debris
        /// remains.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn export_rename_failure_rolls_back_and_restores_previous_bytes() {
            let dir = scratch("publish-rollback");
            let prev_path = dir.join("db.lbug");
            let export = dir.join("graph.jsonl");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));
            std::fs::write(&export, b"previous export\n").unwrap();
            let prev_db = std::fs::read(&prev_path).unwrap();
            let prev_export = std::fs::read(&export).unwrap();

            let (seeded, assembled) = seed_and_splice(&prev_path, &a, &b, &c);
            install_publish_hook(PublishStage::BeforeExportRename, || {
                Err(anyhow::anyhow!("injected export rename failure"))
            });
            let err = publish(seeded, &assembled, &export).unwrap_err();
            assert!(
                format!("{err:#}").contains("injected export rename failure"),
                "the original cause must surface: {err:#}"
            );

            assert_eq!(
                std::fs::read(&prev_path).unwrap(),
                prev_db,
                "db.lbug must be restored byte-identical to the previous database"
            );
            assert_eq!(
                std::fs::read(&export).unwrap(),
                prev_export,
                "graph.jsonl must still be the previous export"
            );
            assert!(
                transient_entries(&dir).is_empty(),
                "rollback must not leak temps/backups: {:?}",
                transient_entries(&dir)
            );

            // The restored DB is genuinely the previous graph, not the spliced one.
            let snap = published_snapshot(&prev_path);
            assert!(
                !snap.contains("Function:m.A.h:"),
                "the rollback must restore the previous graph: {snap:?}"
            );
            assert!(
                snap.contains("Function:m.C.q:"),
                "the removed unit must be back: {snap:?}"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A failure before the DB swap (the export build) leaves both targets
        /// byte-identical — the previous DB is never replaced.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn export_build_failure_leaves_the_previous_artifacts_byte_identical() {
            let dir = scratch("publish-early");
            let prev_path = dir.join("db.lbug");
            let export = dir.join("graph.jsonl");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));
            std::fs::write(&export, b"previous export\n").unwrap();
            let prev_db = std::fs::read(&prev_path).unwrap();
            let prev_export = std::fs::read(&export).unwrap();

            let (seeded, assembled) = seed_and_splice(&prev_path, &a, &b, &c);
            install_publish_hook(PublishStage::BeforeExportWrite, || {
                Err(anyhow::anyhow!("injected export build failure"))
            });
            let err = publish(seeded, &assembled, &export).unwrap_err();
            assert!(
                format!("{err:#}").contains("injected export build failure"),
                "the original cause must surface: {err:#}"
            );

            assert_eq!(
                std::fs::read(&prev_path).unwrap(),
                prev_db,
                "an earlier failure leaves db.lbug byte-identical"
            );
            assert_eq!(
                std::fs::read(&export).unwrap(),
                prev_export,
                "an earlier failure leaves graph.jsonl byte-identical"
            );
            assert!(
                transient_entries(&dir).is_empty(),
                "an earlier failure must clean up its temps: {:?}",
                transient_entries(&dir)
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        // -----------------------------------------------------------------------
        // Enumerated equivalence oracle + export leg (phase-03 task-8)
        // -----------------------------------------------------------------------

        /// `domain.constraint.db-splice-equivalence`, enumerated: a spliced DB
        /// agrees with a full rebuild of the same assembled graph on EVERY table's
        /// row count — the per-label NODE counts AND the per-rel-type COUNTS
        /// (Contains/Calls/Uses/UnresolvedCall/UnresolvedUse listed explicitly) —
        /// on the UnresolvedTarget set by FQN WITH categories (hence the
        /// per-category counts), and on the single `Scan` row.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn spliced_and_full_rebuild_agree_on_every_table_count() {
            let dir = scratch("counts");
            let prev_path = dir.join("db.lbug");
            let export = dir.join("graph.jsonl");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));
            std::fs::write(&export, b"previous export\n").unwrap();

            let (seeded, assembled) = seed_and_splice(&prev_path, &a, &b, &c);
            publish(seeded, &assembled, &export).unwrap();

            // The full-rebuild reference: the same assembled graph, loaded whole.
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &assembled);

            let spliced_counts = table_counts(&prev_path);
            let expected_counts = table_counts(&expected_path);
            assert_eq!(
                spliced_counts, expected_counts,
                "every table's row count must equal a full rebuild"
            );
            // Prove the enumeration is COMPLETE: every code label and every code
            // rel table the oracle compares is actually in the map (a missing
            // table would otherwise make the equality above vacuous).
            for label in ["Module", "File", "Struct", "Function", "UnresolvedTarget"] {
                assert!(
                    spliced_counts.contains_key(label),
                    "the oracle must enumerate the {label} node table: {spliced_counts:?}"
                );
            }
            for table in [
                "Contains",
                "Calls",
                "Uses",
                "UnresolvedCall",
                "UnresolvedUse",
            ] {
                assert!(
                    spliced_counts.contains_key(table),
                    "the oracle must enumerate the {table} rel table: {spliced_counts:?}"
                );
                assert_eq!(
                    spliced_counts.get(table),
                    expected_counts.get(table),
                    "{table} row count must equal a full rebuild"
                );
            }

            // UnresolvedTarget by FQN WITH category, and the folded per-category
            // counts.
            let spliced_unres = unresolved_rows(&prev_path);
            let expected_unres = unresolved_rows(&expected_path);
            assert_eq!(
                spliced_unres, expected_unres,
                "the unresolved set by (fqn, category) must equal a full rebuild"
            );
            assert_eq!(
                category_counts(&spliced_unres),
                category_counts(&expected_unres),
                "the per-category unresolved counts must equal a full rebuild"
            );

            // And the Scan row.
            assert_eq!(
                scan_row_of(&prev_path),
                scan_row_of(&expected_path),
                "the Scan row must equal a full rebuild's"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// feedback-90 — the incoming-edge invariant. A body-only change to a
        /// widely-referenced unit whose CALLERS ARE NOT re-emitted must keep every
        /// incoming Calls/Uses edge from those callers (the persisting FQN is
        /// UPSERTed, never DETACH DELETEd); a removed FQN disappears and leaves no
        /// dangling edge; and the per-rel-type counts still equal a full rebuild,
        /// so nothing a rebuild keeps was lost.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn persisting_fqn_is_upserted_and_incoming_edges_survive() {
            let dir = scratch("incoming");
            let prev_path = dir.join("db.lbug");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));

            let seeded = match seed(&prev_path) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
            };
            // `assembled_graph(a, b)` is the body-only change to `a.go` (plus a new
            // `m.A.h`); `b.go`'s caller `m.B.g` is a cached, NON-re-emitted unit and
            // `c.go` is removed. The delete scope is exactly the changed + removed
            // files.
            let assembled = assembled_graph(&a, &b);
            let targets: BTreeSet<String> = [a.clone(), c.clone()].into_iter().collect();
            let removed: BTreeSet<String> = BTreeSet::new();
            let report = seeded
                .apply(&SpliceDelta {
                    graph: &assembled,
                    targets: &targets,
                    removed_fqns: &removed,
                    scan: ScanRow {
                        git_sha: Some("newsha".into()),
                        git_clean: Some(true),
                        content_key: Some("newkey".into()),
                        scanned_at: "2026-01-02T00:00:00Z".into(),
                    },
                })
                .unwrap();

            let conn = seeded.conn().unwrap();
            let (names, rows) = query_rows(
                &conn,
                "MATCH (x:Function)-[:Calls]->(y:Function) RETURN x.fqn AS x, y.fqn AS y",
            )
            .unwrap();
            let xi = column_index(&names, "x").unwrap();
            let yi = column_index(&names, "y").unwrap();
            let calls: BTreeSet<String> = rows
                .iter()
                .map(|r| format!("{}->{}", cell(r, xi), cell(r, yi)))
                .collect();
            let (names, rows) = query_rows(
                &conn,
                "MATCH (x:Function)-[:Uses]->(y:Struct) RETURN x.fqn AS x, y.fqn AS y",
            )
            .unwrap();
            let xi = column_index(&names, "x").unwrap();
            let yi = column_index(&names, "y").unwrap();
            let uses: BTreeSet<String> = rows
                .iter()
                .map(|r| format!("{}->{}", cell(r, xi), cell(r, yi)))
                .collect();
            drop(conn);

            assert!(
                calls.contains("m.B.g->m.A.f"),
                "a caller OUTSIDE the delete scope must keep its incoming edge to the \
                 upserted FQN: {calls:?}"
            );
            assert!(
                uses.contains("m.A.f->m.A"),
                "the re-emitted unit's own Uses edge must survive: {uses:?}"
            );
            // The persisting `m.A.f` was UPSERTed in place, never detached: exactly
            // one row remains.
            let snap = code_snapshot(&seeded.db);
            assert_eq!(
                snap.iter()
                    .filter(|s| s.starts_with("Function:m.A.f:"))
                    .count(),
                1,
                "the persisting FQN must be a single upserted row: {snap:?}"
            );
            // The removed FQN disappeared: no node row and no rel naming it (the
            // graph stays a closure — no dangling edge).
            assert!(
                !snap.iter().any(|s| s.contains("m.C")),
                "the removed unit/module and every rel naming them must be gone: {snap:?}"
            );
            assert_eq!(
                report.nodes_deleted, 3,
                "exactly the removed file's nodes are deleted: {report:?}"
            );

            // Per-rel-type counts (and every other table) still equal a full
            // rebuild — the surviving incoming edge is part of that equality.
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &assembled);
            assert_eq!(
                table_counts(&seeded.temp_path),
                table_counts(&expected_path),
                "counts must equal a full rebuild after the upsert+delete"
            );

            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The UnresolvedTarget SHARED lifecycle: a target still referenced by an
        /// unchanged, NON-re-emitted unit survives a re-emitted unit's edge
        /// deletion; a target whose only referrer changed (and dropped the
        /// reference) is GC'd; and a delta-first-referenced target is inserted with
        /// its category exactly once (dedup by FQN). The per-category counts equal a
        /// full rebuild.
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn unresolved_target_lifecycle_survives_gc_and_dedup() {
            let dir = scratch("unresolved-life");
            let prev_path = dir.join("db.lbug");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();

            let module = || Node {
                kind: NodeKind::Module,
                ..Node::default()
            };
            let target = |cat: &str| Node {
                kind: NodeKind::UnresolvedTarget,
                category: Some(cat.to_string()),
                ..Node::default()
            };

            // prev: a.go's `m.A.f` references `ext.Gone`; b.go's `m.B.g`
            // references `ext.Keep`.
            let mut prev = Graph::default();
            prev.nodes.insert("m".into(), module());
            prev.nodes
                .insert(a.clone(), located(NodeKind::File, &a, 1, 80));
            prev.nodes
                .insert(b.clone(), located(NodeKind::File, &b, 1, 40));
            prev.nodes
                .insert("m.A.f".into(), located(NodeKind::Function, &a, 2, 20));
            prev.nodes
                .insert("m.B.g".into(), located(NodeKind::Function, &b, 2, 30));
            prev.nodes.insert("ext.Gone".into(), target("external"));
            prev.nodes.insert("ext.Keep".into(), target("stdlib"));
            prev.contains.insert(("m".into(), a.clone()));
            prev.contains.insert(("m".into(), b.clone()));
            prev.contains.insert((a.clone(), "m.A.f".into()));
            prev.contains.insert((b.clone(), "m.B.g".into()));
            prev.unresolved_calls
                .insert(("m.A.f".into(), "ext.Gone".into(), String::new()));
            prev.unresolved_calls
                .insert(("m.B.g".into(), "ext.Keep".into(), String::new()));
            prev.nodes.insert(
                crate::schema::SCAN_HEAD.into(),
                scan_node("oldsha", "oldkey", "2026-01-01T00:00:00Z"),
            );
            build_db(&prev_path, &prev);

            // assembled: only a.go is re-emitted (targets = {a}); `m.A.f` now
            // references the delta-first `ext.Brand`; b.go is a cached unit, so
            // `m.B.g` keeps `ext.Keep`.
            let mut new = Graph::default();
            new.nodes.insert("m".into(), module());
            new.nodes
                .insert(a.clone(), located(NodeKind::File, &a, 1, 90));
            new.nodes
                .insert(b.clone(), located(NodeKind::File, &b, 1, 40));
            new.nodes
                .insert("m.A.f".into(), located(NodeKind::Function, &a, 2, 22));
            new.nodes
                .insert("m.B.g".into(), located(NodeKind::Function, &b, 2, 30));
            new.nodes.insert("ext.Brand".into(), target("stdlib"));
            new.nodes.insert("ext.Keep".into(), target("stdlib"));
            new.contains.insert(("m".into(), a.clone()));
            new.contains.insert(("m".into(), b.clone()));
            new.contains.insert((a.clone(), "m.A.f".into()));
            new.contains.insert((b.clone(), "m.B.g".into()));
            new.unresolved_calls
                .insert(("m.A.f".into(), "ext.Brand".into(), String::new()));
            new.unresolved_calls
                .insert(("m.B.g".into(), "ext.Keep".into(), String::new()));
            new.nodes.insert(
                crate::schema::SCAN_HEAD.into(),
                scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
            );

            let seeded = match seed(&prev_path) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
            };
            let targets: BTreeSet<String> = [a.clone()].into_iter().collect();
            let removed: BTreeSet<String> = BTreeSet::new();
            let report = seeded
                .apply(&SpliceDelta {
                    graph: &new,
                    targets: &targets,
                    removed_fqns: &removed,
                    scan: ScanRow {
                        git_sha: Some("newsha".into()),
                        git_clean: Some(true),
                        content_key: Some("newkey".into()),
                        scanned_at: "2026-01-02T00:00:00Z".into(),
                    },
                })
                .unwrap();

            let rows = unresolved_rows(&seeded.temp_path);
            // (1) The target of the non-re-emitted unit SURVIVES.
            assert!(
                rows.contains(&("ext.Keep".to_string(), "stdlib".to_string())),
                "a target of an unchanged, non-re-emitted unit must survive: {rows:?}"
            );
            // (2) The target whose only referrer was re-emitted and dropped the
            // reference is GC'd.
            assert!(
                !rows.iter().any(|(f, _)| f == "ext.Gone"),
                "a target whose only referrer changed must be GC'd: {rows:?}"
            );
            assert_eq!(
                report.unresolved_gc, 1,
                "exactly one target GC'd: {report:?}"
            );
            // (3) The delta-first target is inserted exactly ONCE, carrying its
            // category (dedup by FQN).
            let brand: Vec<&(String, String)> =
                rows.iter().filter(|(f, _)| f == "ext.Brand").collect();
            assert_eq!(
                brand.len(),
                1,
                "a delta-first-referenced target must be exactly one row: {rows:?}"
            );
            assert_eq!(
                brand[0].1, "stdlib",
                "the inserted UnresolvedTarget row keeps its category"
            );

            // Per-category counts equal a full rebuild.
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &new);
            assert_eq!(
                category_counts(&rows),
                category_counts(&unresolved_rows(&expected_path)),
                "per-category unresolved counts must equal a full rebuild"
            );

            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The EXPORT leg: the published `graph.jsonl` equals a full rebuild's
        /// (both are the unchanged writer's rendering of the same assembled graph),
        /// ROUND-TRIPS through the re-ingest reader (JSONL → Graph → JSONL), and its
        /// line 1 is the `scan_meta` control record whose fields equal the spliced
        /// DB's `Scan` row (feedback-91).
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn published_export_equals_a_full_rebuild_and_round_trips() {
            let dir = scratch("export-roundtrip");
            let prev_path = dir.join("db.lbug");
            let export = dir.join("graph.jsonl");
            let a = "/x/a.go".to_string();
            let b = "/x/b.go".to_string();
            let c = "/x/c.go".to_string();
            build_db(&prev_path, &previous_graph(&a, &b, &c));
            std::fs::write(&export, b"previous export\n").unwrap();

            let (seeded, assembled) = seed_and_splice(&prev_path, &a, &b, &c);
            publish(seeded, &assembled, &export).unwrap();

            let canonical =
                |text: &str| -> BTreeSet<String> { text.lines().map(str::to_string).collect() };
            let published = std::fs::read_to_string(&export).unwrap();

            // The full-rebuild reference export: the same assembled graph, whole.
            let reference = dir.join("reference.jsonl");
            load::write_graph_jsonl(&assembled, &reference).unwrap();
            assert_eq!(
                canonical(&published),
                canonical(&std::fs::read_to_string(&reference).unwrap()),
                "the spliced export must equal a full rebuild's graph.jsonl"
            );

            // Round-trip through the re-ingest leg: JSONL → Graph → JSONL.
            let back = crate::load::tests::read_graph_jsonl(&export).unwrap();
            let again = dir.join("again.jsonl");
            load::write_graph_jsonl(&back, &again).unwrap();
            assert_eq!(
                canonical(&published),
                canonical(&std::fs::read_to_string(&again).unwrap()),
                "graph.jsonl must round-trip through read_graph_jsonl"
            );

            // Line 1 is the scan_meta control record; its fields equal the spliced
            // DB's Scan row (both come from the delta's ScanRow).
            let first = published.lines().next().unwrap();
            let v: serde_json::Value = serde_json::from_str(first).unwrap();
            assert_eq!(
                v["type"], "scan_meta",
                "line 1 must lead with scan_meta: {first}"
            );
            assert_eq!(v["git_sha"], "newsha");
            assert_eq!(v["git_clean"], true);
            assert_eq!(v["content_key"], "newkey");
            assert_eq!(v["scanned_at"], "2026-01-02T00:00:00Z");
            let db = Database::new(&prev_path, SystemConfig::default().read_only(true)).unwrap();
            let snap = code_snapshot(&db);
            drop(db);
            assert!(
                snap.contains("Scan:scan/HEAD|newsha|true|newkey|2026-01-02T00:00:00Z"),
                "the spliced DB's Scan row must match the export's line 1: {snap:?}"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// feedback-118 (phase-03 task-2): an ADD is the mirror of a removal. A
        /// scan that adds a NEW FILE to an existing package AND a NEW PACKAGE (a
        /// Module absent from the seed) must land exactly what a full rebuild
        /// lands. The previous revision gated the module re-decide on the SEED
        /// subtree (`reached`), so a module absent from the seed was never
        /// inserted and the `Contains` edges a SEEDED module authors to the added
        /// children (`Module -> File`, `Module -> Module`) were silently skipped —
        /// the spliced DB then missed a `Module` row and `Contains` rels a full
        /// rebuild keeps (`domain.constraint.db-splice-equivalence`).
        #[test]
        #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
        fn added_file_and_new_package_land_like_a_full_rebuild() {
            let dir = scratch("add-file-module");
            let prev_path = dir.join("db.lbug");
            let a = "/x/a.go".to_string();
            let d = "/x/d.go".to_string();
            let e = "/x/n/e.go".to_string();

            let module = || Node {
                kind: NodeKind::Module,
                ..Node::default()
            };

            // prev: module `m` owning `a.go` (struct `m.A`, func `m.A.f`).
            let mut prev = Graph::default();
            prev.nodes.insert("m".into(), module());
            prev.nodes
                .insert(a.clone(), located(NodeKind::File, &a, 1, 20));
            prev.nodes
                .insert("m.A".into(), located(NodeKind::Struct, &a, 1, 20));
            prev.nodes
                .insert("m.A.f".into(), located(NodeKind::Function, &a, 2, 10));
            prev.contains.insert(("m".into(), a.clone()));
            prev.contains.insert((a.clone(), "m.A".into()));
            prev.contains.insert((a.clone(), "m.A.f".into()));
            prev.contains.insert(("m.A".into(), "m.A.f".into()));
            prev.nodes.insert(
                crate::schema::SCAN_HEAD.into(),
                scan_node("oldsha", "oldkey", "2026-01-01T00:00:00Z"),
            );
            build_db(&prev_path, &prev);

            // assembled: the WHOLE new tree — `m` gains `d.go`, plus a NEW package
            // `m.N` holding `n/e.go`.
            let mut new = Graph::default();
            new.nodes.insert("m".into(), module());
            new.nodes.insert("m.N".into(), module());
            new.nodes
                .insert(a.clone(), located(NodeKind::File, &a, 1, 20));
            new.nodes
                .insert(d.clone(), located(NodeKind::File, &d, 1, 20));
            new.nodes
                .insert(e.clone(), located(NodeKind::File, &e, 1, 20));
            new.nodes
                .insert("m.A".into(), located(NodeKind::Struct, &a, 1, 20));
            new.nodes
                .insert("m.A.f".into(), located(NodeKind::Function, &a, 2, 10));
            new.nodes
                .insert("m.D".into(), located(NodeKind::Struct, &d, 1, 20));
            new.nodes
                .insert("m.D.d".into(), located(NodeKind::Function, &d, 2, 10));
            new.nodes
                .insert("m.N.E".into(), located(NodeKind::Struct, &e, 1, 20));
            new.nodes
                .insert("m.N.E.e".into(), located(NodeKind::Function, &e, 2, 10));
            new.contains.insert(("m".into(), a.clone()));
            new.contains.insert(("m".into(), d.clone()));
            new.contains.insert(("m".into(), "m.N".into()));
            new.contains.insert(("m.N".into(), e.clone()));
            new.contains.insert((a.clone(), "m.A".into()));
            new.contains.insert((a.clone(), "m.A.f".into()));
            new.contains.insert(("m.A".into(), "m.A.f".into()));
            new.contains.insert((d.clone(), "m.D".into()));
            new.contains.insert((d.clone(), "m.D.d".into()));
            new.contains.insert(("m.D".into(), "m.D.d".into()));
            new.contains.insert((e.clone(), "m.N.E".into()));
            new.contains.insert((e.clone(), "m.N.E.e".into()));
            new.contains.insert(("m.N.E".into(), "m.N.E.e".into()));
            new.nodes.insert(
                crate::schema::SCAN_HEAD.into(),
                scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
            );

            let seeded = match seed(&prev_path) {
                SeedDecision::Seed(s) => s,
                SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
            };
            // The delta's re-emission target set: exactly the TWO added files.
            let targets: BTreeSet<String> = [d.clone(), e.clone()].into_iter().collect();
            let removed: BTreeSet<String> = BTreeSet::new();
            seeded
                .apply(&SpliceDelta {
                    graph: &new,
                    targets: &targets,
                    removed_fqns: &removed,
                    scan: ScanRow {
                        git_sha: Some("newsha".into()),
                        git_clean: Some(true),
                        content_key: Some("newkey".into()),
                        scanned_at: "2026-01-02T00:00:00Z".into(),
                    },
                })
                .unwrap();

            // The full-rebuild reference: the same assembled graph, loaded whole.
            let expected_path = dir.join("expected.lbug");
            build_db(&expected_path, &new);

            // Per-table counts: per-label NODE counts AND per-rel-type COUNTS,
            // with the Module node table and the Contains rel table named.
            let spliced_counts = table_counts(&seeded.temp_path);
            let expected_counts = table_counts(&expected_path);
            assert_eq!(
                spliced_counts, expected_counts,
                "every table's count must equal a full rebuild"
            );
            assert_eq!(
                spliced_counts.get("Module"),
                expected_counts.get("Module"),
                "the Module node count must equal a full rebuild's"
            );
            assert_eq!(
                spliced_counts.get("Module"),
                Some(&2),
                "both packages (the seeded `m` and the NEW `m.N`) must be present"
            );
            assert_eq!(
                spliced_counts.get("Contains"),
                expected_counts.get("Contains"),
                "the Contains rel count must equal a full rebuild's"
            );

            // The full structural set oracle (the Module set included).
            let spliced = code_snapshot(&seeded.db);
            let expected = {
                let db =
                    Database::new(&expected_path, SystemConfig::default().read_only(true)).unwrap();
                let snap = code_snapshot(&db);
                drop(db);
                snap
            };
            assert_eq!(
                spliced, expected,
                "a spliced DB must answer a full rebuild's node/edge/Scan sets"
            );
            for needle in [
                "Module:m.N:",
                "Contains:m->/x/d.go",
                "Contains:m->m.N",
                "Contains:m.N->/x/n/e.go",
            ] {
                assert!(
                    spliced.contains(needle),
                    "the added file/package must land: `{needle}` missing from {spliced:?}"
                );
            }

            let temp = seeded.temp_path.clone();
            drop(seeded);
            std::fs::remove_file(&temp).ok();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A code node with a location under `path`.
    fn located(kind: NodeKind, path: &str, sl: u32, el: u32) -> Node {
        Node {
            kind,
            location: Some(Location {
                path: PathBuf::from(path),
                start: 0,
                end: 1,
                start_line: sl,
                end_line: el,
            }),
            code_type: "src".to_string(),
            ..Node::default()
        }
    }

    /// A `Scan` node with `(sha, clean, key, at)`.
    fn scan_node(sha: &str, key: &str, at: &str) -> Node {
        Node {
            kind: NodeKind::Scan,
            git_sha: Some(sha.to_string()),
            git_clean: Some(true),
            content_key: Some(key.to_string()),
            scanned_at: Some(at.to_string()),
            ..Node::default()
        }
    }

    /// The previous tree: module `m` with `a.go` (struct `m.A`, fun `m.A.f`) and
    /// `b.go` (fun `m.B.g` → `m.A.f`), plus module `m.C` with `c.go` (fun
    /// `m.C.q`). `m.A.f` references `ext.Old`.
    fn previous_graph(a: &str, b: &str, c: &str) -> Graph {
        let mut g = Graph::default();
        g.nodes.insert(
            "m".into(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "m.C".into(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(a.into(), located(NodeKind::File, a, 1, 80));
        g.nodes.insert(b.into(), located(NodeKind::File, b, 1, 40));
        g.nodes.insert(c.into(), located(NodeKind::File, c, 1, 20));
        g.nodes
            .insert("m.A".into(), located(NodeKind::Struct, a, 1, 50));
        g.nodes
            .insert("m.A.f".into(), located(NodeKind::Function, a, 2, 20));
        g.nodes
            .insert("m.B.g".into(), located(NodeKind::Function, b, 2, 30));
        g.nodes
            .insert("m.C.q".into(), located(NodeKind::Function, c, 2, 10));
        g.nodes.insert(
            "ext.Old".into(),
            Node {
                kind: NodeKind::UnresolvedTarget,
                category: Some("external".into()),
                ..Node::default()
            },
        );
        g.contains.insert(("m".into(), a.into()));
        g.contains.insert(("m".into(), b.into()));
        g.contains.insert(("m.C".into(), c.into()));
        g.contains.insert((a.into(), "m.A".into()));
        g.contains.insert((a.into(), "m.A.f".into()));
        g.contains.insert(("m.A".into(), "m.A.f".into()));
        g.contains.insert((b.into(), "m.B.g".into()));
        g.contains.insert((c.into(), "m.C.q".into()));
        g.calls.insert(("m.B.g".into(), "m.A.f".into()));
        g.uses.insert(("m.A.f".into(), "m.A".into()));
        g.unresolved_calls
            .insert(("m.A.f".into(), "ext.Old".into(), String::new()));
        g.nodes.insert(
            crate::schema::SCAN_HEAD.into(),
            scan_node("oldsha", "oldkey", "2026-01-01T00:00:00Z"),
        );
        g
    }

    /// The new tree — the win-B assembled graph: `a.go` re-emitted with a new
    /// body (same FQNs) plus a new function `m.A.h`, now referencing `ext.New`
    /// instead of `ext.Old`; `b.go` is a cached (unchanged) unit; `c.go` (and its
    /// module `m.C`) is gone, so `m.C` is not in the graph. The new `Scan` row is
    /// carried on the graph as the scanner emits it.
    fn assembled_graph(a: &str, b: &str) -> Graph {
        let mut g = Graph::default();
        g.nodes.insert(
            "m".into(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(a.into(), located(NodeKind::File, a, 1, 90));
        g.nodes.insert(b.into(), located(NodeKind::File, b, 1, 40));
        g.nodes
            .insert("m.A".into(), located(NodeKind::Struct, a, 1, 55));
        g.nodes
            .insert("m.A.f".into(), located(NodeKind::Function, a, 2, 22));
        g.nodes
            .insert("m.A.h".into(), located(NodeKind::Function, a, 24, 40));
        g.nodes
            .insert("m.B.g".into(), located(NodeKind::Function, b, 2, 30));
        g.nodes.insert(
            "ext.New".into(),
            Node {
                kind: NodeKind::UnresolvedTarget,
                category: Some("stdlib".into()),
                ..Node::default()
            },
        );
        g.contains.insert(("m".into(), a.into()));
        g.contains.insert(("m".into(), b.into()));
        g.contains.insert((a.into(), "m.A".into()));
        g.contains.insert((a.into(), "m.A.f".into()));
        g.contains.insert((a.into(), "m.A.h".into()));
        g.contains.insert(("m.A".into(), "m.A.f".into()));
        g.contains.insert((b.into(), "m.B.g".into()));
        g.calls.insert(("m.B.g".into(), "m.A.f".into()));
        g.uses.insert(("m.A.f".into(), "m.A".into()));
        g.unresolved_calls
            .insert(("m.A.f".into(), "ext.New".into(), String::new()));
        g.nodes.insert(
            crate::schema::SCAN_HEAD.into(),
            scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
        );
        g
    }

    /// The full structural snapshot the equivalence oracle compares: every code
    /// node (FQN + label + the UnresolvedTarget category), every code rel (table
    /// + endpoints), and the single `Scan` row.
    fn code_snapshot(db: &Database) -> BTreeSet<String> {
        let conn = Connection::new(db).unwrap();
        let mut out = BTreeSet::new();
        for label in ["Module", "Struct", "Function", "File", "UnresolvedTarget"] {
            let cat = if label == "UnresolvedTarget" {
                ", n.category AS category"
            } else {
                ""
            };
            let (_, rows) = query_rows(
                &conn,
                &format!("MATCH (n:{label}) RETURN n.fqn AS fqn{cat}"),
            )
            .unwrap();
            for row in rows {
                out.insert(format!("{label}:{}:{}", cell(&row, 0), cell(&row, 1)));
            }
        }
        // `rel_table_pairs` omits the scanned code pairs (see `CODE_REL_PAIRS`),
        // so the oracle must add them back — otherwise it would silently skip
        // every Function→Function `Calls` / Function→Struct `Uses` edge and
        // compare only the tables a full rebuild happens to share with it.
        let mut pairs: Vec<(&str, &str, &str)> = load::rel_table_pairs().to_vec();
        pairs.extend(CODE_REL_PAIRS);
        for (table, from, to) in pairs {
            if !matches!(
                table,
                "Contains" | "Calls" | "Uses" | "UnresolvedCall" | "UnresolvedUse"
            ) {
                continue;
            }
            let (_, rows) = query_rows(
                &conn,
                &format!("MATCH (x:{from})-[r:{table}]->(y:{to}) RETURN x.fqn AS x, y.fqn AS y"),
            )
            .unwrap();
            for row in rows {
                out.insert(format!("{table}:{}->{}", cell(&row, 0), cell(&row, 1)));
            }
        }
        let (_, rows) = query_rows(
            &conn,
            "MATCH (s:Scan) RETURN s.fqn AS fqn, s.git_sha AS sha, s.git_clean AS clean, \
             s.content_key AS key, s.scanned_at AS at",
        )
        .unwrap();
        for row in rows {
            out.insert(format!(
                "Scan:{}|{}|{}|{}|{}",
                cell(&row, 0),
                cell(&row, 1),
                cell(&row, 2),
                cell(&row, 3),
                cell(&row, 4)
            ));
        }
        out
    }

    /// The previous full graph for the two-language fixture: a changed language
    /// (`godemo` -> `godemo/changed`, one file `changed`) and a skipped
    /// language whose hierarchy has two pure-intermediate modules
    /// (`Apg` -> `Apg.CsharpFrontend` -> `Apg.CsharpFrontend.Tests`) above the
    /// leaf module that owns the file `skipped`. Neither `Apg` nor
    /// `Apg.CsharpFrontend` has a File child, so a full scan emits them only as
    /// `Module -> Module` scaffolding.
    fn multi_lang_previous(changed: &str, skipped: &str) -> Graph {
        let module = |_: &str| Node {
            kind: NodeKind::Module,
            ..Node::default()
        };
        let mut g = Graph::default();
        // The changed language (it spawns and re-emits its full hierarchy).
        g.nodes.insert("godemo".into(), module("godemo"));
        g.nodes
            .insert("godemo/changed".into(), module("godemo/changed"));
        g.nodes
            .insert(changed.into(), located(NodeKind::File, changed, 1, 30));
        g.nodes.insert(
            "godemo.changed.S".into(),
            located(NodeKind::Struct, changed, 1, 30),
        );
        g.nodes.insert(
            "godemo.changed.S.f".into(),
            located(NodeKind::Function, changed, 2, 10),
        );
        g.contains
            .insert(("godemo".into(), "godemo/changed".into()));
        g.contains.insert(("godemo/changed".into(), changed.into()));
        g.contains
            .insert((changed.into(), "godemo.changed.S".into()));
        g.contains
            .insert((changed.into(), "godemo.changed.S.f".into()));
        g.contains
            .insert(("godemo.changed.S".into(), "godemo.changed.S.f".into()));
        // The skipped language: global Module->Module scaffolding with two
        // pure-intermediate modules and a leaf module that owns the file.
        g.nodes.insert("Apg".into(), module("Apg"));
        g.nodes
            .insert("Apg.CsharpFrontend".into(), module("Apg.CsharpFrontend"));
        g.nodes.insert(
            "Apg.CsharpFrontend.Tests".into(),
            module("Apg.CsharpFrontend.Tests"),
        );
        g.nodes
            .insert(skipped.into(), located(NodeKind::File, skipped, 1, 20));
        g.nodes.insert(
            "Apg.CsharpFrontend.Tests.Program".into(),
            located(NodeKind::Struct, skipped, 1, 20),
        );
        g.nodes.insert(
            "Apg.CsharpFrontend.Tests.Program.Main".into(),
            located(NodeKind::Function, skipped, 2, 10),
        );
        g.contains
            .insert(("Apg".into(), "Apg.CsharpFrontend".into()));
        g.contains.insert((
            "Apg.CsharpFrontend".into(),
            "Apg.CsharpFrontend.Tests".into(),
        ));
        g.contains
            .insert(("Apg.CsharpFrontend.Tests".into(), skipped.into()));
        g.contains
            .insert((skipped.into(), "Apg.CsharpFrontend.Tests.Program".into()));
        g.contains.insert((
            skipped.into(),
            "Apg.CsharpFrontend.Tests.Program.Main".into(),
        ));
        g.contains.insert((
            "Apg.CsharpFrontend.Tests.Program".into(),
            "Apg.CsharpFrontend.Tests.Program.Main".into(),
        ));
        g.nodes.insert(
            crate::schema::SCAN_HEAD.into(),
            scan_node("oldsha", "oldkey", "2026-01-01T00:00:00Z"),
        );
        g
    }

    /// The win-B ASSEMBLED graph a partial scan produces when the changed
    /// language spawns and the skipped language does not: the changed language's
    /// full hierarchy is re-emitted, and the skipped language's cached per-file
    /// facts arrive (its leaf module is the reused File's direct parent) but its
    /// global scaffolding — the two pure-intermediate modules and every
    /// `Module -> Module` hierarchy edge — is MISSING. This is exactly the
    /// assembled graph the feedback names.
    fn multi_lang_assembled(changed: &str, skipped: &str) -> Graph {
        let module = |_: &str| Node {
            kind: NodeKind::Module,
            ..Node::default()
        };
        let mut g = Graph::default();
        g.nodes.insert("godemo".into(), module("godemo"));
        g.nodes
            .insert("godemo/changed".into(), module("godemo/changed"));
        g.nodes
            .insert(changed.into(), located(NodeKind::File, changed, 1, 40));
        g.nodes.insert(
            "godemo.changed.S".into(),
            located(NodeKind::Struct, changed, 1, 40),
        );
        g.nodes.insert(
            "godemo.changed.S.f".into(),
            located(NodeKind::Function, changed, 2, 10),
        );
        g.nodes.insert(
            "godemo.changed.S.h".into(),
            located(NodeKind::Function, changed, 12, 20),
        );
        g.contains
            .insert(("godemo".into(), "godemo/changed".into()));
        g.contains.insert(("godemo/changed".into(), changed.into()));
        g.contains
            .insert((changed.into(), "godemo.changed.S".into()));
        g.contains
            .insert((changed.into(), "godemo.changed.S.f".into()));
        g.contains
            .insert((changed.into(), "godemo.changed.S.h".into()));
        g.contains
            .insert(("godemo.changed.S".into(), "godemo.changed.S.f".into()));
        g.contains
            .insert(("godemo.changed.S".into(), "godemo.changed.S.h".into()));
        // Cached facts for the skipped language: the reused File's direct-parent
        // module only — NO `Apg`, NO `Apg.CsharpFrontend`, NO Module->Module
        // edges.
        g.nodes.insert(
            "Apg.CsharpFrontend.Tests".into(),
            module("Apg.CsharpFrontend.Tests"),
        );
        g.nodes
            .insert(skipped.into(), located(NodeKind::File, skipped, 1, 20));
        g.nodes.insert(
            "Apg.CsharpFrontend.Tests.Program".into(),
            located(NodeKind::Struct, skipped, 1, 20),
        );
        g.nodes.insert(
            "Apg.CsharpFrontend.Tests.Program.Main".into(),
            located(NodeKind::Function, skipped, 2, 10),
        );
        g.contains
            .insert(("Apg.CsharpFrontend.Tests".into(), skipped.into()));
        g.contains
            .insert((skipped.into(), "Apg.CsharpFrontend.Tests.Program".into()));
        g.contains.insert((
            skipped.into(),
            "Apg.CsharpFrontend.Tests.Program.Main".into(),
        ));
        g.contains.insert((
            "Apg.CsharpFrontend.Tests.Program".into(),
            "Apg.CsharpFrontend.Tests.Program.Main".into(),
        ));
        g.nodes.insert(
            crate::schema::SCAN_HEAD.into(),
            scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
        );
        g
    }

    /// The TRUE new tree of the multi-language fixture — the full-rebuild
    /// reference: the changed language re-emitted (with the new `...S.h`), the
    /// skipped language untouched.
    fn multi_lang_new(changed: &str, skipped: &str) -> Graph {
        let mut g = multi_lang_previous(changed, skipped);
        g.nodes.insert(
            "godemo.changed.S.h".into(),
            located(NodeKind::Function, changed, 12, 20),
        );
        g.contains
            .insert((changed.into(), "godemo.changed.S.h".into()));
        g.contains
            .insert(("godemo.changed.S".into(), "godemo.changed.S.h".into()));
        g.nodes.insert(
            crate::schema::SCAN_HEAD.into(),
            scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
        );
        g
    }

    /// The TRUE new tree of the multi-package Java fixture — the full-rebuild
    /// reference: `pkg`, `pkg.a` (unchanged), `pkg.b` (changed), `pkg.c`
    /// (unchanged), each package's `Module` record and `Module -> Module`
    /// hierarchy edge, plus one File/Struct/Function per package. Java emits the
    /// package hierarchy for EVERY package it walks, whether or not the package's
    /// per-file facts pass the targeted emission filter.
    fn java_pkg_tree(a: &str, b: &str, c: &str) -> Graph {
        let module = |_: &str| Node {
            kind: NodeKind::Module,
            ..Node::default()
        };
        let mut g = Graph::default();
        for p in ["pkg", "pkg.a", "pkg.b", "pkg.c"] {
            g.nodes.insert(p.to_string(), module(p));
        }
        for (pkg, file, ty) in [("pkg.a", a, "A"), ("pkg.b", b, "B"), ("pkg.c", c, "C")] {
            let st = format!("{pkg}.{ty}");
            let fun = format!("{pkg}.{ty}.f");
            g.nodes
                .insert(file.to_string(), located(NodeKind::File, file, 1, 20));
            g.nodes
                .insert(st.clone(), located(NodeKind::Struct, file, 1, 20));
            g.nodes
                .insert(fun.clone(), located(NodeKind::Function, file, 2, 10));
            // The ingestor derives a `File -> unit` edge for every located
            // Struct AND Function (Pass B3), so the full-rebuild reference must
            // carry both or the oracle compares against a graph no full scan
            // would ever produce.
            g.contains.insert((pkg.to_string(), file.to_string()));
            g.contains.insert((file.to_string(), st.clone()));
            g.contains.insert((file.to_string(), fun.clone()));
            g.contains.insert((st.clone(), fun.clone()));
        }
        for p in ["pkg.a", "pkg.b", "pkg.c"] {
            g.contains.insert(("pkg".to_string(), p.to_string()));
        }
        g.nodes.insert(
            crate::schema::SCAN_HEAD.into(),
            scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
        );
        g
    }

    /// The dot-prefixed transient files this module creates (seed copy, publish
    /// temp, backup). Ignores unrelated dotfiles such as `.DS_Store`.
    fn transient_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| {
                n.starts_with('.')
                    && (n.contains(".seed-") || n.contains(".graph-") || n.contains(".prev-"))
            })
            .collect();
        names.sort();
        names
    }

    /// Seeds `prev_path` and applies the standard delta (a body change to
    /// `a.go` and the removal of `c.go`), returning the spliced copy together
    /// with the assembled graph that `publish` must render the export from.
    fn seed_and_splice(prev_path: &Path, a: &str, b: &str, c: &str) -> (SeededDb, Graph) {
        let seeded = match seed(prev_path) {
            SeedDecision::Seed(s) => s,
            SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
        };
        let assembled = assembled_graph(a, b);
        let targets: BTreeSet<String> = [a.to_string(), c.to_string()].into_iter().collect();
        let removed: BTreeSet<String> = BTreeSet::new();
        seeded
            .apply(&SpliceDelta {
                graph: &assembled,
                targets: &targets,
                removed_fqns: &removed,
                scan: ScanRow {
                    git_sha: Some("newsha".into()),
                    git_clean: Some(true),
                    content_key: Some("newkey".into()),
                    scanned_at: "2026-01-02T00:00:00Z".into(),
                },
            })
            .unwrap();
        (seeded, assembled)
    }

    /// Reads the current `db.lbug` snapshot through the code oracle.
    fn published_snapshot(path: &Path) -> BTreeSet<String> {
        let db = Database::new(path, SystemConfig::default().read_only(true)).unwrap();
        let snap = code_snapshot(&db);
        drop(db);
        snap
    }
}
