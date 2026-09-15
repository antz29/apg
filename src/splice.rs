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
//! 3. **task-3** — atomically `rename` the temp copy over `target_path`;
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lbug::{Connection, Database, SystemConfig};

use crate::load;

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

    /// (a) A seed copy preserves every table / every row of the previous DB.
    #[test]
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
}
