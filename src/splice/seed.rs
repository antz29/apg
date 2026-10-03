//! The seed half of the splice (phase-03 task-1): resolve and validate the
//! previous `db.lbug`, whole-file copy it to a same-directory temp sibling, and
//! open the copy read-write — the [`SeedDecision`] surface the pipeline
//! dispatches on.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use lbug::{Connection, Database, SystemConfig};

use crate::graph::{Graph, Node, NodeKind};
use crate::load;
use crate::schema::Record;

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
///
/// `assembled` is the assembled graph's authored/transient identity
/// ([`assembled_authored_identity`]). Once the content key matches, the seed DB
/// must ALSO represent the assembled authored/transient rows: the win-C splice
/// writes no authored row, while `graph.jsonl` is serialized from the full
/// assembled graph, so a same-content seed that lacks those rows would publish
/// a DB diverging from its export. When the seed's own digest
/// ([`seed_authored_identity`]) differs, the seed is refused with
/// [`SeedFallback::UnrepresentedAuthored`] and the caller runs the full load.
///
/// Every failure is a [`SeedFallback`], never a panic: an unopenable seed DB on
/// the authored-identity read falls back to the full load rather than
/// propagating. Refusal ordering is content-key first (`StaleSeed`), then
/// authored identity.
pub fn seed_checked(previous: &Path, expected: Option<&str>, assembled: &str) -> SeedDecision {
    if !previous.exists() {
        return SeedDecision::FullLoad(SeedFallback::MissingPrevious);
    }
    let seed_key = match recorded_content_key(previous) {
        Ok(key) => key,
        Err(e) => return SeedDecision::FullLoad(SeedFallback::Unreadable(e.to_string())),
    };
    if !seed_key_matches(seed_key.as_deref(), expected) {
        return SeedDecision::FullLoad(SeedFallback::StaleSeed {
            seed: seed_key.unwrap_or_else(|| "(none)".to_string()),
            recorded: expected.unwrap_or("(none)").to_string(),
        });
    }
    // Content key matches: the tree content is the same, but the seed must also
    // represent the assembled graph's authored/transient rows. A read failure is
    // a fallback (never a panic), exactly like the content-key read above.
    let seed_authored = match seed_authored_identity(previous) {
        Ok(identity) => identity,
        Err(e) => return SeedDecision::FullLoad(SeedFallback::Unreadable(e.to_string())),
    };
    if seed_authored != assembled {
        return SeedDecision::FullLoad(SeedFallback::UnrepresentedAuthored {
            assembled: assembled.to_string(),
            seed: seed_authored,
        });
    }
    seed(previous)
}

/// The pure content-key equivalence the seed guard applies (task-10): the seed
/// is current iff BOTH sides carry a key and they are equal. A missing key on
/// either side is never equivalent — a seed DB or a shared record written before
/// the phase-01 content key cannot be verified as the same tree, so the caller
/// runs the full load (the correctness reference) rather than publish a
/// divergence. No filesystem, no database.
pub fn seed_key_matches(seed: Option<&str>, expected: Option<&str>) -> bool {
    matches!((seed, expected), (Some(s), Some(r)) if s == r)
}

/// The canonical digest of the assembled graph's **authored/transient** rows —
/// the layer/plan/feedback node kinds (`Requirement`, `Note`, `Feedback`,
/// `Plan`, `PlanPhase`, `Task`, `Stakeholder`, `User`, `DomainGroup`, `Entity`,
/// `Value`, `Service`, `System`, `Container`, `Component`, `Person`,
/// `Constraint`) and the edges those nodes author — the identity the seed
/// equivalence guard ([`seed_checked`]) compares against a seed DB's own
/// authored/transient set.
///
/// The win-C splice writes no authored/transient row (it applies only the code
/// delta), while `graph.jsonl` is serialized from the FULL assembled graph, so
/// a spec-only change-set would publish a DB that diverges from its export. The
/// caller threads this digest — and a seed DB digest computed the same way
/// (`seed_authored_identity`) — into the guard; an inequality makes the seed
/// ineligible and the existing full load (the correctness reference) runs.
///
/// ## Canonical form
///
/// Every authored/transient node contributes one row, every edge whose SOURCE
/// node is one of those kinds contributes one row, and each row is a JSON array
/// of strings:
///
/// * node — `["N", <DB table>, <column 1>, …, <column n>]`, the table's columns
///   in `create_schema` order, so the digest is reproducible from the seed DB's
///   own rows. A missing property is normalized to exactly what
///   `build_load_files` writes: the empty string for an absent string, `0` for
///   an absent `PlanPhase.number`;
/// * edge — `["E", <DB rel table>, <from>, <to>]`. The source-kind filter
///   excludes the code pairs of the shared `Contains`/`Calls`/`Uses` tables and
///   includes `Details`/`Reviews`/`SpecImplementedBy` edges whose target is a
///   code FQN.
///
/// The rows are sorted (so set iteration order never matters) and folded into a
/// 64-bit FNV-1a digest rendered as 16 lowercase hex digits. Pure: it reads
/// only `graph` — no filesystem, database, git, or process.
pub fn assembled_authored_identity(graph: &Graph) -> String {
    // The Option→value normalization `build_load_files` applies: an absent
    // string is written as "", never NULL, so both sides agree.
    let empty = |v: &Option<String>| v.clone().unwrap_or_default();

    let node_row = |fqn: &str, node: &Node| -> Option<String> {
        let mut row: Vec<String> = Vec::new();
        match node.kind {
            NodeKind::Requirement => {
                row.push("N".into());
                row.push("Requirement".into());
                row.push(fqn.into());
                row.push(empty(&node.id));
                row.push(empty(&node.title));
                row.push(empty(&node.body));
                row.push(empty(&node.feature));
            }
            NodeKind::Note => {
                row.push("N".into());
                row.push("Note".into());
                row.push(fqn.into());
                row.push(empty(&node.body));
                row.push(empty(&node.sub_kind));
            }
            NodeKind::Feedback => {
                row.push("N".into());
                row.push("Feedback".into());
                row.push(fqn.into());
                row.push(empty(&node.body));
                row.push(empty(&node.status));
                row.push(empty(&node.disposition));
            }
            NodeKind::Plan => {
                row.push("N".into());
                row.push("Plan".into());
                row.push(fqn.into());
                row.push(empty(&node.title));
                row.push(empty(&node.strategy));
            }
            NodeKind::PlanPhase => {
                row.push("N".into());
                row.push("PlanPhase".into());
                row.push(fqn.into());
                row.push(node.number.unwrap_or_default().to_string());
                row.push(empty(&node.title));
                row.push(empty(&node.deliverable));
                row.push(empty(&node.status));
            }
            NodeKind::Task => {
                row.push("N".into());
                row.push("Task".into());
                row.push(fqn.into());
                row.push(empty(&node.title));
                row.push(empty(&node.sub_kind));
                row.push(empty(&node.tier));
                row.push(empty(&node.status));
                row.push(empty(&node.verb));
                row.push(empty(&node.target));
                row.push(empty(&node.new_fqn));
            }
            NodeKind::Stakeholder => {
                row.push("N".into());
                row.push("Stakeholder".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::Entity => {
                row.push("N".into());
                row.push("Entity".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::System => {
                row.push("N".into());
                row.push("System".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::Container => {
                row.push("N".into());
                row.push("Container".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.sub_kind));
                row.push(empty(&node.body));
            }
            NodeKind::Component => {
                row.push("N".into());
                row.push("Component".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::User => {
                row.push("N".into());
                row.push("User".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::Group => {
                row.push("N".into());
                row.push("DomainGroup".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.attribute));
                row.push(empty(&node.root));
                row.push(empty(&node.body));
            }
            NodeKind::Value => {
                row.push("N".into());
                row.push("Value".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::Service => {
                row.push("N".into());
                row.push("Service".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::Person => {
                row.push("N".into());
                row.push("Person".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
            }
            NodeKind::Constraint => {
                row.push("N".into());
                row.push("Constraint".into());
                row.push(fqn.into());
                row.push(empty(&node.name));
                row.push(empty(&node.body));
                row.push(empty(&node.attaches_to));
            }
            _ => return None,
        }
        Some(serde_json::to_string(&row).expect("a JSON array of strings is always serializable"))
    };

    let is_authored = |kind: NodeKind| {
        matches!(
            kind,
            NodeKind::Requirement
                | NodeKind::Note
                | NodeKind::Feedback
                | NodeKind::Plan
                | NodeKind::PlanPhase
                | NodeKind::Task
                | NodeKind::Stakeholder
                | NodeKind::User
                | NodeKind::Group
                | NodeKind::Entity
                | NodeKind::Value
                | NodeKind::Service
                | NodeKind::System
                | NodeKind::Container
                | NodeKind::Component
                | NodeKind::Person
                | NodeKind::Constraint
        )
    };

    let mut rows: Vec<String> = Vec::new();
    for (fqn, node) in &graph.nodes {
        if let Some(row) = node_row(fqn, node) {
            rows.push(row);
        }
    }

    // The loader writes a spec edge into the DB only when its `(table, from, to)`
    // kind pair is one the schema declares ([`load::tables::rel_table_pairs`],
    // the single enumeration shared with `build_load_files`/`create_schema`).
    // The DB-side twin ([`seed_authored_identity`]) can therefore only ever
    // observe those pairs, so the graph-side canonical form must apply the same
    // filter: a raw `graph.*` edge whose pair is undeclared — e.g. a legacy
    // `Note`→`Note` `details` the write-time validator now refuses
    // ([`crate::layers::validate`]) — is not representable in the DB and must
    // not contribute a row, or the two digests can never agree and freshness
    // fails closed forever.
    let pair_allowed = |table: &str, from: NodeKind, to: NodeKind| {
        let (from_label, to_label) = (load::tables::label_of(from), load::tables::label_of(to));
        load::tables::rel_table_pairs()
            .iter()
            .any(|(t, f, o)| *t == table && *f == from_label && *o == to_label)
    };
    // An edge belongs to the authored/transient set iff its SOURCE is one of the
    // authored/transient kinds AND its `(table, from, to)` pair is DB-declared:
    // this keeps the shared `Contains`/`Calls`/`Uses` tables' code pairs out
    // while retaining authored edges whose target is a code FQN (`Details`,
    // `Reviews`, `SpecImplementedBy`).
    let mut add_edges = |table: &str, edges: &HashSet<(String, String)>| {
        for (from, to) in edges {
            let Some(from_node) = graph.nodes.get(from) else {
                continue;
            };
            if !is_authored(from_node.kind) {
                continue;
            }
            let Some(to_node) = graph.nodes.get(to) else {
                continue;
            };
            if !pair_allowed(table, from_node.kind, to_node.kind) {
                continue;
            }
            rows.push(
                serde_json::to_string(&["E", table, from.as_str(), to.as_str()])
                    .expect("a JSON array of strings is always serializable"),
            );
        }
    };
    add_edges("Contains", &graph.contains);
    add_edges("Calls", &graph.calls);
    add_edges("Uses", &graph.uses);
    add_edges("Details", &graph.details);
    add_edges("Reviews", &graph.reviews);
    add_edges("DependsOn", &graph.depends_on);
    add_edges("Gates", &graph.gates);
    add_edges("Satisfies", &graph.satisfies);
    add_edges("Drives", &graph.drives);
    add_edges("Represents", &graph.represents);
    add_edges("RealisedBy", &graph.realised_by);
    add_edges("SpecImplementedBy", &graph.spec_implemented_by);
    add_edges("Publishes", &graph.publishes);
    add_edges("Subscribes", &graph.subscribes);

    // Order-independent: sort before folding, then include the row separator so
    // adjacent rows can never run together ambiguously.
    rows.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in &rows {
        for &byte in row.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= u64::from(b'\n');
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The canonical digest of a **seed DB's own** authored/transient rows — the
/// read-only, DB-side twin of [`assembled_authored_identity`].
///
/// Opens `previous` read-only and reproduces, from the DB's own rows, the exact
/// canonical form [`assembled_authored_identity`] computes from an assembled
/// [`Graph`]: one row per authored/transient node — the seventeen
/// layer/plan/feedback tables, whose columns are projected in `create_schema`
/// order — and one row per edge in the fourteen authored/transient rel tables
/// whose SOURCE FQN belongs to one of those tables. Each row is a JSON array of
/// strings; the rows are sorted and folded into the same 64-bit FNV-1a digest.
///
/// `seed_authored_identity(seed) == assembled_authored_identity(graph)` iff the
/// seed DB already represents the assembled graph's authored/transient rows —
/// the equivalence the seed guard ([`seed_checked`]) requires before letting the
/// code-only win-C splice publish a DB that must answer like a full rebuild.
///
/// Pure of side effects on the DB (read-only; no writes and no schema changes)
/// and never panics: a missing, locked, or corrupt DB — or one missing a table —
/// is returned as an `Err`.
pub fn seed_authored_identity(previous: &Path) -> anyhow::Result<String> {
    let db = Database::new(previous, SystemConfig::default().read_only(true))?;
    let conn = Connection::new(&db)?;

    // Every authored/transient node table and its columns in `create_schema`
    // order — the exact table/column vocabulary `build_load_files` writes.
    let node_tables: [(&str, &[&str]); 17] = [
        ("Requirement", &["fqn", "id", "title", "body", "feature"]),
        ("Note", &["fqn", "body", "kind"]),
        ("Feedback", &["fqn", "body", "status", "disposition"]),
        ("Plan", &["fqn", "title", "strategy"]),
        (
            "PlanPhase",
            &["fqn", "number", "title", "deliverable", "status"],
        ),
        (
            "Task",
            &[
                "fqn", "title", "kind", "tier", "status", "verb", "target", "new_fqn",
            ],
        ),
        ("Stakeholder", &["fqn", "name", "body"]),
        ("Entity", &["fqn", "name", "body"]),
        ("System", &["fqn", "name", "body"]),
        ("Container", &["fqn", "name", "kind", "body"]),
        ("Component", &["fqn", "name", "body"]),
        ("User", &["fqn", "name", "body"]),
        ("DomainGroup", &["fqn", "name", "attribute", "root", "body"]),
        ("Value", &["fqn", "name", "body"]),
        ("Service", &["fqn", "name", "body"]),
        ("Person", &["fqn", "name", "body"]),
        ("Constraint", &["fqn", "name", "body", "attaches_to"]),
    ];

    let mut authored: HashSet<String> = HashSet::new();
    let mut rows: Vec<String> = Vec::new();
    for (table, columns) in node_tables {
        let projection = columns
            .iter()
            .map(|c| format!("n.`{c}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let (_, node_rows) = query_rows(&conn, &format!("MATCH (n:{table}) RETURN {projection}"))?;
        for row in node_rows {
            authored.insert(cell(&row, 0));
            let mut canonical: Vec<String> = Vec::with_capacity(columns.len() + 2);
            canonical.push("N".into());
            canonical.push(table.into());
            for i in 0..columns.len() {
                canonical.push(cell(&row, i));
            }
            rows.push(serde_json::to_string(&canonical)?);
        }
    }

    // An edge belongs to the authored/transient set iff its SOURCE FQN is one
    // of the authored/transient nodes — the same source-kind filter
    // `assembled_authored_identity` applies. This drops the shared
    // `Contains`/`Calls`/`Uses` tables' code pairs while retaining authored
    // edges whose target is a code FQN (`Details`, `Reviews`,
    // `SpecImplementedBy`).
    let rel_tables: [&str; 14] = [
        "Contains",
        "Calls",
        "Uses",
        "Details",
        "Reviews",
        "DependsOn",
        "Gates",
        "Satisfies",
        "Drives",
        "Represents",
        "RealisedBy",
        "SpecImplementedBy",
        "Publishes",
        "Subscribes",
    ];
    for table in rel_tables {
        let (_, edge_rows) = query_rows(
            &conn,
            &format!("MATCH (a)-[:{table}]->(b) RETURN a.fqn, b.fqn"),
        )?;
        for row in edge_rows {
            let from = cell(&row, 0);
            if authored.contains(&from) {
                let to = cell(&row, 1);
                rows.push(serde_json::to_string(&[
                    "E",
                    table,
                    from.as_str(),
                    to.as_str(),
                ])?);
            }
        }
    }

    // Byte-identical canonical fold to `assembled_authored_identity`: sort, then
    // fold each row plus its separator into the 64-bit FNV-1a digest.
    rows.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in &rows {
        for &byte in row.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= u64::from(b'\n');
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(format!("{hash:016x}"))
}

/// The canonical digest of the worktree's **authored/transient sources** — the
/// tree-side source of truth for [`seed_authored_identity`]'s DB-side digest.
///
/// Assembles exactly the post-code record stream `cmd_scan` chains after the
/// scanner records — the durable `apg/layers/**` tree via
/// [`crate::layers::ingest_tree`] plus the transient `.trans/plans/*.jsonl` and
/// `.trans/<tier>/*.jsonl` legs via [`crate::specs::plan_files`] /
/// [`crate::specs::trans_mirror_files`] — and folds it through the single
/// canonicalizer [`assembled_authored_identity`], so the result is
/// byte-identical to those same authored rows' entry in a real scan's assembled
/// graph.
///
/// The code-reference universe `ingest_tree` validates `implemented-by` targets
/// against is read from the export
/// ([`crate::artifacts::code_universes_from_export`]), never from the DB. Those
/// real code FQNs are ALSO carried into the assembly as placeholder code nodes,
/// so `ingest`'s finalize keeps the authored→code edges (`implemented-by`,
/// `details`, `reviews`) exactly as the real scan's assembled graph does — the
/// real scan has the actual code nodes; the tree has none. A `PlannedNode`
/// placeholder enters at its exact FQN (no language re-rooting) and, being a
/// code kind, contributes no authored row to the digest; its only effect is to
/// keep those edges.
///
/// No database is opened. Every failure (a malformed layers/transient file, a
/// drift ref, a bad export line) is returned as an `Err`.
pub fn tree_authored_identity(apg_root: &Path) -> anyhow::Result<String> {
    // The code-identity universe the real scan validates `implemented-by` refs
    // against, from the export only.
    let (scanned, planned) = crate::artifacts::code_universes_from_export(apg_root)?;

    // The durable tree (validated against the code/planned universes) plus the
    // worktree's transient legs — exactly `cmd_scan`'s post-code record stream.
    let mut records = crate::layers::ingest_tree(apg_root, &scanned, &planned)?;
    for path in crate::specs::plan_files(apg_root)
        .into_iter()
        .chain(crate::specs::trans_mirror_files(apg_root))
    {
        records.extend(crate::specs::read_jsonl(&path)?);
    }

    // Placeholder code nodes for the code universe — the analog of the real
    // scan's actual code nodes, without which `ingest`'s finalize would prune
    // every authored→code edge (the target node would be absent). Code kinds
    // contribute no authored row, so the digest is unchanged by their presence
    // other than through the edges they keep.
    for fqn in scanned.iter().chain(planned.iter()) {
        records.push(Record::PlannedNode {
            fqn: fqn.clone(),
            kind: "module".to_string(),
            name: String::new(),
            parent: String::new(),
        });
    }

    let (graph, _) = crate::ingest::ingest(
        records,
        &crate::ingest::IngestOptions {
            blacklist: &[],
            language: "",
            config: None,
            base: None,
        },
    );
    Ok(assembled_authored_identity(&graph))
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
pub struct ColumnShape {
    name: String,
    type_name: String,
    primary_key: bool,
}

/// One table's structural shape: its kind (`NODE`/`REL`), its ordered columns,
/// and — for a rel table — its declared `(from, to)` connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableShape {
    kind: String,
    columns: Vec<ColumnShape>,
    connections: Vec<(String, String)>,
}

/// The whole-DB structural fingerprint: table name → shape. A `BTreeMap` so the
/// engine's internal table ordering never affects the comparison.
pub type SchemaFingerprint = BTreeMap<String, TableShape>;

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
pub fn extract_schema(conn: &Connection) -> anyhow::Result<SchemaFingerprint> {
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
pub fn query_rows(
    conn: &Connection,
    query: &str,
) -> anyhow::Result<(Vec<String>, Vec<Vec<String>>)> {
    let result = conn.query(query)?;
    let names = result.get_column_names();
    let rows = result
        .map(|row| row.iter().map(|v| v.to_string()).collect())
        .collect();
    Ok((names, rows))
}

/// The index of column `want` in a result's header.
pub fn column_index(names: &[String], want: &str) -> anyhow::Result<usize> {
    names
        .iter()
        .position(|n| n == want)
        .ok_or_else(|| anyhow::anyhow!("query result is missing the `{want}` column: {names:?}"))
}

/// Cell `i` of `row`, or an empty string when the row is short.
pub fn cell(row: &[String], i: usize) -> String {
    row.get(i).cloned().unwrap_or_default()
}

/// Escapes a value for a single-quoted Cypher string literal.
pub(super) fn cypher_escape(value: &str) -> String {
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
    /// The previous DB was built from the same tree content the delta was
    /// derived from, but its **authored/transient rows** are not the assembled
    /// graph's — the win-C splice writes no authored/transient row and
    /// `graph.jsonl` is serialized from the full assembled graph, so seeding it
    /// would publish a DB that diverges from its export (phase-01). Distinct
    /// from [`StaleSeed`](Self::StaleSeed), which is a content-key / worktree
    /// drift: this is the assembled identity's authored/transient divergence.
    /// Carries the seed DB's authored/transient digest and the assembled
    /// graph's so the log attributes the divergence. The caller runs the full
    /// load.
    UnrepresentedAuthored { assembled: String, seed: String },
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
            SeedFallback::UnrepresentedAuthored { assembled, seed } => format!(
                "previous db.lbug does not represent the assembled graph's authored/transient rows (seed {seed}, assembled {assembled}) — full load"
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
