mod common;

use apg::graph::{Graph, Location, Node, NodeKind};
use apg::load;
use apg::schema::SCAN_HEAD;
use apg::splice::*;
use apg::testutil::read_graph_jsonl;
use lbug::{Connection, Database, SystemConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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

/// Inserts one authored/transient node (`Requirement`) plus one authored edge
/// whose source is that node — enough to give a graph a non-trivial
/// authored/transient identity. Shared by both sides of the authored-identity
/// guard test (the seed DB's built graph and the assembled graph) so their
/// digests match.
fn with_authored_row(mut g: Graph) -> Graph {
    let mut req = Node {
        kind: NodeKind::Requirement,
        ..Node::default()
    };
    req.body = Some("the requirement body".into());
    req.feature = Some("the feature".into());
    g.nodes.insert("requirements.requirement.req".into(), req);
    g.nodes.insert(
        "domain.entity.task".into(),
        Node {
            kind: NodeKind::Entity,
            ..Node::default()
        },
    );
    g.drives.insert((
        "requirements.requirement.req".into(),
        "domain.entity.task".into(),
    ));
    g
}

/// [`with_authored_row`] plus the `Scan` row the seed guard's content-key check
/// reads — a realistic seed side: one `Requirement` (`req`), its `Entity`, and a
/// `Drives` edge, built at content key `oldkey`.
fn seeded_authored_graph(base: Graph) -> Graph {
    let mut g = with_authored_row(base);
    g.nodes.insert(
        SCAN_HEAD.into(),
        scan_node("oldsha", "oldkey", "2026-01-01T00:00:00Z"),
    );
    g
}

/// A spec-only delta over [`seeded_authored_graph`]: the SAME code, the SAME
/// `Scan` row, but an EXTRA `Requirement` (`req2`) and its `Drives` edge the
/// seed DB does not carry. Its authored/transient identity can therefore never
/// equal a seed built from [`seeded_authored_graph`], while every code row is
/// unchanged.
fn with_authored_delta(base: Graph) -> Graph {
    let mut g = seeded_authored_graph(base);
    let req2 = Node {
        kind: NodeKind::Requirement,
        body: Some("a second requirement".into()),
        ..Node::default()
    };
    g.nodes.insert("requirements.requirement.req2".into(), req2);
    g.drives.insert((
        "requirements.requirement.req2".into(),
        "domain.entity.task".into(),
    ));
    g
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
        SCAN_HEAD.into(),
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
        SCAN_HEAD.into(),
        scan_node("newsha", "newkey", "2026-01-02T00:00:00Z"),
    );
    g
}

/// A wide fixture tree for the batched-DML regression (phase-04 task-22):
/// one module `w`, `n` `/w/f{i}.go` file/struct/function triples, a chain of
/// `Calls` between the functions, a `Uses` per function, and one shared
/// external `UnresolvedTarget` per file. `drop` omits file index `drop` and
/// every edge that names its units — the removed-file half of the delta.
fn wide_tree(n: usize, drop: usize, sha: &str, key: &str) -> Graph {
    let mut g = Graph::default();
    g.nodes.insert(
        "w".into(),
        Node {
            kind: NodeKind::Module,
            ..Node::default()
        },
    );
    for i in 0..n {
        if i == drop {
            continue;
        }
        let path = format!("/w/f{i}.go");
        let sty = format!("w.C{i}");
        let fun = format!("w.C{i}.m");
        g.nodes
            .insert(path.clone(), located(NodeKind::File, &path, 1, 20));
        g.nodes
            .insert(sty.clone(), located(NodeKind::Struct, &path, 1, 20));
        g.nodes
            .insert(fun.clone(), located(NodeKind::Function, &path, 2, 10));
        g.contains.insert(("w".to_string(), path.clone()));
        g.contains.insert((path.clone(), sty.clone()));
        g.contains.insert((path.clone(), fun.clone()));
        g.contains.insert((sty.clone(), fun.clone()));
        g.uses.insert((fun.clone(), sty.clone()));
        if i + 1 < n && i + 1 != drop {
            g.calls.insert((fun.clone(), format!("w.C{}.m", i + 1)));
        }
        let ext = format!("ext.T{i}");
        g.nodes.insert(
            ext.clone(),
            Node {
                kind: NodeKind::UnresolvedTarget,
                category: Some("external".into()),
                ..Node::default()
            },
        );
        g.unresolved_calls
            .insert((fun.clone(), ext.clone(), String::new()));
        g.unresolved_uses.insert((fun, ext));
    }
    g.nodes.insert(
        SCAN_HEAD.into(),
        scan_node(sha, key, "2026-01-02T00:00:00Z"),
    );
    g
}

/// The wide-tree body-change delta: `graph` re-emits every surviving unit;
/// `targets` is the previous tree's every file path (so the dropped file's
/// units are in scope and disappear).
fn wide_delta<'a>(
    graph: &'a Graph,
    targets: &'a BTreeSet<String>,
    removed: &'a BTreeSet<String>,
) -> SpliceDelta<'a> {
    SpliceDelta {
        graph,
        targets,
        removed_fqns: removed,
        scan: ScanRow {
            git_sha: Some("newsha".into()),
            git_clean: Some(true),
            content_key: Some("newkey".into()),
            scanned_at: "2026-01-02T00:00:00Z".into(),
        },
    }
}

/// Seeds `prev`, panicking if the seed was not taken (the test fixture is
/// always schema-compatible).
fn seed_or_panic(prev: &Path) -> SeededDb {
    match seed(prev) {
        SeedDecision::Seed(s) => s,
        SeedDecision::FullLoad(f) => panic!("expected a seed, got: {}", f.describe()),
    }
}

/// One wide-tree splice run's oracle data (phase-04 task-22): the splice
/// report, the spliced DB's code snapshot / per-table counts / unresolved
/// set / Scan row, the full rebuild's counterparts, and whether the abandon
/// path left the previous DB byte-identical.
struct WideOutcome {
    report: SpliceReport,
    spliced_snapshot: BTreeSet<String>,
    expected_snapshot: BTreeSet<String>,
    spliced_counts: BTreeMap<String, i64>,
    expected_counts: BTreeMap<String, i64>,
    spliced_unres: BTreeSet<(String, String)>,
    expected_unres: BTreeSet<(String, String)>,
    spliced_scan: (String, String, String, String),
    expected_scan: (String, String, String, String),
    previous_bytes_preserved: bool,
}

/// Seeds, applies and publishes the wide-tree delta of size `n`, and gathers
/// the full-rebuild comparison. Runs the delta TWICE: once abandoned via
/// `SeededDb::discard` (proving the previous DB is byte-identical and the
/// temp removed) and once published for the oracle.
fn wide_splice(n: usize) -> WideOutcome {
    let dir = scratch(&format!("batch-{n}"));
    let prev_path = dir.join("db.lbug");
    let export = dir.join("graph.jsonl");
    // Previous: every file. Assembled: the same tree minus the LAST file
    // (the removed file), every surviving unit re-emitted (a body change).
    let previous = wide_tree(n, n, "oldsha", "oldkey");
    let assembled = wide_tree(n, n - 1, "newsha", "newkey");
    build_db(&prev_path, &previous);
    let before = std::fs::read(&prev_path).unwrap();
    let targets: BTreeSet<String> = (0..n).map(|i| format!("/w/f{i}.go")).collect();
    let removed: BTreeSet<String> = BTreeSet::new();

    // Run 1 — apply, then abandon via `discard`.
    let seeded = seed_or_panic(&prev_path);
    let first = seeded
        .apply(&wide_delta(&assembled, &targets, &removed))
        .unwrap();
    let temp = seeded.temp_path.clone();
    seeded.discard().unwrap();
    let previous_bytes_preserved = !temp.exists() && std::fs::read(&prev_path).unwrap() == before;

    // Run 2 — apply + publish, then compare the published DB to a full rebuild.
    let seeded = seed_or_panic(&prev_path);
    let report = seeded
        .apply(&wide_delta(&assembled, &targets, &removed))
        .unwrap();
    assert_eq!(
        report.dml_statements, first.dml_statements,
        "the statement count is deterministic across runs"
    );
    publish(seeded, &assembled, &export).unwrap();
    let expected_path = dir.join("expected.lbug");
    build_db(&expected_path, &assembled);

    let out = WideOutcome {
        report,
        spliced_snapshot: published_snapshot(&prev_path),
        expected_snapshot: published_snapshot(&expected_path),
        spliced_counts: table_counts(&prev_path),
        expected_counts: table_counts(&expected_path),
        spliced_unres: unresolved_rows(&prev_path),
        expected_unres: unresolved_rows(&expected_path),
        spliced_scan: scan_row_of(&prev_path),
        expected_scan: scan_row_of(&expected_path),
        previous_bytes_preserved,
    };
    let _ = std::fs::remove_dir_all(&dir);
    out
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
    let language = |_: &str| Node {
        kind: NodeKind::Language,
        ..Node::default()
    };
    let mut g = Graph::default();
    // PHASE_09: the ingestor roots every module/symbol FQN under its
    // lang_switch id, so this "last full scan" graph is rooted — `go.` for
    // the changed language, `csharp.` for the skipped one — and carries each
    // stream's `Language` root plus its `Language -> Module` edge.
    g.nodes.insert("go".into(), language("go"));
    g.nodes.insert("csharp".into(), language("csharp"));
    // The changed language (it spawns and re-emits its full hierarchy).
    g.nodes.insert("go.godemo".into(), module("go.godemo"));
    g.nodes
        .insert("go.godemo/changed".into(), module("go.godemo/changed"));
    g.nodes
        .insert(changed.into(), located(NodeKind::File, changed, 1, 30));
    g.nodes.insert(
        "go.godemo.changed.S".into(),
        located(NodeKind::Struct, changed, 1, 30),
    );
    g.nodes.insert(
        "go.godemo.changed.S.f".into(),
        located(NodeKind::Function, changed, 2, 10),
    );
    g.contains.insert(("go".into(), "go.godemo".into()));
    g.contains.insert(("go".into(), "go.godemo/changed".into()));
    g.contains
        .insert(("go.godemo".into(), "go.godemo/changed".into()));
    g.contains
        .insert(("go.godemo/changed".into(), changed.into()));
    g.contains
        .insert((changed.into(), "go.godemo.changed.S".into()));
    g.contains
        .insert((changed.into(), "go.godemo.changed.S.f".into()));
    g.contains
        .insert(("go.godemo.changed.S".into(), "go.godemo.changed.S.f".into()));
    // The skipped language: global Module->Module scaffolding with two
    // pure-intermediate modules and a leaf module that owns the file.
    g.nodes.insert("csharp.Apg".into(), module("csharp.Apg"));
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend".into(),
        module("csharp.Apg.CsharpFrontend"),
    );
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend.Tests".into(),
        module("csharp.Apg.CsharpFrontend.Tests"),
    );
    g.nodes
        .insert(skipped.into(), located(NodeKind::File, skipped, 1, 20));
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend.Tests.Program".into(),
        located(NodeKind::Struct, skipped, 1, 20),
    );
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend.Tests.Program.Main".into(),
        located(NodeKind::Function, skipped, 2, 10),
    );
    g.contains.insert(("csharp".into(), "csharp.Apg".into()));
    g.contains
        .insert(("csharp".into(), "csharp.Apg.CsharpFrontend".into()));
    g.contains
        .insert(("csharp".into(), "csharp.Apg.CsharpFrontend.Tests".into()));
    g.contains
        .insert(("csharp.Apg".into(), "csharp.Apg.CsharpFrontend".into()));
    g.contains.insert((
        "csharp.Apg.CsharpFrontend".into(),
        "csharp.Apg.CsharpFrontend.Tests".into(),
    ));
    g.contains
        .insert(("csharp.Apg.CsharpFrontend.Tests".into(), skipped.into()));
    g.contains.insert((
        skipped.into(),
        "csharp.Apg.CsharpFrontend.Tests.Program".into(),
    ));
    g.contains.insert((
        skipped.into(),
        "csharp.Apg.CsharpFrontend.Tests.Program.Main".into(),
    ));
    g.contains.insert((
        "csharp.Apg.CsharpFrontend.Tests.Program".into(),
        "csharp.Apg.CsharpFrontend.Tests.Program.Main".into(),
    ));
    g.nodes.insert(
        SCAN_HEAD.into(),
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
    let language = |_: &str| Node {
        kind: NodeKind::Language,
        ..Node::default()
    };
    let mut g = Graph::default();
    // PHASE_09: the changed language's re-emitted hierarchy is rooted and
    // carries its `Language` root.
    g.nodes.insert("go".into(), language("go"));
    g.nodes.insert("go.godemo".into(), module("go.godemo"));
    g.nodes
        .insert("go.godemo/changed".into(), module("go.godemo/changed"));
    g.nodes
        .insert(changed.into(), located(NodeKind::File, changed, 1, 40));
    g.nodes.insert(
        "go.godemo.changed.S".into(),
        located(NodeKind::Struct, changed, 1, 40),
    );
    g.nodes.insert(
        "go.godemo.changed.S.f".into(),
        located(NodeKind::Function, changed, 2, 10),
    );
    g.nodes.insert(
        "go.godemo.changed.S.h".into(),
        located(NodeKind::Function, changed, 12, 20),
    );
    g.contains.insert(("go".into(), "go.godemo".into()));
    g.contains.insert(("go".into(), "go.godemo/changed".into()));
    g.contains
        .insert(("go.godemo".into(), "go.godemo/changed".into()));
    g.contains
        .insert(("go.godemo/changed".into(), changed.into()));
    g.contains
        .insert((changed.into(), "go.godemo.changed.S".into()));
    g.contains
        .insert((changed.into(), "go.godemo.changed.S.f".into()));
    g.contains
        .insert((changed.into(), "go.godemo.changed.S.h".into()));
    g.contains
        .insert(("go.godemo.changed.S".into(), "go.godemo.changed.S.f".into()));
    g.contains
        .insert(("go.godemo.changed.S".into(), "go.godemo.changed.S.h".into()));
    // Cached facts for the skipped language: the reused File's direct-parent
    // module only — NO `csharp.Apg`, NO `csharp.Apg.CsharpFrontend`, NO
    // Module->Module edges and NO `csharp` Language root (that scaffolding is
    // replayed from the store only when the language is actually skipped).
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend.Tests".into(),
        module("csharp.Apg.CsharpFrontend.Tests"),
    );
    g.nodes
        .insert(skipped.into(), located(NodeKind::File, skipped, 1, 20));
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend.Tests.Program".into(),
        located(NodeKind::Struct, skipped, 1, 20),
    );
    g.nodes.insert(
        "csharp.Apg.CsharpFrontend.Tests.Program.Main".into(),
        located(NodeKind::Function, skipped, 2, 10),
    );
    g.contains
        .insert(("csharp.Apg.CsharpFrontend.Tests".into(), skipped.into()));
    g.contains.insert((
        skipped.into(),
        "csharp.Apg.CsharpFrontend.Tests.Program".into(),
    ));
    g.contains.insert((
        skipped.into(),
        "csharp.Apg.CsharpFrontend.Tests.Program.Main".into(),
    ));
    g.contains.insert((
        "csharp.Apg.CsharpFrontend.Tests.Program".into(),
        "csharp.Apg.CsharpFrontend.Tests.Program.Main".into(),
    ));
    g.nodes.insert(
        SCAN_HEAD.into(),
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
        "go.godemo.changed.S.h".into(),
        located(NodeKind::Function, changed, 12, 20),
    );
    g.contains
        .insert((changed.into(), "go.godemo.changed.S.h".into()));
    g.contains
        .insert(("go.godemo.changed.S".into(), "go.godemo.changed.S.h".into()));
    g.nodes.insert(
        SCAN_HEAD.into(),
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
    let language = |_: &str| Node {
        kind: NodeKind::Language,
        ..Node::default()
    };
    let mut g = Graph::default();
    // PHASE_09: the ingestor roots every module/symbol FQN under the `java`
    // lang_switch id, and the stream carries its `Language` root.
    g.nodes.insert("java".into(), language("java"));
    for p in ["java.pkg", "java.pkg.a", "java.pkg.b", "java.pkg.c"] {
        g.nodes.insert(p.to_string(), module(p));
    }
    g.contains.insert(("java".into(), "java.pkg".into()));
    for p in ["java.pkg.a", "java.pkg.b", "java.pkg.c"] {
        g.contains.insert(("java".into(), p.to_string()));
    }
    for (pkg, file, ty) in [
        ("java.pkg.a", a, "A"),
        ("java.pkg.b", b, "B"),
        ("java.pkg.c", c, "C"),
    ] {
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
    for p in ["java.pkg.a", "java.pkg.b", "java.pkg.c"] {
        g.contains.insert(("java.pkg".to_string(), p.to_string()));
    }
    g.nodes.insert(
        SCAN_HEAD.into(),
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
        use apg::cache::{CacheKey, Manifest, ScanConfigKey};
        use apg::delta::ScanRecord;

        let dir = scratch("stale-seed");
        let prev_path = dir.join("db.lbug");
        let a = "/x/a.go".to_string();
        let b = "/x/b.go".to_string();
        let c = "/x/c.go".to_string();

        // This worktree's LOCAL DB: built by its last scan at content "oldkey".
        // It carries an authored/transient row (a requirement + its edge) so the
        // authored-identity guard has something to agree on.
        build_db(&prev_path, &with_authored_row(previous_graph(&a, &b, &c)));

        // The ASSEMBLED graph for this worktree's CURRENT tree, and its full
        // rebuild (the same graph loaded whole) — the correctness reference.
        // The assembled graph carries the SAME authored/transient rows as the
        // seed, so the content-key / cross-worktree staleness is the only
        // divergence this test exercises.
        let assembled = with_authored_row(assembled_graph(&a, &b));
        let assembled_authored = assembled_authored_identity(&assembled);
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
        // the full load, which is the correctness reference. The assembled
        // authored identity MATCHES the seed, so the refusal is unambiguously
        // the content-key staleness, not the authored guard.
        match seed_checked(
            &prev_path,
            recorded.content_key.as_deref(),
            &assembled_authored,
        ) {
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
        // IS the local DB's own key AND the seed represents the assembled
        // authored/transient rows.
        match seed_checked(&prev_path, Some("oldkey"), &assembled_authored) {
            SeedDecision::Seed(s) => s.discard().unwrap(),
            SeedDecision::FullLoad(f) => {
                panic!("a current local seed must still splice: {}", f.describe())
            }
        }

        // A missing key on either side is ineligible — equivalence cannot be
        // verified.
        assert!(matches!(
            seed_checked(&prev_path, None, &assembled_authored),
            SeedDecision::FullLoad(SeedFallback::StaleSeed { .. })
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spec-only change-set: the assembled graph carries the SAME code as the
    /// seed DB but DIFFERENT authored/transient rows (an extra `Requirement`).
    /// The win-C splice writes no authored row while `graph.jsonl` is serialized
    /// from the full assembled graph, so seeding this DB would publish a
    /// `db.lbug` that diverges from its own export. The phase-01 authored-identity
    /// guard must refuse the seed and hand the caller to the full load, which
    /// projects exactly the assembled authored/transient tables — so a spec-only
    /// splice can never diverge from a full rebuild.
    ///
    /// Pre-fix `seed_checked` had no authored-identity parameter; with a matching
    /// content key it returned `Seed`, so the step-3 assertion below (an
    /// `UnrepresentedAuthored` fallback) could not hold.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn spec_only_delta_cannot_diverge_from_full_rebuild() {
        let dir = scratch("spec-only-splice");
        let prev_path = dir.join("db.lbug");

        // The seed DB: fixture code + authored rows (`req`) + `Scan` row, built
        // through the same full load the scan path uses.
        let seed_graph = seeded_authored_graph(fixture_graph());
        build_db(&prev_path, &seed_graph);
        let seed_authored = seed_authored_identity(&prev_path).unwrap();

        // The assembled graph: the same code, one EXTRA authored requirement —
        // a spec-only delta whose authored rows the seed does not represent.
        let assembled = with_authored_delta(fixture_graph());
        let assembled_authored = assembled_authored_identity(&assembled);
        assert_ne!(
            seed_authored, assembled_authored,
            "the fixture must genuinely diverge in authored rows"
        );

        // (1) The guard refuses: the content key matches ("oldkey"), but the
        // seed does not represent the assembled authored rows, so the caller runs
        // the full load instead of publishing a diverged DB.
        match seed_checked(&prev_path, Some("oldkey"), &assembled_authored) {
            SeedDecision::FullLoad(SeedFallback::UnrepresentedAuthored {
                assembled: a,
                seed: s,
            }) => {
                assert_eq!(a, assembled_authored);
                assert_eq!(s, seed_authored);
            }
            SeedDecision::FullLoad(other) => {
                panic!("expected UnrepresentedAuthored, got: {}", other.describe())
            }
            SeedDecision::Seed(_) => panic!(
                "a spec-only delta must never seed: the spliced DB would diverge from its export"
            ),
        }

        // (2) Positive control: when the assembled authored rows DO match the
        // seed's, the guard is not over-broad — it still seeds.
        let represented = seeded_authored_graph(fixture_graph());
        let represented_authored = assembled_authored_identity(&represented);
        assert_eq!(represented_authored, seed_authored);
        match seed_checked(&prev_path, Some("oldkey"), &represented_authored) {
            SeedDecision::Seed(s) => s.discard().unwrap(),
            SeedDecision::FullLoad(f) => {
                panic!(
                    "the guard must not refuse a represented seed: {}",
                    f.describe()
                )
            }
        }

        // (3) The full-load reference: the guard's fallback loads the assembled
        // graph whole, so the resulting DB carries exactly the assembled
        // authored/transient rows — no divergence is possible.
        let full_path = dir.join("full.lbug");
        build_db(&full_path, &assembled);
        assert_eq!(
            seed_authored_identity(&full_path).unwrap(),
            assembled_authored,
            "the full load must project exactly the assembled authored rows"
        );
        let full_counts = {
            let db = Database::new(&full_path, SystemConfig::default().read_only(true)).unwrap();
            row_counts(&db)
        };
        assert_eq!(full_counts.get("Requirement"), Some(&2));
        assert_eq!(full_counts.get("Entity"), Some(&1));
        assert_eq!(full_counts.get("Drives"), Some(&2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The recovery proof for
    /// `requirements.requirement.freshness-fast-path-cannot-mask-divergence` (and
    /// the observable completion of feedback-3): a re-anchored, agreeing
    /// `Scan`-row pair cannot declare a DB that dropped an authored row fresh.
    ///
    /// 1. A scratch project worktree carries an authored `Requirement` under
    ///    `apg/layers/**`; a hermetic scan builds `db.lbug` + `graph.jsonl` in
    ///    sync with it.
    /// 2. A durable commit through a live-session save re-anchors the Scan
    ///    metadata to the current state, so the DB's own `Scan` row and the
    ///    export's recorded scan agree — the pre-fix trap.
    /// 3. Re-anchoring done, the authored row is dropped from `db.lbug` ONLY
    ///    (the tree still carries it), producing a DB the tree contradicts while
    ///    both Scan-row halves still name the current git state. The drop must
    ///    FOLLOW the save: a routed session mutation re-projects the whole
    ///    effective node set at admission, so an earlier drop would be healed.
    /// 4. `is_fresh(apg_root)` is FALSE — the DB's ACTUAL authored digest is
    ///    compared to the on-disk tree, never to the refreshed Scan row — so the
    ///    next scan does NOT fast-path: it rebuilds and restores the row.
    ///
    /// Pre-fix (before the authored/transient reconciliation) step 4's
    /// `!is_fresh` assertion fails: the re-anchored, agreeing Scan-row pair made
    /// the diverged DB look fresh and it was reused.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/db.lbug/graph.jsonl/git/live-session process); run via cargo test-e2e"]
    fn diverged_authored_rows_are_not_fast_pathed() {
        use apg::artifacts::ArtifactDb;
        use apg::testutil::{self, Repo};

        let repo = Repo::new("splice-diverged-authored");
        let wt = repo.start_project("foo");
        let apg_root = wt.join(apg::specs::LAYOUT);

        // 1. The checkout carries an authored Requirement. Commit it so the
        //    worktree is clean, then scan: `db.lbug` and `graph.jsonl` carry the
        //    authored row and the scan_meta records the clean HEAD.
        let req = apg::layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: "req".to_string(),
            body: "the authored row the divergence will drop".to_string(),
            properties: BTreeMap::new(),
            out: Vec::new(),
            in_edges: Vec::new(),
        };
        let req_path = apg::layers::node_file_path(
            &apg_root,
            apg::layers::Layer::Requirements,
            "requirement",
            "req",
        );
        apg::layers::write_node(&apg_root, &req).unwrap();
        testutil::wt_commit_paths(
            &wt,
            &["apg/layers/requirements/requirement/req.json"],
            "author requirement",
        );
        testutil::scan_checkout(&wt).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                db.has_node("requirements.requirement.req"),
                "the scan must project the authored row"
            );
        }
        assert!(
            apg::git::is_fresh(&apg_root),
            "a scan carrying an authored row must be fresh"
        );

        // 2. A durable commit through a live-session save re-anchors the Scan
        //    metadata to the current state: the session's own `Scan` row and the
        //    re-anchored graph.jsonl lead name the SAME (post-commit) state.
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);
        let add =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "req2"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            add.status.success(),
            "routed node add: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "session end: {}",
            String::from_utf8_lossy(&end.stderr)
        );
        let coord = session.child.wait_with_output().unwrap();
        assert!(
            coord.status.success(),
            "session process: {}",
            String::from_utf8_lossy(&coord.stderr)
        );
        assert!(
            !apg::session::live_session(&apg_root),
            "the session must have released the DB"
        );

        // The re-anchor landed: the DB's own Scan row and the export's recorded
        // scan agree on the current state — the pre-fix trap's agreeing pair.
        let db_scan = apg::git::db_recorded_scan(&apg_root).expect("DB Scan row");
        let rec_scan = apg::git::recorded_scan(&apg_root).expect("recorded scan");
        assert_eq!(
            (db_scan.sha, db_scan.clean, db_scan.content_key),
            (rec_scan.sha, rec_scan.clean, rec_scan.content_key),
            "the durability commit must leave the DB and export Scan rows agreeing"
        );

        // 3. Drop the authored `req` row from `db.lbug` ONLY — the tree still
        //    carries it. The re-anchored, agreeing Scan-row pair now hides the
        //    divergence from the pre-fix fast path.
        let deleted = testutil::detach_node(&apg_root, "requirements.requirement.req");
        assert_eq!(deleted, 1, "exactly one authored row must be dropped");
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                !db.has_node("requirements.requirement.req"),
                "the authored row must be gone from the DB"
            );
            assert!(
                db.has_node("requirements.requirement.req2"),
                "the durability commit's row must survive the drop"
            );
        }
        assert!(req_path.exists(), "the tree must still carry the row");

        // 4. The recovery safety net: the re-anchored Scan row cannot mask the
        //    dropped row — the DB's actual authored digest differs from the
        //    tree's, so the fast path is NOT fresh and the DB is not reused.
        assert!(
            !apg::git::is_fresh(&apg_root),
            "a diverged authored row must make the DB unfresh even with an agreeing, re-anchored Scan row"
        );

        // The next scan therefore does not fast-path; it rebuilds from the tree
        // and restores the dropped authored row in `db.lbug`.
        testutil::scan_checkout(&wt).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                db.has_node("requirements.requirement.req"),
                "the rebuilding scan must restore the dropped authored row"
            );
            assert!(db.has_node("requirements.requirement.req2"));
        }

        testutil::remove(&repo);
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
            "Module:csharp.Apg:",
            "Module:csharp.Apg.CsharpFrontend:",
            "Module:csharp.Apg.CsharpFrontend.Tests:",
            "Contains:csharp.Apg->csharp.Apg.CsharpFrontend",
            "Contains:csharp.Apg.CsharpFrontend->csharp.Apg.CsharpFrontend.Tests",
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
        use apg::cache::{CacheKey, FactStore, FileFragment, ModuleScaffolding, ScanConfigKey};
        use apg::ingest::{IngestOptions, Reuse, ingest_with_reuse};
        use apg::schema::Record;

        let dir = scratch("export-equiv");
        let changed = "go/changed.go".to_string();
        let skipped = "csharp/Tests.cs".to_string();
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
                base: None,
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
            "\"type\":\"module\",\"fqn\":\"csharp.Apg\"",
            "\"type\":\"module\",\"fqn\":\"csharp.Apg.CsharpFrontend\"",
            "\"type\":\"contains\",\"from\":\"csharp.Apg\",\"to\":\"csharp.Apg.CsharpFrontend\"",
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
        use apg::cache::{CacheKey, FactStore, FileFragment, ScanConfigKey};
        use apg::ingest::{IngestOptions, Reuse, ingest_with_reuse};
        use apg::schema::Record;

        let dir = scratch("java-targeted-export");
        let a = "java/pkg/a/A.java".to_string();
        let b = "java/pkg/b/B.java".to_string();
        let c = "java/pkg/c/C.java".to_string();
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
            base: None,
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
            assembled_jsonl.contains("\"type\":\"module\",\"fqn\":\"java.pkg.a\""),
            "the export must carry the unchanged package's module record"
        );
        assert!(
            assembled_jsonl
                .contains("\"type\":\"contains\",\"from\":\"java.pkg\",\"to\":\"java.pkg.a\""),
            "the export must carry the spawned stream's java.pkg -> java.pkg.a scaffolding:\n{assembled_jsonl}"
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
            before_jsonl.contains("\"type\":\"module\",\"fqn\":\"java.pkg.a\""),
            "the reused file's cached direct-parent module survives even pre-fix"
        );
        assert!(
            !before_jsonl
                .contains("\"type\":\"contains\",\"from\":\"java.pkg\",\"to\":\"java.pkg.a\""),
            "the pre-fix stream cannot carry java.pkg -> java.pkg.a:\n{before_jsonl}"
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

    /// phase-04 task-22 — the splice applies its delta in a BOUNDED number
    /// of DML statements that does NOT grow with the delta's node/edge
    /// count, while equivalence to a full rebuild is NOT weakened. The
    /// pre-fix per-row implementation issued one statement per node and per
    /// edge (`plan.note-87`: ~82 298 on the jgrapht scenario), so it fails
    /// the bound; the batched implementation reports a small constant, and
    /// the existing enumerated splice equivalence + rollback oracles stay
    /// green beside it.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn batched_dml_statement_count_is_bounded_and_size_independent() {
        // A small delta and a 4x-larger delta of the SAME shape: the batched
        // statement count is a function of the label/rel-pair set, not of the
        // row count.
        const BOUND: u64 = 64;
        let small = wide_splice(400);
        let large = wide_splice(1600);

        // (a) the falsifiable batching claim.
        for out in [&small, &large] {
            assert!(
                out.report.dml_statements <= BOUND,
                "the batched DML must stay within {BOUND} statements: {:?}",
                out.report
            );
            assert!(
                out.report.nodes_upserted + out.report.edges_merged >= 400,
                "the fixture must really exercise a wide delta: {:?}",
                out.report
            );
        }
        assert_eq!(
            small.report.dml_statements, large.report.dml_statements,
            "the statement count must not grow with the delta's row count: small={:?} large={:?}",
            small.report, large.report
        );
        assert!(
            large.report.nodes_upserted > small.report.nodes_upserted
                && large.report.edges_merged > small.report.edges_merged,
            "the large delta must actually carry more rows — otherwise the \
             equality above is vacuous: small={:?} large={:?}",
            small.report,
            large.report
        );

        // (b) equivalence is NOT weakened: the enumerated oracle runs beside
        // the count assertion — per-label/per-rel-type counts, the
        // UnresolvedTarget set by FQN WITH category, the refreshed Scan row,
        // and the published DB's full code snapshot (every node/rel) all
        // equal a full rebuild of the same assembled graph.
        for out in [&small, &large] {
            assert_eq!(
                out.spliced_counts, out.expected_counts,
                "every table's row count must equal a full rebuild"
            );
            assert_eq!(
                out.spliced_unres, out.expected_unres,
                "the unresolved set by (fqn, category) must equal a full rebuild"
            );
            assert_eq!(
                out.spliced_scan, out.expected_scan,
                "the refreshed Scan row must equal a full rebuild's"
            );
            assert_eq!(
                out.spliced_snapshot, out.expected_snapshot,
                "the full code snapshot (nodes + rels + Scan) must equal a full rebuild"
            );
            assert!(
                out.report.nodes_deleted > 0
                    && out.report.edges_deleted > 0
                    && out.report.unresolved_gc > 0,
                "the delta must exercise the delete + UnresolvedTarget-GC paths: {:?}",
                out.report
            );
        }

        // (c) the seed stays abandonable: `discard` removed the temp copy and
        // left the previous `db.lbug` byte-identical (the full-load fallback
        // path of task-4/task-1).
        assert!(
            small.previous_bytes_preserved && large.previous_bytes_preserved,
            "discard must leave the previous db.lbug byte-identical"
        );
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
            SCAN_HEAD.into(),
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
            SCAN_HEAD.into(),
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
        let brand: Vec<&(String, String)> = rows.iter().filter(|(f, _)| f == "ext.Brand").collect();
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
        let back = read_graph_jsonl(&export).unwrap();
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
            SCAN_HEAD.into(),
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
            SCAN_HEAD.into(),
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
