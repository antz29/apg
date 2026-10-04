mod common;

use apg::artifacts::*;
use apg::git;
use apg::graph::{Graph, Location, Node, NodeKind};
use apg::layers::{self, Layer, NodeProperties};
use apg::load;
use apg::schema::Record;
use apg::specs;
use apg::testutil::{self, Repo};
use common::wt_commit_paths;
use lbug::{Connection, Database};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A real project context (R4): a git repo whose worktree `foo` on branch
/// `foo` carries a real `apg/.trans/db.lbug` code graph plus a fresh
/// scan_meta. Returns `(wt_apg_root, repo, wt_root)`. Tags are
/// module-prefixed so parallel tests in other modules never collide on a
/// temp dir.
fn project_fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = Repo::new(&format!("artifacts-{name}"));
    let wt = repo.start_project("foo");
    db_at(&wt);
    testutil::write_scan_meta(
        &wt.join(specs::LAYOUT),
        Some(&repo.head_sha()),
        true,
        "2026-09-07T00:00:00Z",
    );
    (wt.join(specs::LAYOUT), repo, wt)
}

/// Builds a real DB + load files under `dir/apg` (used by `project_fixture`
/// — the git-aware fixtures init the repo around the worktree first).
fn db_at(dir: &Path) {
    std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
    std::fs::create_dir_all(dir.join("apg").join("specs")).unwrap();
    let mut g = Graph::default();
    g.nodes.insert(
        "github.com/x/y".to_string(),
        Node {
            kind: NodeKind::Module,
            ..Node::default()
        },
    );
    g.nodes.insert(
        "/abs/store.go".to_string(),
        Node {
            kind: NodeKind::File,
            location: Some(Location {
                path: "/abs/store.go".into(),
                start: 0,
                end: 0,
                start_line: 1,
                end_line: 100,
            }),
            code_type: "src".to_string(),
            ..Node::default()
        },
    );
    g.nodes.insert(
        "github.com/x/y.Store".to_string(),
        Node {
            kind: NodeKind::Struct,
            location: Some(Location {
                path: "/abs/store.go".into(),
                start: 0,
                end: 40,
                start_line: 1,
                end_line: 40,
            }),
            code_type: "src".to_string(),
            ..Node::default()
        },
    );
    g.contains
        .insert(("github.com/x/y".to_string(), "/abs/store.go".to_string()));
    g.contains.insert((
        "/abs/store.go".to_string(),
        "github.com/x/y.Store".to_string(),
    ));
    let ldir = dir.join("apg").join(specs::TRANS).join("load");
    std::fs::create_dir_all(&ldir).unwrap();
    load::build_load_files(&g, &ldir).unwrap();
    let db = Database::new(
        dir.join("apg").join(specs::TRANS).join("db.lbug"),
        Default::default(),
    )
    .unwrap();
    let conn = Connection::new(&db).unwrap();
    load::create_schema(&conn).unwrap();
    load::copy_from(&conn, &ldir).unwrap();
    drop(conn);
    drop(db);
}

/// The committed baseline records for the `foo` project: a transient plan
/// with one phase and one healthy note (Details → Plan). The funnel's
/// durable spec/note halves are gone — it now serves the `.trans` plan/
/// review writers only.
fn baseline_records() -> Vec<Record> {
    vec![
        Record::Plan {
            fqn: "foo/plan".into(),
            title: "Foo".into(),
            strategy: "G".into(),
        },
        Record::PlanPhase {
            fqn: "foo/plan.phase-01".into(),
            number: 1,
            title: "P1".into(),
            deliverable: "D".into(),
            status: "pending".into(),
        },
        Record::Contains {
            from: "foo/plan".into(),
            to: "foo/plan.phase-01".into(),
            properties: NodeProperties::default(),
        },
        Record::Note {
            fqn: "foo/note-1".into(),
            body: "first".into(),
            kind: "background".into(),
            properties: NodeProperties::default(),
        },
        Record::Details {
            from: "foo/note-1".into(),
            to: "foo/plan".into(),
            properties: NodeProperties::default(),
        },
    ]
}

/// Note nodes with no incident `Details` edge — the orphan residue a failed
/// write-through used to leave behind.
fn orphan_notes(db: &ArtifactDb) -> i64 {
    let total = count(&db.db, "MATCH (n:Note) RETURN count(*)");
    let with_edge = count(
        &db.db,
        "MATCH (n:Note)-[:Details]->() RETURN count(DISTINCT n)",
    );
    total - with_edge
}

/// The authored node set the phase-4 lossless round-trip pins.
///
/// One node of every durable type, each carrying arbitrary *unknown* property
/// keys and empty-valued keys; the `Entity` carries `kind = event` (a durable
/// key the `Entity` table has no dedicated column for, so only the
/// serialized-properties column can carry it); and edges — with their own
/// properties — of every durable authored kind. Every authored edge appears in
/// BOTH endpoint files with identical properties (the pairing invariant),
/// except `implemented-by`, whose code target (`go.fixture.mod.Store`, the
/// scanned struct `testutil::project_with_db` provides) has no node file to
/// hold the in half.
fn lossless_authored_nodes() -> Vec<layers::NodeFile> {
    use layers::{InEdge, NodeFile, OutEdge};

    fn props(pairs: &[(&str, &str)]) -> NodeProperties {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    fn out(kind: &str, target: &str, properties: NodeProperties) -> OutEdge {
        OutEdge {
            kind: kind.to_string(),
            target: target.to_string(),
            properties,
        }
    }
    fn inc(kind: &str, source: &str, properties: NodeProperties) -> InEdge {
        InEdge {
            kind: kind.to_string(),
            source: source.to_string(),
            properties,
        }
    }
    fn node(
        layer: &str,
        node_type: &str,
        name: &str,
        body: &str,
        properties: NodeProperties,
        out_edges: Vec<OutEdge>,
        in_edges: Vec<InEdge>,
    ) -> NodeFile {
        NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: body.to_string(),
            properties,
            out: out_edges,
            in_edges,
        }
    }

    let drives_props = props(&[("flavor", "context-map"), ("empty-prop", "")]);
    let calls_props = props(&[("protocol", "http"), ("empty-prop", "")]);
    let buyer_props = props(&[("role", "buyer")]);
    let approver_props = props(&[("role", "approver")]);
    let admin_props = props(&[("role", "admin")]);
    let impl_props = props(&[("version", "1"), ("empty-prop", "")]);

    vec![
        node(
            "requirements",
            "stakeholder",
            "customer",
            "the customer",
            props(&[("tier", "gold"), ("empty-meta", "")]),
            vec![out(
                "contains",
                "requirements.requirement.checkout",
                props(&[]),
            )],
            vec![],
        ),
        node(
            "requirements",
            "user",
            "shopper",
            "the shopper",
            props(&[("channel", "mobile"), ("empty-meta", "")]),
            vec![out(
                "represents",
                "domain.entity.order",
                buyer_props.clone(),
            )],
            vec![],
        ),
        node(
            "requirements",
            "requirement",
            "checkout",
            "the checkout flow",
            props(&[
                ("id", "REQ-1"),
                ("feature", "checkout"),
                ("owner", "team-x"),
                ("empty-meta", ""),
            ]),
            vec![
                out("drives", "domain.entity.order", drives_props.clone()),
                out("depends-on", "requirements.requirement.audit", props(&[])),
            ],
            vec![inc(
                "contains",
                "requirements.stakeholder.customer",
                props(&[]),
            )],
        ),
        node(
            "requirements",
            "requirement",
            "audit",
            "the audit flow",
            props(&[
                ("id", "REQ-2"),
                ("feature", ""),
                ("owner", ""),
                ("empty-meta", ""),
            ]),
            vec![],
            vec![inc(
                "depends-on",
                "requirements.requirement.checkout",
                props(&[]),
            )],
        ),
        node(
            "requirements",
            "note",
            "design",
            "a design note",
            props(&[("kind", "background"), ("tag", "d"), ("empty-meta", "")]),
            vec![out("details", "solution.container.api", props(&[]))],
            vec![],
        ),
        node(
            "requirements",
            "constraint",
            "local-rule",
            "the local rule",
            props(&[
                ("attaches-to", "requirements.requirement.checkout"),
                ("scope", "local"),
                ("empty-meta", ""),
            ]),
            vec![],
            vec![],
        ),
        node(
            "domain",
            "group",
            "core",
            "the core domain",
            props(&[
                ("attribute", "core"),
                ("root", "checkout"),
                ("owner-team", ""),
                ("empty-meta", ""),
            ]),
            vec![
                out("realised-by", "solution.system.platform", props(&[])),
                out("contains", "domain.value.cart", props(&[])),
            ],
            vec![],
        ),
        node(
            "domain",
            "entity",
            "order",
            "the order event",
            props(&[("kind", "event"), ("owner", ""), ("empty-meta", "")]),
            vec![
                out("realised-by", "solution.container.api", props(&[])),
                out(
                    "represents",
                    "solution.person.alice",
                    approver_props.clone(),
                ),
            ],
            vec![
                inc(
                    "drives",
                    "requirements.requirement.checkout",
                    drives_props.clone(),
                ),
                inc(
                    "represents",
                    "requirements.user.shopper",
                    buyer_props.clone(),
                ),
                inc(
                    "publishes",
                    "domain.service.billing",
                    props(&[("topic", "orders")]),
                ),
                inc(
                    "subscribes",
                    "domain.service.billing",
                    props(&[("topic", "orders")]),
                ),
            ],
        ),
        node(
            "domain",
            "value",
            "cart",
            "the cart",
            props(&[("unit", "currency"), ("empty-meta", "")]),
            vec![],
            vec![inc("contains", "domain.group.core", props(&[]))],
        ),
        node(
            "domain",
            "service",
            "billing",
            "the billing service",
            props(&[("runtime", "rust"), ("empty-meta", "")]),
            vec![
                out("calls", "domain.service.notify", calls_props.clone()),
                out(
                    "publishes",
                    "domain.entity.order",
                    props(&[("topic", "orders")]),
                ),
                out(
                    "subscribes",
                    "domain.entity.order",
                    props(&[("topic", "orders")]),
                ),
            ],
            vec![],
        ),
        node(
            "domain",
            "service",
            "notify",
            "the notify service",
            props(&[("runtime", "rust"), ("empty-meta", "")]),
            vec![],
            vec![inc("calls", "domain.service.billing", calls_props.clone())],
        ),
        node(
            "solution",
            "system",
            "platform",
            "the platform",
            props(&[("owner-team", ""), ("empty-meta", "")]),
            vec![out("contains", "solution.container.api", props(&[]))],
            vec![
                inc("realised-by", "domain.group.core", props(&[])),
                inc("uses", "solution.person.alice", admin_props.clone()),
            ],
        ),
        node(
            "solution",
            "container",
            "api",
            "the api container",
            props(&[("kind", "service"), ("owner", ""), ("empty-meta", "")]),
            vec![
                out("implemented-by", "go.fixture.mod.Store", impl_props.clone()),
                out("contains", "solution.component.worker", props(&[])),
            ],
            vec![
                inc("realised-by", "domain.entity.order", props(&[])),
                inc("contains", "solution.system.platform", props(&[])),
                inc("details", "requirements.note.design", props(&[])),
            ],
        ),
        node(
            "solution",
            "component",
            "worker",
            "the worker component",
            props(&[("runtime", "rust"), ("empty-meta", "")]),
            vec![],
            vec![inc("contains", "solution.container.api", props(&[]))],
        ),
        node(
            "solution",
            "person",
            "alice",
            "alice",
            props(&[("email", "a@x"), ("empty-meta", "")]),
            vec![out("uses", "solution.system.platform", admin_props.clone())],
            vec![inc(
                "represents",
                "domain.entity.order",
                approver_props.clone(),
            )],
        ),
    ]
}

/// The exact FQN delete set for `nodes` (`<layer>.<type>.<name>`) — the
/// touched-FQN set the session's admission computes (`LayersOverlay::touched_fqns`).
fn touched_fqns(nodes: &[layers::NodeFile]) -> BTreeSet<String> {
    nodes
        .iter()
        .map(|n| format!("{}.{}.{}", n.layer, n.node_type, n.name))
        .collect()
}

/// Normalize a node set the way `ArtifactDb::node_files_from_db` renders it:
/// nodes by `(layer, node_type, name)`, each node's out edges by
/// `(kind, target)` and in edges by `(kind, source)`.
fn normalize_node_files(mut nodes: Vec<layers::NodeFile>) -> Vec<layers::NodeFile> {
    for node in &mut nodes {
        node.out
            .sort_by(|a, b| (&a.kind, &a.target).cmp(&(&b.kind, &b.target)));
        node.in_edges
            .sort_by(|a, b| (&a.kind, &a.source).cmp(&(&b.kind, &b.source)));
    }
    nodes.sort_by(|a, b| (&a.layer, &a.node_type, &a.name).cmp(&(&b.layer, &b.node_type, &b.name)));
    nodes
}

/// `node_files_from_db` must reconstruct `expected` exactly: bodies, every
/// property key (arbitrary, empty-valued, and an `Entity`'s `kind`), and both
/// edge halves with their properties.
fn assert_lossless_round_trip(reconstructed: &[layers::NodeFile], expected: &[layers::NodeFile]) {
    let expected = normalize_node_files(expected.to_vec());
    assert_eq!(
        reconstructed,
        expected.as_slice(),
        "node_files_from_db must round-trip the authored node set exactly"
    );
}

/// The DB-only serialized-properties column must never leak into `graph.jsonl`:
/// every export line keeps the `Export` shape, and the export round-trips
/// through `read_graph_jsonl` → `write_graph_jsonl` (line-set equality —
/// `Graph`'s maps are unordered, so the scan's line order is not stable).
fn assert_graph_jsonl_consistent(graph_path: &Path) {
    let text = std::fs::read_to_string(graph_path).unwrap();
    for line in text.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            value.get("properties").is_none(),
            "graph.jsonl must not carry the DB-only serialized-properties column: {line}"
        );
    }
    let graph = testutil::read_graph_jsonl(graph_path).unwrap();
    let round_trip = graph_path.with_extension("roundtrip.jsonl");
    load::write_graph_jsonl(&graph, &round_trip).unwrap();
    let canonical = |p: &Path| -> Vec<String> {
        let mut lines: Vec<String> = std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(
        canonical(graph_path),
        canonical(&round_trip),
        "graph.jsonl must round-trip through read_graph_jsonl"
    );
    std::fs::remove_file(&round_trip).unwrap();
}

/// e2e tier -- real I/O: every test here builds a real project context
/// (git worktree + `db.lbug`), writes node files/JSONL on disk or spawns the
/// `apg` binary. Each is `#[ignore]`d, so a plain `cargo test` never runs
/// one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn illegal_details_pair_is_projected_away_not_a_binder_error() {
        let (apg_root, repo, _wt) = project_fixture("orphan");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();

        // A healthy committed state, write-through.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("foo/plan"));
            assert!(db.has_node("foo/note-1"));
            assert_eq!(orphan_notes(&db), 0);
        }

        // A note whose Details edge targets another Note — a pair the Details
        // rel table does NOT declare. The schema-pair guard projects the
        // illegal pair away (the same bucketing the scan load path applies),
        // so the write-through succeeds and the DB projection matches what a
        // fresh scan would produce (the note node lands, the impossible edge
        // never materializes).
        let mut mutated = baseline.clone();
        mutated.push(Record::Note {
            fqn: "foo/note-2".into(),
            body: "poison".into(),
            kind: "background".into(),
            properties: NodeProperties::default(),
        });
        mutated.push(Record::Details {
            from: "foo/note-2".into(),
            to: "foo/note-1".into(),
            properties: NodeProperties::default(),
        });
        write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap();

        // The JSONL committed (the note record is durable truth).
        assert_eq!(specs::read_jsonl(&path).unwrap(), mutated);
        // No temp residue next to the committed JSONL.
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        // The DB projection: the note node lands, the undeclared Details edge
        // does not (identical to the scan load path's pair bucketing).
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/note-2"));
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'foo/note-2'})-[:Details]->() RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("0"),
            "illegal pair must be projected away: {out}"
        );
        // The prior healthy edge survived the re-merge.
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'foo/note-1'})-[:Details]->(p:Plan) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "healthy edge: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn write_through_commits_jsonl_and_db() {
        let (apg_root, repo, _wt) = project_fixture("commit");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();

        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        // The JSONL matches the new records (a first write lands in the DB too,
        // even though the file did not exist before this call).
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/plan"));
        assert!(db.has_node("foo/plan.phase-01"));
        assert!(db.has_node("foo/note-1"));
        assert_eq!(orphan_notes(&db), 0);
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note)-[:Details]->(p:Plan) RETURN p.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/plan"), "details edge: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    /// The plan re-merge write-through (`node_merge` → `MERGE SET`) carries the
    /// Task verb projection too: a re-ingested Task record lands its
    /// verb/target/new_fqn in the live DB, so the suite tools see the same
    /// columns the scan load path projects.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn write_through_projects_task_verb_fields() {
        let (apg_root, repo, _wt) = project_fixture("task-verb");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "Foo".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: String::new(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
                properties: NodeProperties::default(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "Rename".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "renames".into(),
                target: "foo.Old".into(),
                new_fqn: "foo.New".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
                properties: NodeProperties::default(),
            },
        ];

        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        let db = ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .q("MATCH (t:Task) RETURN t.fqn, t.verb, t.target, t.new_fqn")
            .unwrap();
        assert!(
            out.contains("foo/plan.phase-01.task-1")
                && out.contains("renames")
                && out.contains("foo.Old")
                && out.contains("foo.New"),
            "write-through task verb projection: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn write_through_without_db_writes_jsonl() {
        let (apg_root, repo, _wt) = project_fixture("nodb");
        std::fs::remove_file(apg_root.join(specs::TRANS).join("db.lbug")).unwrap();
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();

        // No scan yet: the JSONL is the durable form; no re-ingest is attempted.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        assert!(!path.as_os_str().to_string_lossy().ends_with(".tmp"));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn stale_db_refuses_mutation_before_any_jsonl_write() {
        let (apg_root, repo, wt) = project_fixture("stale");
        // The DB records a clean scan at the first commit; then the branch
        // tree moves on to a second commit: stale.
        std::fs::write(wt.join("extra.txt"), "x").unwrap();
        wt_commit_paths(&wt, &["extra.txt"], "second");
        assert!(git::is_stale(&apg_root));

        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("graph is stale"), "refusal message: {msg}");
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");

        // The durable JSONL was never written (no partial mutation), and no
        // temp residue sits next to where it would have gone.
        assert!(!path.exists(), "refused mutation must not write JSONL");
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn fresh_git_db_allows_write_through() {
        let (apg_root, repo, _wt) = project_fixture("fresh");
        // The recorded scan matches the current tree exactly → fresh.
        assert!(!git::is_stale(&apg_root));

        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        // The write-through landed in both the durable JSONL and the live DB.
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/plan"));
        assert!(db.has_node("foo/note-1"));
        assert_eq!(orphan_notes(&db), 0);
        drop(db);

        testutil::remove(&repo);
    }

    /// Phase-02 task-8: `code_universes_from_export` classifies Real
    /// (graph.jsonl) / Planned (plan store) / absent with NO `db.lbug` open.
    ///
    /// The fixture deliberately writes a BOGUS `db.lbug` (not a database): if
    /// the resolver opened it, decoding would fail — a passed test proves the
    /// DB was never touched. A declared-but-unscanned planned FQN (the
    /// `apg.session.Coordinator` example) classifies Planned, never drift.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn code_universes_from_export_classifies_real_planned_absent_without_db() {
        let dir = std::env::temp_dir().join(format!("apg-cu-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trans = dir.join(specs::TRANS);
        std::fs::create_dir_all(trans.join("plans")).unwrap();

        // graph.jsonl: a scan_meta lead, two real code nodes, and one node
        // already projected with `status: planned` (a planned declaration that
        // a scan carried through).
        let graph = [
        r#"{"type":"scan_meta","git_sha":"abc","git_clean":true,"scanned_at":"2026-09-07T00:00:00Z"}"#,
        r#"{"type":"module","fqn":"github.com/x/y"}"#,
        r#"{"type":"struct","fqn":"github.com/x/y.Store","path":"/abs/store.go","start":0,"end":1,"start_line":1,"end_line":1,"code_type":"src"}"#,
        r#"{"type":"function","fqn":"github.com/x/y.plannedFn","path":"/abs/store.go","start":0,"end":1,"start_line":1,"end_line":1,"code_type":"src","status":"planned"}"#,
    ]
    .join("\n");
        std::fs::write(trans.join("graph.jsonl"), format!("{graph}\n")).unwrap();

        // The plan store declares a FQN that has not been scanned yet.
        specs::write_jsonl(
            &trans.join("plans").join("foo.jsonl"),
            &[Record::PlannedNode {
                fqn: "apg.session.Coordinator".into(),
                kind: "struct".into(),
                name: "Coordinator".into(),
                parent: "apg.session".into(),
            }],
        )
        .unwrap();

        // A bogus DB — never opened by the resolver.
        std::fs::write(trans.join("db.lbug"), b"this is definitely not a db").unwrap();

        let (scanned, planned) = code_universes_from_export(&dir).unwrap();
        assert!(scanned.contains("github.com/x/y"));
        assert!(scanned.contains("github.com/x/y.Store"));
        assert!(
            !scanned.contains("github.com/x/y.plannedFn"),
            "a status:planned export record is not real code"
        );
        assert!(planned.contains("github.com/x/y.plannedFn"));
        assert!(planned.contains("apg.session.Coordinator"));

        // The three-way classification: Real / Planned (never Drift) / Drift.
        assert_eq!(
            apg::layers::classify_code_ref("github.com/x/y.Store", &scanned, &planned),
            apg::layers::CodeRefStatus::Real
        );
        assert_eq!(
            apg::layers::classify_code_ref("apg.session.Coordinator", &scanned, &planned),
            apg::layers::CodeRefStatus::Pending
        );
        assert_eq!(
            apg::layers::classify_code_ref("apg.gone.Nope", &scanned, &planned),
            apg::layers::CodeRefStatus::Drift
        );

        // graph.jsonl absent: the export contributes nothing; the plan store's
        // planned FQNs remain — the caller's graph.jsonl gate decides whether
        // code-FQN refs are validated at all.
        std::fs::remove_file(trans.join("graph.jsonl")).unwrap();
        let (scanned2, planned2) = code_universes_from_export(&dir).unwrap();
        assert!(scanned2.is_empty(), "no export ⇒ no scanned universe");
        assert!(planned2.contains("apg.session.Coordinator"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase-01 task-19: a routed durable write through a live session is
    /// projected into the session-held `db.lbug` IMMEDIATELY — a separate
    /// routed reader sees the mutation before any save — but it is BUFFERED,
    /// not durable. `apg/layers/**` stays untouched and no commit lands until
    /// `apg session save`. Save is the single durability point: it writes the
    /// buffered node file(s) and lands exactly ONE commit, then clears the
    /// buffer (a second save over the now-clean buffer creates no commit). The
    /// session stays live throughout and ends cleanly.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn session_routed_write_is_committed_once_and_immediately_projected() {
        let (wt_apg, repo, wt) = project_fixture("session-routed-buffer");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        let before = testutil::commit_count(&wt);

        // The routed mutation is admitted into the live session's write-back
        // buffer and projected into the session-held DB — but it is NOT durable
        // yet: the node file is absent and no commit landed.
        let add =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "buffered"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        let node_file =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "buffered");
        assert!(
            !node_file.exists(),
            "a routed write must be buffered, not written to apg/layers/** before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before,
            "a buffered routed write must not commit before save"
        );

        // (a) Immediately projected: a NEW routed reader (separate process)
        // sees the still-unsaved mutation BEFORE save/end.
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.buffered'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the routed write must be queryable immediately, before save"
        );

        // (b) The single durability point: save writes the buffered node file
        // and lands exactly ONE commit.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            node_file.exists(),
            "save must write the buffered node file to apg/layers/**"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before + 1,
            "the whole buffered set must land in exactly one commit"
        );

        // The buffer is cleared: a second save over the now-clean buffer is a
        // no-op — no further commit.
        let resave = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            resave.status.success(),
            "{}",
            String::from_utf8_lossy(&resave.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before + 1,
            "a save over the cleared buffer must make no commit"
        );

        // The session stayed live through save, then ends cleanly.
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-7: pin the three planned-node states against the exact-FQN
    /// projection delta.
    ///
    /// (1) A planned-only FQN with NO `<project>/` prefix disappears immediately
    /// when the plan's planned declaration is removed, and the code graph is
    /// otherwise untouched.
    /// (2) That FQN AFTER a scan realized it — the delete is guarded by
    /// `status = 'planned'`, so the REAL code node and its incident edges survive
    /// removing the plan's placeholder declaration.
    /// (3) An `implemented-by` to a declared-but-unscanned planned FQN stays
    /// Pending (never drift) once `code_universes_from_export` reads the plan
    /// store.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn planned_fqn_states_reflect_and_guard_realized_code() {
        let (apg_root, repo, _wt) = project_fixture("planned-states");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let planned = |fqn: &str, kind: &str, name: &str, parent: &str| Record::PlannedNode {
            fqn: fqn.to_string(),
            kind: kind.to_string(),
            name: name.to_string(),
            parent: parent.to_string(),
        };

        // The code-graph baseline: the real struct + the File→Struct Contains
        // edge. It must be untouched by every planned-declaration mutation.
        let code_contains = {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("github.com/x/y.Store"));
            count(
                &db.db,
                "MATCH (:File)-[:Contains]->(:Struct) RETURN count(*)",
            )
        };
        assert_eq!(code_contains, 1);

        // (1) A planned-only FQN with no project prefix.
        write_jsonl_and_reingest(
            &apg_root,
            &path,
            "foo",
            &[planned(
                "apg.session.Coordinator.start",
                "function",
                "start",
                "apg.session",
            )],
        )
        .unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("apg.session.Coordinator.start"));
            assert!(db.is_planned("apg.session.Coordinator.start"));
        }

        // Removing the declaration drops the un-prefixed planned FQN at once and
        // leaves the code graph alone.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &[]).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                !db.has_node("apg.session.Coordinator.start"),
                "a removed planned FQN without a project prefix must disappear immediately"
            );
            assert!(db.has_node("github.com/x/y.Store"));
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:File)-[:Contains]->(:Struct) RETURN count(*)"
                ),
                code_contains,
                "the code graph must be otherwise untouched"
            );
        }

        // (2) The FQN AFTER a scan realized it: declare a planned node at the
        // REAL struct's FQN. `merge_records` skips the placeholder (real code
        // wins), and removing the declaration must NOT detach the real node —
        // the status='planned' guard.
        write_jsonl_and_reingest(
            &apg_root,
            &path,
            "foo",
            &[planned(
                "github.com/x/y.Store",
                "struct",
                "Store",
                "github.com/x/y",
            )],
        )
        .unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("github.com/x/y.Store"));
            assert!(
                !db.is_planned("github.com/x/y.Store"),
                "the realized code node must keep status NULL"
            );
        }
        write_jsonl_and_reingest(&apg_root, &path, "foo", &[]).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                db.has_node("github.com/x/y.Store"),
                "removing a planned declaration must never drop the realized code node"
            );
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:File)-[:Contains]->(:Struct) RETURN count(*)"
                ),
                1,
                "the realized code node's incident edges must survive the guarded delete"
            );
        }

        // (3) An implemented-by to a declared-but-unscanned planned FQN is
        // Pending, never Drift — resolved from the plan store with no scan.
        let candidate = "apg.session.Coordinator.wait";
        write_jsonl_and_reingest(
            &apg_root,
            &path,
            "foo",
            &[planned(candidate, "function", "wait", "apg.session")],
        )
        .unwrap();
        let (scanned, planned_fqns) = code_universes_from_export(&apg_root).unwrap();
        assert_eq!(
            apg::layers::classify_code_ref(candidate, &scanned, &planned_fqns),
            apg::layers::CodeRefStatus::Pending,
            "a declared-but-unscanned planned FQN is pending, never drift"
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-8: the projection delta converges with no residue —
    /// add-then-remove and remove-then-add of the same node both converge, with
    /// no duplicate/stale rows and no orphan edges.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn projection_delta_converges_without_residue() {
        let (apg_root, repo, _wt) = project_fixture("delta-converge");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();

        let with_note2 = {
            let mut r = baseline.clone();
            r.push(Record::Note {
                fqn: "foo/note-2".into(),
                body: "second".into(),
                kind: "background".into(),
                properties: NodeProperties::default(),
            });
            r.push(Record::Details {
                from: "foo/note-2".into(),
                to: "foo/plan".into(),
                properties: NodeProperties::default(),
            });
            r
        };

        // Add: exactly one row and one edge.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &with_note2).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                1
            );
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:Note {fqn: 'foo/note-2'})-[:Details]->(:Plan) RETURN count(*)"
                ),
                1
            );
            assert_eq!(orphan_notes(&db), 0);
        }

        // A changed node (its body) is re-projected without duplicating rows or
        // dropping/re-adding its edges twice.
        let changed = {
            let mut r = with_note2.clone();
            for x in &mut r {
                if let Record::Note { fqn, body, .. } = x
                    && fqn == "foo/note-1"
                {
                    *body = "first (edited)".into();
                }
            }
            r
        };
        write_jsonl_and_reingest(&apg_root, &path, "foo", &changed).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-1'}) RETURN count(*)"),
                1,
                "a changed node must not leave a stale duplicate row"
            );
            assert_eq!(
                count(&db.db, "MATCH ()-[:Details]->() RETURN count(*)"),
                2,
                "both Details edges survive the changed-node re-merge"
            );
        }

        // Remove: the node and its edge are gone, no orphan/duplicate residue.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                0,
                "a removed node must not linger"
            );
            assert_eq!(
                count(&db.db, "MATCH ()-[:Details]->() RETURN count(*)"),
                1,
                "the removed node's edge must not linger"
            );
            assert_eq!(orphan_notes(&db), 0);
        }

        // Remove-then-add: re-adding converges to exactly one row/edge.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &with_note2).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                1,
                "remove-then-add must not duplicate the row"
            );
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:Note {fqn: 'foo/note-2'})-[:Details]->(:Plan) RETURN count(*)"
                ),
                1,
                "remove-then-add must not duplicate the edge"
            );
            assert_eq!(orphan_notes(&db), 0);
        }

        testutil::remove(&repo);
    }

    /// Phase-05 task-10 (int): a forced mid-apply re-ingest failure rolls the
    /// projection back to the prior state and the mutation reports failure. The
    /// durable file write landed FIRST (commit-then-project), so the committed
    /// JSONL holds the new state while the projection stays prior — the DB is
    /// deliberately behind the tree, which the corrected staleness model refuses
    /// until the DB is resynced; once level again, the committed state is
    /// reproducible.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn projection_apply_failure_rolls_back_and_reports() {
        let (apg_root, repo, _wt) = project_fixture("projection-rollback");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();

        let mut mutated = baseline.clone();
        mutated.push(Record::Note {
            fqn: "foo/note-2".into(),
            body: "second".into(),
            kind: "background".into(),
            properties: NodeProperties::default(),
        });
        mutated.push(Record::Details {
            from: "foo/note-2".into(),
            to: "foo/plan".into(),
            properties: NodeProperties::default(),
        });

        install_projection_hook(|| anyhow::bail!("forced mid-apply re-ingest failure"));
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap_err();
        assert!(format!("{err:#}").contains("forced mid-apply"), "{err:#}");

        // The projection rolled back to the prior state.
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                0,
                "the failed mutation must not leave a projected row"
            );
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-1'}) RETURN count(*)"),
                1,
                "the prior projection must survive the rollback"
            );
            assert_eq!(orphan_notes(&db), 0);
        }
        // The durable write landed FIRST: the committed JSONL is the new state.
        assert_eq!(specs::read_jsonl(&path).unwrap(), mutated);
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        // Commit-then-project deliberately leaves the DB behind the committed
        // JSONL after the rollback, and the corrected staleness model now REFUSES
        // the next write-through: `is_fresh` compares the DB's actual
        // authored/transient digest to `tree_authored_identity` (the independent
        // source of truth), so the behind-the-tree DB can never be reused.
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap_err();
        assert!(
            format!("{err:#}").contains("stale"),
            "the behind-the-tree DB must refuse the next write: {err:#}"
        );

        // Resync the fixture DB to the committed JSONL — the projection the
        // rolled-back attempt would have applied — so the tree and DB agree
        // again, then assert the failure was one-shot: the next apply reproduces
        // the committed state.
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            let conn = db.conn().unwrap();
            conn.query("BEGIN TRANSACTION").unwrap();
            db.merge_records(&conn, &mutated).unwrap();
            conn.query("COMMIT").unwrap();
        }
        write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                1,
                "the committed state must be reproducible"
            );
        }

        testutil::remove(&repo);
    }

    /// Phase-04 task-3 (acceptance): the next scan rebuilds `db.lbug` from
    /// source and every metadata mutation stays visible in the query index.
    ///
    /// The mutation is projected write-through first (immediately queryable),
    /// then the index is deleted and the real post-code scan leg re-run: the
    /// rebuilt DB is a fresh file carrying both the scanned code and the
    /// durable node-file mutation — no re-scan is needed for the metadata, and
    /// the scan never loses it.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn acceptance_next_scan_rebuilds_db_from_source_and_keeps_mutations_visible() {
        use std::os::unix::fs::MetadataExt;

        let (repo, wt, wt_apg) = testutil::project_with_db("accept-scan-rebuild");

        // A durable mutation lands write-through: immediately queryable with no
        // scan.
        layers::write_project(
            &wt_apg,
            &[layers::NodeFile {
                layer: "requirements".to_string(),
                node_type: "requirement".to_string(),
                name: "rebuilt".to_string(),
                body: "survives the rebuild".to_string(),
                properties: std::collections::BTreeMap::new(),
                out: Vec::new(),
                in_edges: Vec::new(),
            }],
            &[],
        )
        .unwrap();
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.requirement.rebuilt"));
            // PHASE_09: the hermetic scan roots the frontend-emitted module
            // identity under its lang_switch id (`go` — testutil::scan_checkout),
            // so the scanned FQN is `go.fixture.mod.Store`.
            assert!(db.has_node("go.fixture.mod.Store"));
        }

        // The next scan REBUILDS `db.lbug` from source: remove the index (a
        // scan unlinks and recreates it) and run the real post-code scan leg.
        let db_path = wt_apg.join(specs::TRANS).join("db.lbug");
        let inode_before = std::fs::metadata(&db_path).unwrap().ino();
        std::fs::remove_file(&db_path).unwrap();
        testutil::scan_checkout(&wt).unwrap();
        let inode_after = std::fs::metadata(&db_path).unwrap().ino();
        assert_ne!(
            inode_before, inode_after,
            "the scan must rebuild db.lbug as a fresh file"
        );

        // Every mutation remains visible in the rebuilt index, alongside the
        // freshly scanned code.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(
            db.has_node("requirements.requirement.rebuilt"),
            "the metadata mutation must survive the scan rebuild"
        );
        assert!(
            db.has_node("go.fixture.mod.Store"),
            "the scanned code must be rebuilt from source"
        );
        drop(db);

        testutil::remove(&repo);
    }

    /// Phase-01 task-5: `ArtifactDb::node_files_from_db` reconstructs the
    /// durable node-file set from a REAL `db.lbug` — the metadata nodes were
    /// written as node files, projected into the DB by a real scan (the
    /// canonical `layers::ingest_tree` → `create_schema`/`build_load_files`
    /// path), and read back.
    ///
    /// This pins the subset the CURRENT columns carry: `body`, the projected
    /// typed property keys (`Requirement` `id`/`feature`, `Note` `kind`,
    /// `Container` `kind`, `Group` `attribute`/`root`, `Constraint`
    /// `attaches-to` → the `attaches-to` property), and BOTH halves of every
    /// durable authored edge. Phase 4 added the serialized-properties column, so
    /// the authored `drives` edge property now round-trips on both halves; the
    /// exact full-fidelity round-trip (arbitrary keys, `Entity` `kind`, the
    /// session's incremental merge path) is pinned by phase 4's own e2e task.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn node_files_from_db_reconstructs_the_projected_columns() {
        let (repo, wt, wt_apg) = testutil::project_with_db("node-files-from-db");

        let props = |pairs: &[(&str, &str)]| -> layers::NodeProperties {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let out_edge =
            |kind: &str, target: &str, properties: layers::NodeProperties| layers::OutEdge {
                kind: kind.to_string(),
                target: target.to_string(),
                properties,
            };
        let in_edge =
            |kind: &str, source: &str, properties: layers::NodeProperties| layers::InEdge {
                kind: kind.to_string(),
                source: source.to_string(),
                properties,
            };

        // The `drives` edge carries an authored edge property on BOTH halves
        // (the pairing invariant demands identical properties); the current
        // schema has no rel property column, so the reconstruction must drop it.
        let drives_props = || props(&[("flavor", "context-map")]);

        // Each node carries exactly the typed metadata keys the schema projects.
        let requirement = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: "checkout".to_string(),
            body: "the checkout flow".to_string(),
            properties: props(&[("id", "REQ-1"), ("feature", "feature-checkout")]),
            out: vec![out_edge("drives", "domain.group.core", drives_props())],
            in_edges: vec![in_edge("details", "requirements.note.note-1", props(&[]))],
        };
        let note = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "note".to_string(),
            name: "note-1".to_string(),
            body: "a note".to_string(),
            properties: props(&[("kind", "background")]),
            out: vec![out_edge(
                "details",
                "requirements.requirement.checkout",
                props(&[]),
            )],
            in_edges: Vec::new(),
        };
        let group = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "group".to_string(),
            name: "core".to_string(),
            body: "the core domain".to_string(),
            properties: props(&[("attribute", "core"), ("root", "checkout")]),
            out: vec![out_edge(
                "realised-by",
                "solution.container.api",
                props(&[]),
            )],
            in_edges: vec![in_edge(
                "drives",
                "requirements.requirement.checkout",
                drives_props(),
            )],
        };
        let container = layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "container".to_string(),
            name: "api".to_string(),
            body: "the api service".to_string(),
            properties: props(&[("kind", "service")]),
            // A code endpoint: the target has no node file, so only this
            // source-side `out` half exists.
            out: vec![out_edge(
                "implemented-by",
                "go.fixture.mod.Store",
                props(&[]),
            )],
            in_edges: vec![in_edge("realised-by", "domain.group.core", props(&[]))],
        };
        let constraint = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "constraint".to_string(),
            name: "local-rule".to_string(),
            body: "the local rule".to_string(),
            properties: props(&[("attaches-to", "requirements.requirement.checkout")]),
            out: Vec::new(),
            in_edges: Vec::new(),
        };

        for node in [&requirement, &note, &group, &container, &constraint] {
            layers::write_node(&wt_apg, node).unwrap();
        }
        // Project the durable tree into a real db.lbug through the real scan
        // path (the canonical schema/load projection).
        testutil::scan_checkout(&wt).unwrap();

        let db = ArtifactDb::open(&wt_apg).unwrap();
        let nodes = db.node_files_from_db().unwrap();
        drop(db);

        // Only the five durable authored nodes are reconstructed — the scanned
        // code and the transient rows are not durable node files.
        assert_eq!(nodes.len(), 5, "durable node files: {nodes:#?}");

        let find = |layer: &str, node_type: &str, name: &str| -> layers::NodeFile {
            nodes
                .iter()
                .find(|n| n.layer == layer && n.node_type == node_type && n.name == name)
                .cloned()
                .unwrap_or_else(|| {
                    panic!("node_files_from_db is missing `{layer}.{node_type}.{name}`")
                })
        };
        let out = |n: &layers::NodeFile, kind: &str, target: &str| -> layers::OutEdge {
            n.out
                .iter()
                .find(|e| e.kind == kind && e.target == target)
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "missing out edge `{kind}` -> `{target}` on `{}.{}.{}`",
                        n.layer, n.node_type, n.name
                    )
                })
        };
        let incoming = |n: &layers::NodeFile, kind: &str, source: &str| -> layers::InEdge {
            n.in_edges
                .iter()
                .find(|e| e.kind == kind && e.source == source)
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "missing in edge `{source}` -{kind}-> on `{}.{}.{}`",
                        n.layer, n.node_type, n.name
                    )
                })
        };

        // Body + the projected typed property keys, per node type.
        let r = find("requirements", "requirement", "checkout");
        assert_eq!(r.body, "the checkout flow");
        assert_eq!(
            r.properties,
            props(&[("id", "REQ-1"), ("feature", "feature-checkout")])
        );

        let n = find("requirements", "note", "note-1");
        assert_eq!(n.body, "a note");
        assert_eq!(n.properties, props(&[("kind", "background")]));

        let g = find("domain", "group", "core");
        assert_eq!(g.body, "the core domain");
        assert_eq!(
            g.properties,
            props(&[("attribute", "core"), ("root", "checkout")])
        );

        let c = find("solution", "container", "api");
        assert_eq!(c.body, "the api service");
        assert_eq!(c.properties, props(&[("kind", "service")]));

        let k = find("requirements", "constraint", "local-rule");
        assert_eq!(k.body, "the local rule");
        assert_eq!(
            k.properties,
            props(&[("attaches-to", "requirements.requirement.checkout")])
        );

        // Both halves of `drives`, with the authored edge property intact: the
        // serialized-properties column carries it, and it feeds both halves.
        let drives_out = out(&r, "drives", "domain.group.core");
        assert_eq!(
            drives_out.properties,
            drives_props(),
            "the serialized-properties column round-trips an edge property: {drives_out:?}"
        );
        let drives_in = incoming(&g, "drives", "requirements.requirement.checkout");
        assert_eq!(
            drives_in.properties,
            drives_props(),
            "the serialized-properties column round-trips an edge property: {drives_in:?}"
        );

        // Both halves of `realised-by`.
        out(&g, "realised-by", "solution.container.api");
        incoming(&c, "realised-by", "domain.group.core");

        // `implemented-by`: only the source's out half (the code endpoint has no
        // node file to hold the in half).
        let implemented_out = out(&c, "implemented-by", "go.fixture.mod.Store");
        assert!(implemented_out.properties.is_empty());

        // Both halves of `details`.
        out(&n, "details", "requirements.requirement.checkout");
        incoming(&r, "details", "requirements.note.note-1");

        testutil::remove(&repo);
    }

    /// Phase-01 task-6: `ArtifactDb::code_universes_from_db` reads the scanned
    /// and planned code-FQN universes from a REAL `db.lbug` — a hermetic scan of
    /// real source (`testutil::project_with_db`, the canonical code payload)
    /// plus a plan-writer-authored planned Implementation node the scan
    /// projects as a `status = planned` code row — and those sets equal the ones
    /// `artifacts::code_universes_from_export` derives from the SAME scan's
    /// `graph.jsonl` (plus the plan store). The DB-side twin lets session
    /// admission validate `implemented-by` refs with no `graph.jsonl` read.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn code_universes_from_db_match_the_export_universes_for_a_real_scan() {
        let (repo, wt, wt_apg) = testutil::project_with_db("code-universes-from-db");

        // A planned Implementation node declared in the plan store: the next
        // scan projects it as a `status = planned` Struct row into both
        // `db.lbug` and `graph.jsonl`. `parent` is the scanned module FQN, so
        // the Contains edge resolves.
        let planned_fqn = "go.fixture.mod.Future";
        specs::write_jsonl(
            &specs::plan_jsonl_path(&wt_apg, "foo"),
            &[Record::PlannedNode {
                fqn: planned_fqn.to_string(),
                kind: "struct".to_string(),
                name: "Future".to_string(),
                parent: "go.fixture.mod".to_string(),
            }],
        )
        .unwrap();
        testutil::scan_checkout(&wt).unwrap();

        // The DB-side universes from the real `db.lbug`.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let (db_scanned, db_planned) = db.code_universes_from_db().unwrap();
        drop(db);

        // The export-side universes from the same scan's `graph.jsonl`.
        let (export_scanned, export_planned) = code_universes_from_export(&wt_apg).unwrap();

        // The scanned universe carries the real code from the source payload
        // (PHASE_09: the frontend module identity is language-rooted),
        // independent of the export file.
        assert!(db_scanned.contains("go.fixture.mod"), "{db_scanned:?}");
        assert!(
            db_scanned.contains("go.fixture.mod.Store"),
            "{db_scanned:?}"
        );
        assert!(db_scanned.contains("/abs/store.go"), "{db_scanned:?}");
        // The planned placeholder is planned, never real code.
        assert!(
            !db_scanned.contains(planned_fqn),
            "a status:planned node is not scanned code: {db_scanned:?}"
        );
        assert!(db_planned.contains(planned_fqn), "{db_planned:?}");

        // The DB reader and the graph.jsonl reader agree exactly on both sets.
        assert_eq!(
            db_scanned, export_scanned,
            "scanned universes must be identical from db.lbug and graph.jsonl"
        );
        assert_eq!(
            db_planned, export_planned,
            "planned universes must be identical from db.lbug and graph.jsonl+plan store"
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-17 (full-scan path): author a node set carrying arbitrary
    /// and empty-valued property keys, an `Entity` with `kind = event`, edge
    /// properties, and both edge halves; project it through a REAL scan
    /// (`create_schema`/`build_load_files`, the `layers::ingest_tree` leg); then
    /// assert `ArtifactDb::node_files_from_db` reconstructs it exactly and that
    /// `graph.jsonl` stays a consistent, property-free export.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn node_files_from_db_round_trips_every_property_through_a_full_scan() {
        let (repo, wt, wt_apg) = testutil::project_with_db("lossless-full-scan");
        let expected = lossless_authored_nodes();

        for node in &expected {
            layers::write_node(&wt_apg, node).unwrap();
        }
        // The full scan path: create_schema + build_load_files project the
        // durable tree (the canonical schema/load projection).
        testutil::scan_checkout(&wt).unwrap();

        let db = ArtifactDb::open(&wt_apg).unwrap();
        let reconstructed = db.node_files_from_db().unwrap();
        drop(db);

        assert_lossless_round_trip(&reconstructed, &expected);

        let graph_path = wt_apg.join(specs::TRANS).join("graph.jsonl");
        assert_graph_jsonl_consistent(&graph_path);

        // The export still describes the authored nodes by identity (it is
        // deliberately not the property carrier).
        let export = std::fs::read_to_string(&graph_path).unwrap();
        assert!(
            export.lines().any(|line| {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                v.get("type").and_then(|t| t.as_str()) == Some("entity")
                    && v.get("fqn").and_then(|f| f.as_str()) == Some("domain.entity.order")
            }),
            "graph.jsonl must describe the authored entity"
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-17 (session incremental path): the same authored node set,
    /// projected through the session's incremental reingest/merge path
    /// (`ArtifactDb::reingest_layers_on`, the `merge_records` write-through the
    /// live coordinator applies at admission) against a real `db.lbug`, must
    /// reconstruct exactly — the serialized-properties column carries the full
    /// map through `merge_node`/`merge_edge` (both halves), and `graph.jsonl` is
    /// left consistent.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn node_files_from_db_round_trips_every_property_through_the_session_merge() {
        let (repo, _wt, wt_apg) = testutil::project_with_db("lossless-session-merge");
        let expected = lossless_authored_nodes();

        // The in-memory effective node set the session holds: build the
        // validated ingest records and the exact touched-FQN delete set, then
        // apply them through the already-held DB handle — the same call the
        // coordinator makes at admission.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let (scanned, planned) = db.code_universes_from_db().unwrap();
        let records = layers::ingest_nodes(&expected, &scanned, &planned).unwrap();
        db.reingest_layers_on(&touched_fqns(&expected), &records)
            .unwrap();

        let reconstructed = db.node_files_from_db().unwrap();
        drop(db);

        assert_lossless_round_trip(&reconstructed, &expected);
        assert_graph_jsonl_consistent(&wt_apg.join(specs::TRANS).join("graph.jsonl"));

        testutil::remove(&repo);
    }

    /// Phase-02 task-3: live-session admission resolves the durable node set and
    /// the `implemented-by` code universes SOLELY from the held `db.lbug`.
    ///
    /// While the session is live, BOTH disk-side sources the pre-phase-2
    /// admission path could have leaned on are destroyed: the mutated node's own
    /// `apg/layers/**` file (deleted) and the code-identity export
    /// `apg/.trans/graph.jsonl` (corrupted). A routed mutation touching the node
    /// still admits — it resolves the node's base content from the held DB, not
    /// the gone file — and its `implemented-by` target validates against the DB's
    /// scanned code universe: the REAL target (`go.fixture.mod.Store`, the scanned
    /// struct) admits, while a bogus target is refused as spec drift, never
    /// silently recorded unchecked/pending (which is what an empty universe would
    /// do). A routed query then sees the projected change.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn session_admission_resolves_the_node_set_and_code_universes_from_the_held_db() {
        let (repo, wt, wt_apg) = testutil::project_with_db("db-only-admission");
        let home = repo.root.join("home");

        // A committed durable Solution System whose `implemented-by` target is
        // REAL scanned code from the fixture payload (`go.fixture.mod.Store`).
        // The scan projects it into `db.lbug`, so the session holds it when the
        // node file and the export are later destroyed.
        let system = layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "system".to_string(),
            name: "api".to_string(),
            body: "the api system".to_string(),
            properties: NodeProperties::default(),
            out: vec![layers::OutEdge {
                kind: "implemented-by".to_string(),
                target: "go.fixture.mod.Store".to_string(),
                properties: NodeProperties::default(),
            }],
            in_edges: Vec::new(),
        };
        layers::write_node(&wt_apg, &system).unwrap();
        testutil::wt_commit(
            &wt,
            &["apg/layers/solution/system/api.json"],
            "seed the api system",
        );
        testutil::scan_checkout(&wt).unwrap();

        // The DB precondition admission relies on: the System is a durable node
        // and the real target is in its scanned code universe.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("solution.system.api"));
            let (scanned, _planned) = db.code_universes_from_db().unwrap();
            assert!(
                scanned.contains("go.fixture.mod.Store"),
                "fixture scanned code: {scanned:?}"
            );
        }

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // Destroy BOTH disk-side sources while the session is live: the mutated
        // node's file and the code-identity export.
        let node_file = layers::node_file_path(&wt_apg, Layer::Solution, "system", "api");
        assert!(
            node_file.exists(),
            "the node file must exist before it is deleted"
        );
        std::fs::remove_file(&node_file).unwrap();
        let export = wt_apg.join(specs::TRANS).join("graph.jsonl");
        assert!(
            export.exists(),
            "the export must exist before it is corrupted"
        );
        std::fs::write(&export, "this is not a graph.jsonl\n").unwrap();

        // (1) A routed mutation touching the (file-deleted) node still admits:
        // its base content and its `implemented-by` ref resolve from the held DB.
        // The edge-preserving update re-validates the existing REAL target.
        let update = testutil::ApgCommand::new(&[
            "node",
            "update",
            "solution",
            "system",
            "api",
            "--body",
            "updated against the held db",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            update.status.success(),
            "a routed mutation touching a node whose file is gone must admit from the DB: {}",
            String::from_utf8_lossy(&update.stderr)
        );

        // The projected update is visible to a routed reader while the session is
        // still live and nothing has been saved.
        let body = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:System {fqn: 'solution.system.api'}) \
                 WHERE n.body = 'updated against the held db' RETURN count(*)",
            ],
            &wt,
        );
        assert!(
            body.status.success(),
            "routed body read: {}",
            String::from_utf8_lossy(&body.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&body.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the routed update must be projected into the live DB at admission"
        );

        // (2) The held DB's scanned universe drives validation: a bogus
        // `implemented-by` target is refused as drift, NOT recorded
        // unchecked/pending (an empty universe would classify it Pending and
        // admit it).
        let bogus = testutil::ApgCommand::new(&[
            "edge",
            "add",
            "implemented-by",
            "solution.system.api",
            "go.fixture.mod.Absent",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            !bogus.status.success(),
            "a bogus implemented-by target must be refused, got: {}",
            String::from_utf8_lossy(&bogus.stdout)
        );
        let bogus_err = String::from_utf8_lossy(&bogus.stderr);
        assert!(
            bogus_err.contains("spec drift"),
            "the bogus target must be refused as spec drift: {bogus_err}"
        );
        assert!(
            bogus_err.contains("go.fixture.mod.Absent"),
            "the refusal must name the bogus target: {bogus_err}"
        );

        // (3) The routed read sees the projected change: the REAL implemented-by
        // target, and only it (the refused bogus edge was never recorded).
        let query = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:System {fqn: 'solution.system.api'})-[:SpecImplementedBy]->(c) \
                 RETURN c.fqn",
            ],
            &wt,
        );
        assert!(
            query.status.success(),
            "routed edge read: {}",
            String::from_utf8_lossy(&query.stderr)
        );
        let stdout = String::from_utf8_lossy(&query.stdout);
        let rows: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            rows,
            vec!["c.fqn", "go.fixture.mod.Store"],
            "the routed read must see exactly the projected REAL implemented-by target"
        );

        // Clean release: save is the single durability point (a dirty `session
        // end` would refuse), then end the session.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-03 task-4: `apg session save` is the as-is single durability point
    /// over the WHOLE buffered set — a mix of accepted writes plus an accepted
    /// delete of a durable node — with NO reconstruction.
    ///
    /// A live session admits every routed mutation into its write-back buffer
    /// and projects it into the held DB, but `apg/layers/**` and git stay at the
    /// last saved state. One `apg session save` then:
    ///   * writes each buffered node file EXACTLY as staged (byte-identical to
    ///     the admitted `PendingChange` content, not rebuilt from the DB);
    ///   * removes the deleted durable node's file; and
    ///   * lands exactly ONE new git commit for the whole set.
    ///
    /// A routed read afterwards agrees: the held DB reports the same admitted
    /// bodies and the deleted node is gone, so disk and DB converge.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn session_save_flushes_buffered_writes_and_delete_as_is_in_one_commit() {
        let (repo, wt, wt_apg) = testutil::project_with_db("session-save-flush");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // Runs one routed mutation through the live session, asserting success.
        let run = |args: &[&str]| {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // A durable baseline: `survivor` (a later RMW target) and `doomed` (a
        // later accepted-delete target) added through the session and saved.
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "survivor",
            "--body",
            "survivor baseline",
        ]);
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "doomed",
            "--body",
            "doomed baseline",
        ]);
        let seed_save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            seed_save.status.success(),
            "{}",
            String::from_utf8_lossy(&seed_save.stderr)
        );

        let path =
            |name: &str| layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", name);
        assert!(path("survivor").exists() && path("doomed").exists());

        let commits_before = testutil::commit_count(&wt);

        // The buffered mix: three accepted writes (two adds and an update over
        // a durable node) plus an accepted delete of a durable node. The
        // expected node files are exactly the content asserted at admission.
        let bare = |name: &str, body: &str| layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: name.to_string(),
            body: body.to_string(),
            properties: NodeProperties::default(),
            out: Vec::new(),
            in_edges: Vec::new(),
        };
        let kept_a = bare("kept-a", "kept a");
        let kept_b = bare("kept-b", "kept b");
        let updated_survivor = bare("survivor", "survivor buffered");

        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "kept-a",
            "--body",
            "kept a",
        ]);
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "kept-b",
            "--body",
            "kept b",
        ]);
        run(&[
            "node",
            "update",
            "requirements",
            "requirement",
            "survivor",
            "--body",
            "survivor buffered",
        ]);
        run(&["node", "rm", "requirements", "requirement", "doomed"]);

        // Before save nothing is durable: the new node files are absent, the
        // update/delete have not touched `apg/layers/**`, and no commit landed.
        assert!(
            !path("kept-a").exists() && !path("kept-b").exists(),
            "a buffered write must not write apg/layers/** before save"
        );
        assert!(
            path("doomed").exists(),
            "a buffered delete must not touch apg/layers/** before save"
        );
        assert_eq!(
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "survivor")
                .unwrap()
                .body,
            "survivor baseline",
            "a buffered update must not touch apg/layers/** before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before,
            "a buffered write/delete must not create a commit before save"
        );

        // The single durability point: the whole buffered set in ONE commit.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 1,
            "the whole buffered set must land in exactly one commit"
        );

        // Every written node file equals its buffered admitted content,
        // byte-for-byte: save flushes the staged `PendingChange` as-is (no
        // reconstruction).
        for expected in [&kept_a, &kept_b, &updated_survivor] {
            let node_path = path(&expected.name);
            assert!(node_path.exists(), "save must write `{}`", expected.name);
            assert_eq!(
                layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", &expected.name)
                    .unwrap(),
                *expected,
                "the on-disk node file for `{}` must equal the admitted content",
                expected.name
            );
            assert_eq!(
                std::fs::read_to_string(&node_path).unwrap(),
                serde_json::to_string_pretty(expected).unwrap(),
                "save must flush `{}` as the exact staged bytes",
                expected.name
            );
        }

        // The deleted durable node's file is gone.
        assert!(
            !path("doomed").exists(),
            "save must remove the deleted durable node's file"
        );

        // The held DB agrees with disk: routed reads report the admitted bodies
        // and the deleted node is gone.
        let assert_body = |fqn: &str, body: &str| {
            let query = format!(
                "MATCH (n:Requirement {{fqn: '{fqn}'}}) WHERE n.body = '{body}' RETURN count(*)"
            );
            let out = testutil::spawn_apg(&["query", &query], &wt);
            assert!(
                out.status.success(),
                "routed read of {fqn}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .last()
                    .map(str::trim),
                Some("1"),
                "the held DB must report the admitted body for {fqn}"
            );
        };
        assert_body("requirements.requirement.kept-a", "kept a");
        assert_body("requirements.requirement.kept-b", "kept b");
        assert_body("requirements.requirement.survivor", "survivor buffered");

        let gone = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.doomed'}) RETURN count(*)",
            ],
            &wt,
        );
        assert!(
            gone.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&gone.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&gone.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("0"),
            "the held DB must agree the deleted node is gone"
        );

        // The durable node-file store matches disk: the three writes are present
        // and the deleted node is absent.
        let nodes = layers::read_existing_nodes(&wt_apg).unwrap();
        for name in ["kept-a", "kept-b", "survivor"] {
            assert!(
                nodes
                    .iter()
                    .any(|n| n.node_type == "requirement" && n.name == name),
                "the durable store must contain `{name}`"
            );
        }
        assert!(
            !nodes.iter().any(|n| n.name == "doomed"),
            "the durable store must no longer contain `doomed`"
        );

        // Clean release: save cleared the buffer (a dirty `end` would refuse).
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }
}
