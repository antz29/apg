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
use std::collections::{BTreeMap, BTreeSet};
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
/// properties — of every durable authored kind. Every AUTHORED edge appears in
/// BOTH endpoint files with identical properties (the pairing invariant). The
/// two code-targeted kinds are the exception: `implemented-by` and `details`
/// both name a code identity (`go.fixture.mod.Store`, the scanned struct
/// `testutil::project_with_db` provides), so only the source's out half exists —
/// a code node has no file to hold the in half.
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
                "requires",
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
                "requires",
                "requirements.requirement.audit",
                props(&[]),
            )],
            vec![inc("represents", "domain.actor.buyer", buyer_props.clone())],
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
            vec![out(
                "contains",
                "requirements.requirement.audit",
                props(&[]),
            )],
            vec![
                inc("requires", "requirements.stakeholder.customer", props(&[])),
                inc("driven-by", "domain.service.billing", drives_props.clone()),
            ],
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
            vec![
                inc("requires", "requirements.user.shopper", props(&[])),
                inc("contains", "requirements.requirement.checkout", props(&[])),
            ],
        ),
        node(
            "implementation",
            "note",
            "design",
            "a design note",
            props(&[("kind", "background"), ("tag", "d"), ("empty-meta", "")]),
            // `details` names a single CODE identity (a code endpoint is
            // pairing-exempt), never an authored node.
            vec![out("details", "go.fixture.mod.Store", props(&[]))],
            vec![],
        ),
        node(
            "requirements",
            "constraint",
            "local-rule",
            "the local rule",
            props(&[
                ("owner", "platform"),
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
            vec![out("contains", "domain.entity.order", props(&[]))],
            vec![],
        ),
        node(
            "domain",
            "entity",
            "order",
            "the order event",
            props(&[("kind", "event"), ("owner", ""), ("empty-meta", "")]),
            vec![out("contains", "domain.value.cart", props(&[]))],
            vec![
                inc("contains", "domain.group.core", props(&[])),
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
                inc("reads", "domain.service.billing", props(&[])),
                inc("writes", "domain.service.billing", props(&[])),
            ],
        ),
        node(
            "domain",
            "value",
            "cart",
            "the cart",
            props(&[("unit", "currency"), ("empty-meta", "")]),
            vec![],
            vec![inc("contains", "domain.entity.order", props(&[]))],
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
                out("reads", "domain.entity.order", props(&[])),
                out("writes", "domain.entity.order", props(&[])),
                out(
                    "driven-by",
                    "requirements.requirement.checkout",
                    drives_props.clone(),
                ),
            ],
            vec![
                inc("calls", "domain.actor.buyer", props(&[])),
                inc("realises", "solution.component.worker", props(&[])),
            ],
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
            "domain",
            "actor",
            "buyer",
            "the buyer",
            props(&[("channel", "web"), ("empty-meta", "")]),
            vec![
                out(
                    "represents",
                    "requirements.user.shopper",
                    buyer_props.clone(),
                ),
                out("calls", "domain.service.billing", props(&[])),
            ],
            vec![inc(
                "represents",
                "solution.person.alice",
                approver_props.clone(),
            )],
        ),
        node(
            "solution",
            "system",
            "platform",
            "the platform",
            props(&[("owner-team", ""), ("empty-meta", "")]),
            vec![out("contains", "solution.container.api", props(&[]))],
            vec![],
        ),
        node(
            "solution",
            "container",
            "api",
            "the api container",
            props(&[("kind", "service"), ("owner", ""), ("empty-meta", "")]),
            vec![
                out("contains", "solution.component.worker", props(&[])),
                out("is-role", "solution.role.app-service", props(&[])),
            ],
            vec![inc("contains", "solution.system.platform", props(&[]))],
        ),
        node(
            "solution",
            "component",
            "worker",
            "the worker component",
            props(&[("runtime", "rust"), ("empty-meta", "")]),
            vec![
                out("implemented-by", "go.fixture.mod.Store", impl_props.clone()),
                out("realises", "domain.service.billing", props(&[])),
                out("is-role", "solution.role.app-service", props(&[])),
            ],
            vec![
                inc("contains", "solution.container.api", props(&[])),
                inc("uses", "solution.person.alice", admin_props.clone()),
            ],
        ),
        node(
            "solution",
            "role",
            "app-service",
            "the application service role",
            props(&[("tier", "app"), ("empty-meta", "")]),
            vec![],
            vec![
                inc("is-role", "solution.container.api", props(&[])),
                inc("is-role", "solution.component.worker", props(&[])),
            ],
        ),
        node(
            "solution",
            "person",
            "alice",
            "alice",
            props(&[("email", "a@x"), ("empty-meta", "")]),
            vec![
                out("uses", "solution.component.worker", admin_props.clone()),
                out("represents", "domain.actor.buyer", approver_props.clone()),
            ],
            vec![],
        ),
    ]
}

/// The exact FQN delete set for `nodes` (`<layer>.<type>.<name>`) — every
/// node's identity rendered as its FQN, sorted and deduplicated by the
/// `BTreeSet`.
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

/// Every regular file under `dir` as `(path relative to `dir`, bytes)`, sorted —
/// the byte-identity snapshot the forced-scan acceptance test compares before
/// and after the rebuild.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((
                    p.strip_prefix(dir).unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
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
    /// Task edge model too: a task written as a title+status record plus its
    /// assignment and per-kind operation edges lands a `Task` row carrying only
    /// fqn/title/status, with the matching `AssignedTo`/`Implements`/`Target`
    /// and per-kind operation rel rows joining the Agent, Component and
    /// live/planned code nodes the records name.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn write_through_projects_task_verb_fields() {
        let (apg_root, repo, _wt) = project_fixture("task-verb");
        // The live code nodes the operation edges name that the fixture's
        // scanned code does not carry: the Function (the Module/File/Struct are
        // the fixture's scanned code). Both are code kinds, so seeding them
        // leaves the DB's authored/transient digest untouched.
        {
            let db = Database::new(
                apg_root.join(specs::TRANS).join("db.lbug"),
                Default::default(),
            )
            .unwrap();
            let conn = Connection::new(&db).unwrap();
            for q in [
                "MERGE (n:Agent {fqn: 'agent.go-frontend-implementer'})",
                "MERGE (n:Function {fqn: 'github.com/x/y.Store.build'})",
            ] {
                conn.query(q).unwrap();
            }
        }

        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let task = "foo/plan.phase-01.task-1".to_string();
        let mut recs = vec![
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
            // The durable Component the task's `implements` edge joins — it is
            // part of the record set the merge projects, like the assignment.
            Record::Component {
                fqn: "solution.component.order-handler".into(),
                name: "order-handler".into(),
                body: String::new(),
                properties: NodeProperties::default(),
            },
            // The task's own storage is title+status alone; its assignment, the
            // Component it implements and the code it touches are edges.
            Record::Task {
                fqn: task.clone(),
                title: "Edge model".into(),
                status: "pending".into(),
            },
            Record::AssignedTo {
                from: task.clone(),
                to: "agent.go-frontend-implementer".into(),
            },
            Record::Implements {
                from: task.clone(),
                to: "solution.component.order-handler".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: task.clone(),
                properties: NodeProperties::default(),
            },
        ];
        // One full per-kind operation family: a create to a planned
        // destination, modify/delete to the live node, and move/rename to the
        // live source plus a `target` edge to the matching planned
        // destination.
        for (fqn, kind) in [
            ("github.com/x/y.new", "module"),
            ("github.com/x/y.moved", "module"),
            ("github.com/x/y.renamed", "module"),
            ("/new/store.go", "file"),
            ("/moved/store.go", "file"),
            ("/renamed/store.go", "file"),
            ("github.com/x/y.New", "struct"),
            ("github.com/x/y.Moved", "struct"),
            ("github.com/x/y.Renamed", "struct"),
            ("github.com/x/y.New.build", "function"),
            ("github.com/x/y.Moved.build", "function"),
            ("github.com/x/y.Renamed.build", "function"),
        ] {
            recs.push(Record::PlannedNode {
                fqn: fqn.into(),
                kind: kind.into(),
                name: String::new(),
                parent: String::new(),
            });
        }
        recs.extend([
            Record::ModuleCreate {
                from: task.clone(),
                to: "github.com/x/y.new".into(),
            },
            Record::ModuleModify {
                from: task.clone(),
                to: "github.com/x/y".into(),
            },
            Record::ModuleDelete {
                from: task.clone(),
                to: "github.com/x/y".into(),
            },
            Record::ModuleMove {
                from: task.clone(),
                to: "github.com/x/y".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "github.com/x/y.moved".into(),
            },
            Record::ModuleRename {
                from: task.clone(),
                to: "github.com/x/y".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "github.com/x/y.renamed".into(),
            },
            Record::FileCreate {
                from: task.clone(),
                to: "/new/store.go".into(),
            },
            Record::FileModify {
                from: task.clone(),
                to: "/abs/store.go".into(),
            },
            Record::FileDelete {
                from: task.clone(),
                to: "/abs/store.go".into(),
            },
            Record::FileMove {
                from: task.clone(),
                to: "/abs/store.go".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "/moved/store.go".into(),
            },
            Record::FileRename {
                from: task.clone(),
                to: "/abs/store.go".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "/renamed/store.go".into(),
            },
            Record::StructCreate {
                from: task.clone(),
                to: "github.com/x/y.New".into(),
            },
            Record::StructModify {
                from: task.clone(),
                to: "github.com/x/y.Store".into(),
            },
            Record::StructDelete {
                from: task.clone(),
                to: "github.com/x/y.Store".into(),
            },
            Record::StructMove {
                from: task.clone(),
                to: "github.com/x/y.Store".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "github.com/x/y.Moved".into(),
            },
            Record::StructRename {
                from: task.clone(),
                to: "github.com/x/y.Store".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "github.com/x/y.Renamed".into(),
            },
            Record::FunctionCreate {
                from: task.clone(),
                to: "github.com/x/y.New.build".into(),
            },
            Record::FunctionModify {
                from: task.clone(),
                to: "github.com/x/y.Store.build".into(),
            },
            Record::FunctionDelete {
                from: task.clone(),
                to: "github.com/x/y.Store.build".into(),
            },
            Record::FunctionMove {
                from: task.clone(),
                to: "github.com/x/y.Store.build".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "github.com/x/y.Moved.build".into(),
            },
            Record::FunctionRename {
                from: task.clone(),
                to: "github.com/x/y.Store.build".into(),
            },
            Record::Target {
                from: task.clone(),
                to: "github.com/x/y.Renamed.build".into(),
            },
        ]);

        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        let db = ArtifactDb::open(&apg_root).unwrap();

        // The Task row carries fqn/title/status only; the retired properties
        // have no column, so their old query shapes no longer bind.
        let out = db
            .q("MATCH (t:Task) RETURN t.fqn, t.title, t.status")
            .unwrap();
        assert!(
            out.contains("foo/plan.phase-01.task-1")
                && out.contains("Edge model")
                && out.contains("pending"),
            "write-through task row: {out}"
        );
        for retired in ["t.kind", "t.tier", "t.verb", "t.target", "t.new_fqn"] {
            let q = format!("MATCH (t:Task) RETURN {retired}");
            assert!(
                db.q(&q).is_err(),
                "the retired task property `{retired}` must not bind"
            );
        }

        // The assignment and implementation edges join the Agent/Component.
        let out = db
            .q("MATCH (t:Task)-[:AssignedTo]->(a:Agent) RETURN t.fqn, a.fqn")
            .unwrap();
        assert!(
            out.contains("foo/plan.phase-01.task-1")
                && out.contains("agent.go-frontend-implementer"),
            "assigned-to row: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:Implements]->(c:Component) RETURN c.fqn")
            .unwrap();
        assert!(
            out.contains("solution.component.order-handler"),
            "implements row: {out}"
        );

        // Every per-kind operation rel table projects its row, joining the
        // live or planned code node the record names.
        for (table, target) in [
            ("ModuleCreate", "github.com/x/y.new"),
            ("ModuleModify", "github.com/x/y"),
            ("ModuleDelete", "github.com/x/y"),
            ("ModuleMove", "github.com/x/y"),
            ("ModuleRename", "github.com/x/y"),
            ("FileCreate", "/new/store.go"),
            ("FileModify", "/abs/store.go"),
            ("FileDelete", "/abs/store.go"),
            ("FileMove", "/abs/store.go"),
            ("FileRename", "/abs/store.go"),
            ("StructCreate", "github.com/x/y.New"),
            ("StructModify", "github.com/x/y.Store"),
            ("StructDelete", "github.com/x/y.Store"),
            ("StructMove", "github.com/x/y.Store"),
            ("StructRename", "github.com/x/y.Store"),
            ("FunctionCreate", "github.com/x/y.New.build"),
            ("FunctionModify", "github.com/x/y.Store.build"),
            ("FunctionDelete", "github.com/x/y.Store.build"),
            ("FunctionMove", "github.com/x/y.Store.build"),
            ("FunctionRename", "github.com/x/y.Store.build"),
        ] {
            let out = db
                .q(&format!("MATCH (t:Task)-[:{table}]->(n) RETURN n.fqn"))
                .unwrap();
            assert!(out.contains(target), "{table} row: {out}");
        }

        // Every move/rename destination lands on the `target` rel table.
        let out = db.q("MATCH (t:Task)-[:Target]->(n) RETURN n.fqn").unwrap();
        for dest in [
            "github.com/x/y.moved",
            "github.com/x/y.renamed",
            "/moved/store.go",
            "/renamed/store.go",
            "github.com/x/y.Moved",
            "github.com/x/y.Renamed",
            "github.com/x/y.Moved.build",
            "github.com/x/y.Renamed.build",
        ] {
            assert!(out.contains(dest), "target destination `{dest}`: {out}");
        }
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

    /// Phase-01 task-7: the central write funnel keeps its guard (defence in
    /// depth). On the MAIN checkout both the project-scoped plan target
    /// (membership guard) and the shared `_invariants.jsonl` target (universal
    /// guard) refuse, naming branch `main`, and neither file is created.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/git); run via cargo test-e2e"]
    fn write_funnel_refuses_outside_a_project_worktree() {
        let repo = Repo::new("artifacts-main-guard");
        let apg_root = repo.apg_root();
        let commits_before = testutil::commit_count(&repo.root);
        let recs = baseline_records();

        let assert_refusal = |msg: &str| {
            assert!(msg.starts_with("mutation refused — "), "{msg}");
            assert!(msg.contains("branch `main`"), "{msg}");
            assert_eq!(msg.matches("Fix:").count(), 1, "one Fix line: {msg}");
            assert!(msg.contains("apg project start"), "{msg}");
        };

        // A project-scoped plan target: the membership guard.
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        let msg = format!(
            "{:#}",
            write_jsonl_and_reingest(&apg_root, &plan_path, "foo", &recs).unwrap_err()
        );
        assert_refusal(&msg);
        assert!(
            !plan_path.exists(),
            "a refused write must not create the plan"
        );

        // The shared `_invariants.jsonl` ledger: the universal guard.
        let invariants = apg_root.join("_invariants.jsonl");
        let msg = format!(
            "{:#}",
            write_jsonl_and_reingest(&apg_root, &invariants, "foo", &recs).unwrap_err()
        );
        assert_refusal(&msg);
        assert!(
            !invariants.exists(),
            "a refused write must not create _invariants.jsonl"
        );

        assert_eq!(
            testutil::commit_count(&repo.root),
            commits_before,
            "a refused write must not create a commit"
        );
        testutil::remove(&repo);
    }

    /// Phase-08 task-2: `code_universes` classifies Real (the branch
    /// database's scanned code) / Pending (a planned FQN the plan store
    /// declares but the scanned universe lacks) / Drift (present in neither).
    ///
    /// The database is the one code-identity source: the fixture is a real
    /// hermetic scan (`testutil::project_with_db`) whose plan store declares a
    /// `status = planned` Struct the source does not contain, so the scan
    /// projects it as a database row and the reader serves both universes from
    /// `db.lbug` alone. No `graph.jsonl` is written or parsed.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn code_universes_from_the_branch_database_classify_a_planned_absent_fqn_as_pending() {
        let (repo, wt, wt_apg) = testutil::project_with_db("code-universes-export-classify");

        // A plan-writer declaration the scanned source does not contain: the
        // next scan projects it as a `status = planned` Struct row, so the
        // branch database itself carries both universes.
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

        let (scanned, planned) = code_universes(&wt_apg).unwrap();
        assert!(
            scanned.contains("go.fixture.mod.Store"),
            "a scanned struct is real code: {scanned:?}"
        );
        assert!(
            !scanned.contains(planned_fqn),
            "a status:planned row is not real code: {scanned:?}"
        );
        assert!(
            planned.contains(planned_fqn),
            "the declared-but-unscanned FQN is planned: {planned:?}"
        );

        // The three-way classification: Real / Pending (never Drift) / Drift.
        assert_eq!(
            apg::layers::classify_code_ref("go.fixture.mod.Store", &scanned, &planned),
            apg::layers::CodeRefStatus::Real
        );
        assert_eq!(
            apg::layers::classify_code_ref(planned_fqn, &scanned, &planned),
            apg::layers::CodeRefStatus::Pending
        );
        assert_eq!(
            apg::layers::classify_code_ref("go.fixture.gone.Nope", &scanned, &planned),
            apg::layers::CodeRefStatus::Drift
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-23: the four Implementation kinds a code FQN **records in
    /// `db.lbug`** are what a caller materialises it at — one row of each kind
    /// (module, file, struct, function) is present, and the recorded kinds are
    /// exactly those four (no other kind is invented).
    ///
    /// The fixture is a REAL database: a hermetic scan of a scratch repo
    /// (`testutil::project_with_db`) whose payload declares a module, a struct,
    /// a function and a file. `PreviousScan::capture` reads the recorded kinds
    /// from that database alone — this test writes and parses no `graph.jsonl`
    /// export.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn code_kinds_from_export_reads_the_four_implementation_types() {
        let (repo, wt, wt_apg) = testutil::project_with_db("kinds-four-kinds");

        // One function row on top of the fixture payload (module + struct +
        // file): the recorded kinds must span all four Implementation labels.
        let seed = wt.join("code/seed.scan.jsonl");
        let mut payload = testutil::code_payload("fixture.mod", "/abs/store.go", &["Store"]);
        payload.push_str(&testutil::function_line(
            "n4",
            "fixture.mod",
            "Lookup",
            "/abs/store.go",
        ));
        payload.push('\n');
        std::fs::write(&seed, payload).unwrap();
        testutil::wt_commit(&wt, &["code/seed.scan.jsonl"], "function payload");
        testutil::scan_checkout(&wt).unwrap();

        let snapshot = PreviousScan::capture(&wt_apg).expect("a clean scanned database captures");

        let kinds: BTreeSet<&str> = snapshot.kinds.values().map(String::as_str).collect();
        assert_eq!(
            kinds,
            BTreeSet::from(["module", "file", "struct", "function"]),
            "the database records exactly the four Implementation kinds: {:?}",
            snapshot.kinds
        );
        for (fqn, kind) in [
            ("go.fixture.mod", "module"),
            ("/abs/store.go", "file"),
            ("go.fixture.mod.Store", "struct"),
            ("go.fixture.mod.Lookup", "function"),
        ] {
            assert_eq!(
                snapshot.kinds.get(fqn).map(String::as_str),
                Some(kind),
                "{fqn} must record its Implementation kind: {:?}",
                snapshot.kinds
            );
        }

        testutil::remove(&repo);
    }

    /// Phase-08 task-1: the Implementation kind a code FQN **records in the
    /// previous `db.lbug`** is what a caller materialises it at — a planned
    /// row keeps its own label's kind (`struct`) and never falls back to
    /// `module` — and an FQN the database does not record stays **absent** from
    /// the map, so no `module` kind is invented for it.
    ///
    /// The fixture is a REAL database: a hermetic scan of a scratch repo
    /// (`testutil::project_with_db`) whose plan store declares one planned
    /// Struct, which the scan projects as a `status = planned` code row in
    /// `apg/.trans/db.lbug`. `PreviousScan::capture` reads the recorded kinds
    /// from that database alone — this test writes and parses no
    /// `graph.jsonl` export.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn code_kinds_from_export_prefers_the_recorded_planned_kind_over_the_module_fallback() {
        let (repo, wt, wt_apg) = testutil::project_with_db("kinds-from-db");

        // The plan store's declaration: the next scan projects it as a
        // `status = planned` Struct row in `db.lbug`. Its `parent` is the
        // scanned module FQN, so the Contains edge resolves.
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

        // The fixture's recorded state is the database alone: no export is
        // written, and none is read below.
        assert!(
            !wt_apg.join(specs::TRANS).join("graph.jsonl").exists(),
            "the fixture carries no graph.jsonl export"
        );

        let snapshot = PreviousScan::capture(&wt_apg).expect("a clean scanned database captures");

        assert_eq!(
            snapshot.kinds.get(planned_fqn).map(String::as_str),
            Some("struct"),
            "the planned row's recorded kind wins — the FQN is not materialised at `module`"
        );
        assert!(
            snapshot.planned.contains(planned_fqn),
            "the recorded row is the planned one: {:?}",
            snapshot.planned
        );
        assert!(
            !snapshot.kinds.contains_key("go.fixture.mod.Gone"),
            "an FQN the database does not record stays absent — no `module` kind is invented"
        );

        testutil::remove(&repo);
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
        // yet: the node file is absent and no commit landed. The anchor is a
        // lone `global.constraint` (an unconstrained type, so the buffered model
        // is whole-model-valid at the SPEC §8.1 save).
        let add = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "buffered"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        let node_file = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "buffered");
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
                "MATCH (n:Constraint {fqn: 'global.constraint.buffered'}) RETURN count(n)",
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
    /// Pending (never drift) once `code_universes` reads the branch database,
    /// whose plan-store planned rows carry the declaration.
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
        let (scanned, planned_fqns) = code_universes(&apg_root).unwrap();
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
    /// `Container` `kind`, `Group` `attribute`/`root`, the `Constraint`'s
    /// authored metadata (`scope`) serialized in the properties column), and
    /// BOTH halves of every AUTHORED-pair edge. The code-targeted edge kinds
    /// (`implemented-by`, `details`) carry only the source's out half — a code
    /// identity has no node file. Phase 4 added the serialized-properties
    /// column, so the authored `driven-by` edge property now round-trips
    /// on both halves; the exact full-fidelity round-trip (arbitrary keys,
    /// `Entity` `kind`, the session's incremental merge path) is pinned by
    /// phase 4's own e2e task.
    ///
    /// The database working state the next save reads is pinned over the same
    /// real `db.lbug`: the pending-removal ledger (`record_pending_removal` →
    /// `pending_removals`, with the identity's durable row left in place but
    /// omitted from the reconstruction), the `updated` dirty mark landing on
    /// exactly the rows the real change builders name — an authored edge's two
    /// endpoints, a code-target edge's source alone, and a delete's rewritten
    /// authored neighbours — and the mark never projecting into a node file.
    /// "Exactly" is measured from the database (every label's `updated = 1`
    /// rows), never from a change's own `writes` roster, so a marking that
    /// widened to another row fails here.
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

        // The `driven-by` edge carries an authored edge property on BOTH halves
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
            out: Vec::new(),
            in_edges: vec![
                in_edge("requires", "requirements.stakeholder.customer", props(&[])),
                in_edge("driven-by", "domain.service.billing", drives_props()),
            ],
        };
        let stakeholder = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "stakeholder".to_string(),
            name: "customer".to_string(),
            body: "the customer".to_string(),
            properties: props(&[("tier", "gold")]),
            out: vec![out_edge(
                "requires",
                "requirements.requirement.checkout",
                props(&[]),
            )],
            in_edges: Vec::new(),
        };
        let note = layers::NodeFile {
            layer: "implementation".to_string(),
            node_type: "note".to_string(),
            name: "note-1".to_string(),
            body: "a note".to_string(),
            properties: props(&[("kind", "background")]),
            // `details` names a single CODE identity (a code endpoint is
            // pairing-exempt): only this source's out half exists.
            out: vec![out_edge("details", "go.fixture.mod.Store", props(&[]))],
            in_edges: Vec::new(),
        };
        let group = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "group".to_string(),
            name: "core".to_string(),
            body: "the core domain".to_string(),
            properties: props(&[("attribute", "core"), ("root", "checkout")]),
            out: Vec::new(),
            in_edges: Vec::new(),
        };
        let service = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "service".to_string(),
            name: "billing".to_string(),
            body: "the billing service".to_string(),
            properties: props(&[("runtime", "rust")]),
            out: vec![out_edge(
                "driven-by",
                "requirements.requirement.checkout",
                drives_props(),
            )],
            in_edges: Vec::new(),
        };
        let container = layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "container".to_string(),
            name: "api".to_string(),
            body: "the api service".to_string(),
            properties: props(&[("kind", "service")]),
            out: vec![out_edge(
                "contains",
                "solution.component.worker",
                props(&[]),
            )],
            in_edges: Vec::new(),
        };
        let component = layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "component".to_string(),
            name: "worker".to_string(),
            body: "the worker component".to_string(),
            properties: props(&[("runtime", "rust")]),
            // A code endpoint: the target has no node file, so only this
            // source-side `out` half exists.
            out: vec![out_edge(
                "implemented-by",
                "go.fixture.mod.Store",
                props(&[]),
            )],
            in_edges: vec![in_edge("contains", "solution.container.api", props(&[]))],
        };
        let constraint = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "constraint".to_string(),
            name: "local-rule".to_string(),
            body: "the local rule".to_string(),
            properties: props(&[("scope", "local")]),
            out: Vec::new(),
            in_edges: Vec::new(),
        };

        for node in [
            &requirement,
            &stakeholder,
            &note,
            &group,
            &service,
            &container,
            &component,
            &constraint,
        ] {
            layers::write_node(&wt_apg, node).unwrap();
        }
        // Project the durable tree into a real db.lbug through the real scan
        // path (the canonical schema/load projection).
        testutil::scan_checkout(&wt).unwrap();

        let db = ArtifactDb::open(&wt_apg).unwrap();
        let nodes = db.node_files_from_db().unwrap();

        // Only the durable authored nodes are reconstructed — the scanned code
        // and the transient rows are not durable node files.
        assert_eq!(nodes.len(), 8, "durable node files: {nodes:#?}");

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

        let n = find("implementation", "note", "note-1");
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
        assert_eq!(k.properties, props(&[("scope", "local")]));

        // Both halves of `driven-by`, with the authored edge property intact: the
        // serialized-properties column carries it, and it feeds both halves.
        let drives_out = out(&service, "driven-by", "requirements.requirement.checkout");
        assert_eq!(
            drives_out.properties,
            drives_props(),
            "the serialized-properties column round-trips an edge property: {drives_out:?}"
        );
        let drives_in = incoming(&r, "driven-by", "domain.service.billing");
        assert_eq!(
            drives_in.properties,
            drives_props(),
            "the serialized-properties column round-trips an edge property: {drives_in:?}"
        );

        // Both halves of `requires`.
        out(
            &stakeholder,
            "requires",
            "requirements.requirement.checkout",
        );
        incoming(&r, "requires", "requirements.stakeholder.customer");

        // Both halves of `contains` Container → Component.
        out(&c, "contains", "solution.component.worker");
        incoming(&component, "contains", "solution.container.api");

        // `implemented-by`: only the source's out half (the code endpoint has no
        // node file to hold the in half).
        let implemented_out = out(&component, "implemented-by", "go.fixture.mod.Store");
        assert!(implemented_out.properties.is_empty());

        // `details` to a code identity: only the note's out half (mirrors
        // `implemented-by` — a code endpoint has no node file).
        let details_out = out(&n, "details", "go.fixture.mod.Store");
        assert!(details_out.properties.is_empty());

        // ------------------------------------------------------------------
        // The database working state a save reads: the dirty mark and the
        // pending-removal ledger.
        //
        // "Exactly the named rows" is measured from the DATABASE: every label
        // the schema declares is asked which of its rows carries `updated = 1`,
        // so a marking that widened to a neighbour row (or another table) fails
        // here even when the code believed its write set was the named one.
        // Only the durable node tables can answer that property read; a code or
        // transient table refuses it at bind time, which is how the probe finds
        // the roster (one connection per label, so a refusal poisons nothing).
        let dirty_rows = |db: &ArtifactDb| -> Vec<(String, String)> {
            let mut rows = Vec::new();
            for label in load::node_labels() {
                let Ok(conn) = db.conn() else { continue };
                let Ok(result) = conn.query(&format!(
                    "MATCH (n:{label}) WHERE n.updated = 1 RETURN n.fqn"
                )) else {
                    continue;
                };
                for row in result {
                    rows.push((label.to_string(), row[0].to_string()));
                }
            }
            rows.sort();
            rows
        };
        // A mark is session state, not node-file data, so each case is measured
        // from a clean database (the roster probe again: a table without the
        // `updated` column refuses the SET).
        let clear_marks = |db: &ArtifactDb| {
            for label in load::node_labels() {
                let Ok(conn) = db.conn() else { continue };
                let _ = conn.query(&format!("MATCH (n:{label}) SET n.updated = 0"));
            }
        };
        let named = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            let mut rows: Vec<(String, String)> = pairs
                .iter()
                .map(|(label, fqn)| (label.to_string(), fqn.to_string()))
                .collect();
            rows.sort();
            rows
        };
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The session's own change builder, over the DB-reconstructed base: the
        // `writes` it folds are exactly the node files `apply_mutation` marks.
        let change = |base: &[layers::NodeFile], kind: &str, argv: &[&str]| {
            apg::node_cmd::build_change_over(base, &[], &wt_apg, kind, &args(argv)).unwrap()
        };

        assert_eq!(
            dirty_rows(&db),
            named(&[]),
            "no row carries `updated = 1` before an admission marks one"
        );

        // A code-target write (`implemented-by` → a scanned code identity)
        // rewrites the source's node file alone: the code endpoint has no node
        // file, so exactly the source row is marked and no code row ever is.
        let code_target = change(
            &nodes,
            "edge",
            &[
                "add",
                "implemented-by",
                "solution.component.worker",
                "go.fixture.mod",
            ],
        );
        db.mark_dirty(&code_target.writes).unwrap();
        assert_eq!(
            dirty_rows(&db),
            named(&[("Component", "solution.component.worker")]),
            "a code-target write marks its source alone"
        );

        // The flag never leaks into the reconstruction: the mark is database
        // working state, so the node files are identical and no properties map
        // carries an `updated` key.
        let marked = db.node_files_from_db().unwrap();
        assert_eq!(
            marked, nodes,
            "a dirty mark must not change what the node files reconstruct to"
        );
        assert!(
            marked.iter().all(|n| !n.properties.contains_key("updated")),
            "the session's `updated` flag must never project into a node file"
        );
        clear_marks(&db);
        assert_eq!(dirty_rows(&db), named(&[]), "the marks clear");

        // An authored-edge write (`realises` Component → Service) rewrites BOTH
        // endpoint node files, so exactly both endpoint rows are marked.
        let authored_edge = change(
            &nodes,
            "edge",
            &[
                "add",
                "realises",
                "solution.component.worker",
                "domain.service.billing",
            ],
        );
        db.mark_dirty(&authored_edge.writes).unwrap();
        assert_eq!(
            dirty_rows(&db),
            named(&[
                ("Component", "solution.component.worker"),
                ("Service", "domain.service.billing"),
            ]),
            "an authored edge marks both endpoints and nothing else"
        );
        clear_marks(&db);

        // A delete marks every authored neighbour `node_rm_change` folds into
        // `writes` — the nodes whose edge halves name the removed identity —
        // and the removed identity's own row is not marked (its file is
        // deleted, so there is nothing to write).
        let delete = change(
            &nodes,
            "node",
            &["rm", "requirements", "requirement", "checkout"],
        );
        db.mark_dirty(&delete.writes).unwrap();
        assert_eq!(
            dirty_rows(&db),
            named(&[
                ("Stakeholder", "requirements.stakeholder.customer"),
                ("Service", "domain.service.billing"),
            ]),
            "a delete marks exactly its rewritten authored neighbours"
        );
        clear_marks(&db);
        assert_eq!(dirty_rows(&db), named(&[]), "no mark survives the reset");

        // Record-then-read: the ledger holds one removed identity, idempotently,
        // and `node_files_from_db` omits that identity's node file from the base
        // while the durable row itself is untouched (the omission is the
        // ledger's, not a delete), so the neighbour keeps the edge half the save
        // boundary's pairing check has to refuse.
        assert!(
            db.pending_removals().unwrap().is_empty(),
            "the ledger starts empty"
        );
        db.record_pending_removal("requirements.stakeholder.customer")
            .unwrap();
        db.record_pending_removal("requirements.stakeholder.customer")
            .unwrap();
        assert_eq!(
            db.pending_removals().unwrap(),
            vec![PendingRemoval {
                fqn: "requirements.stakeholder.customer".to_string()
            }],
            "the ledger holds the identity exactly once"
        );
        assert!(
            db.has_node("requirements.stakeholder.customer"),
            "the ledger omits the node file; the durable row stays"
        );
        let held = db.node_files_from_db().unwrap();
        assert_eq!(
            held.len(),
            nodes.len() - 1,
            "the ledger-held identity is gone from the reconstruction: {held:#?}"
        );
        assert!(
            !held.iter().any(|n| n.layer == "requirements"
                && n.node_type == "stakeholder"
                && n.name == "customer"),
            "a ledger-held identity never reconstructs into the base"
        );
        let held_requirement = held
            .iter()
            .find(|n| n.layer == "requirements" && n.node_type == "requirement")
            .unwrap();
        assert!(
            held_requirement
                .in_edges
                .iter()
                .any(|e| e.kind == "requires" && e.source == "requirements.stakeholder.customer"),
            "the omission drops the node file alone: the neighbour keeps its half"
        );

        drop(db);
        testutil::remove(&repo);
    }

    /// Phase-06 task-9: `PreviousScan::capture` is the previous database's
    /// snapshot over **one** read — the scanned and planned code universes,
    /// every code FQN's recorded kind, and the `Agent` roster — captured from a
    /// REAL `db.lbug` (a hermetic scan of real source,
    /// `testutil::project_with_db`, plus a plan-writer-authored planned
    /// Implementation node the scan projects as a `status = planned` code row).
    /// A database carrying pending state (an admitted-but-unsaved ledger
    /// identity) captures as absent, never as a partial snapshot.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn previous_scan_capture_yields_a_real_scan_snapshot() {
        let (repo, wt, wt_apg) = testutil::project_with_db("code-universes-from-db");

        // A planned Implementation node declared in the plan store: the next
        // scan projects it as a `status = planned` Struct row into `db.lbug`.
        // `parent` is the scanned module FQN, so the Contains edge resolves.
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

        let snapshot = PreviousScan::capture(&wt_apg).expect("a clean scanned database captures");

        let db = ArtifactDb::open(&wt_apg).unwrap();
        let (db_scanned, db_planned) = db.code_universes_from_db().unwrap();

        // The captured universes are the database's own, and carry the real
        // code from the source payload (PHASE_09: the frontend module identity
        // is language-rooted).
        assert_eq!(
            snapshot.scanned, db_scanned,
            "the capture's scanned universe is the database's"
        );
        assert_eq!(
            snapshot.planned, db_planned,
            "the capture's planned universe is the database's"
        );
        for fqn in ["go.fixture.mod", "go.fixture.mod.Store", "/abs/store.go"] {
            assert!(
                snapshot.scanned.contains(fqn),
                "{fqn} is scanned code: {:?}",
                snapshot.scanned
            );
        }
        // The planned placeholder is planned, never real code.
        assert!(
            !snapshot.scanned.contains(planned_fqn),
            "a status:planned node is not scanned code: {:?}",
            snapshot.scanned
        );
        assert!(
            snapshot.planned.contains(planned_fqn),
            "{:?}",
            snapshot.planned
        );

        // Every universe FQN carries the kind its database row records.
        for (fqn, kind) in [
            ("go.fixture.mod", "module"),
            ("go.fixture.mod.Store", "struct"),
            ("/abs/store.go", "file"),
            (planned_fqn, "struct"),
        ] {
            assert_eq!(
                snapshot.kinds.get(fqn).map(String::as_str),
                Some(kind),
                "{fqn} must record its Implementation kind: {:?}",
                snapshot.kinds
            );
        }
        for fqn in snapshot.scanned.iter().chain(snapshot.planned.iter()) {
            assert!(
                snapshot.kinds.contains_key(fqn),
                "{fqn} has no recorded kind: {:?}",
                snapshot.kinds
            );
        }

        // The Agent roster is the database's own `Agent` rows, as the export
        // records the rebuild re-emits (`agent.` prefix stripped).
        let (_, agent_rows) = apg::splice::query_rows(
            &db.conn().unwrap(),
            "MATCH (n:Agent) RETURN n.fqn, n.path, n.start_line, n.end_line ORDER BY n.fqn",
        )
        .unwrap();
        let db_agents: Vec<Record> = agent_rows
            .iter()
            .filter_map(|row| {
                let fqn = apg::splice::cell(row, 0);
                let name = fqn.strip_prefix("agent.")?.to_string();
                Some(Record::Agent {
                    name,
                    path: apg::splice::cell(row, 1),
                    start_line: apg::splice::cell(row, 2).parse().unwrap_or(0),
                    end_line: apg::splice::cell(row, 3).parse().unwrap_or(0),
                })
            })
            .collect();
        assert_eq!(
            snapshot.agents, db_agents,
            "the capture carries the database's Agent roster"
        );
        drop(db);

        // Pending state: an admitted-but-unsaved ledger identity never seeds a
        // rebuild — the capture fails closed.
        ArtifactDb::open(&wt_apg)
            .unwrap()
            .record_pending_removal("requirements.stakeholder.customer")
            .unwrap();
        assert!(
            PreviousScan::capture(&wt_apg).is_none(),
            "a database carrying pending state does not capture"
        );

        // An absent database has no previous scan either.
        std::fs::remove_file(wt_apg.join(specs::TRANS).join("db.lbug")).unwrap();
        assert!(
            PreviousScan::capture(&wt_apg).is_none(),
            "an absent database does not capture"
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-17 (full-scan path): author a node set carrying arbitrary
    /// and empty-valued property keys, an `Entity` with `kind = event`, edge
    /// properties, and both edge halves; project it through a REAL scan
    /// (`create_schema`/`build_load_files`, the `layers::ingest_tree` leg); then
    /// assert `ArtifactDb::node_files_from_db` reconstructs it exactly and that
    /// the database describes the authored nodes by identity.
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
        assert!(
            db.has_node("domain.entity.order"),
            "the database must describe the authored entity"
        );
        drop(db);

        assert_lossless_round_trip(&reconstructed, &expected);

        testutil::remove(&repo);
    }

    /// Phase-04 task-17 (session incremental path): the same authored node set,
    /// projected through the session's incremental reingest/merge path
    /// (`ArtifactDb::reingest_layers_on`, the `merge_records` write-through the
    /// live coordinator applies at admission) against a real `db.lbug`, must
    /// reconstruct exactly — the serialized-properties column carries the full
    /// map through `merge_node`/`merge_edge` (both halves).
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

        testutil::remove(&repo);
    }

    /// Phase-05 task-14 (partial-delta path): phase-4's lossless projection
    /// survives the session's PARTIAL delta. A routed mutation projects only the
    /// delta it changed — the written node's node+out-edge records via
    /// `layers::tree::records_for_nodes`, extended with the in-edges read back
    /// from the held DB (`ArtifactDb::incident_edge_records_from_db`) — through
    /// `layers::write::project_only`.
    ///
    /// The authored set carries arbitrary/unknown AND empty-valued property keys
    /// on every node, an authored `driven-by` edge with its own property map on
    /// BOTH halves, and an incoming durable `contains` edge on the mutated node
    /// from a source the mutation does NOT touch — so the incident-restore seam is
    /// exercised with real edge properties. After a routed `node update` over the
    /// property-carrying node through the live session, the held DB (read once the
    /// session releases it) reconstructs the WHOLE node-file set exactly: the
    /// updated body, the full properties map (arbitrary + empty-valued keys), the
    /// `driven-by` properties on both halves, and the restored `contains` in-edge
    /// with its properties.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn node_files_from_db_round_trips_every_property_through_the_partial_delta() {
        let (repo, wt, wt_apg) = testutil::project_with_db("lossless-partial-delta");
        let home = repo.root.join("home");

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

        // The authored `driven-by` edge carries a property map on BOTH halves (the
        // pairing invariant); the incoming `contains` edge carries its own map so
        // the incident-restore round-trips edge properties, not just endpoints.
        let drives_props = || props(&[("flavor", "context-map"), ("empty-prop", "")]);
        let contains_props = || props(&[("since", "v1"), ("empty-prop", "")]);

        // The mutated node: arbitrary + empty-valued property keys, an out-edge
        // with properties, and an incoming durable edge from a source the
        // mutation will NOT touch (so `incident_edge_records_from_db` restores it).
        let checkout = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: "checkout".to_string(),
            body: "the checkout flow".to_string(),
            properties: props(&[
                ("id", "REQ-1"),
                ("feature", "checkout"),
                ("owner", ""),
                ("empty-meta", ""),
            ]),
            out: Vec::new(),
            in_edges: vec![
                in_edge(
                    "requires",
                    "requirements.stakeholder.customer",
                    contains_props(),
                ),
                in_edge("driven-by", "domain.service.billing", drives_props()),
            ],
        };
        // The `driven-by` source: a Service with empty-valued keys.
        let billing = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "service".to_string(),
            name: "billing".to_string(),
            body: "the billing service".to_string(),
            properties: props(&[("runtime", "rust"), ("empty-meta", "")]),
            out: vec![out_edge(
                "driven-by",
                "requirements.requirement.checkout",
                drives_props(),
            )],
            in_edges: vec![
                // Owned by the leaf Group below — the whole-model ownership
                // minimum the SPEC §8.1 save boundary enforces.
                in_edge("contains", "domain.group.ordering", props(&[])),
                // Realised by the worker Component — the §6 coverage minimum.
                in_edge("realises", "solution.component.worker", props(&[])),
            ],
        };
        // An Entity carrying `kind = event` (a durable key with no dedicated
        // column, so only the serialized-properties column can carry it) plus
        // empty-valued keys. It is owned by the Group and owns a Value so the
        // seeded model is whole-model-valid.
        let order = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "entity".to_string(),
            name: "order".to_string(),
            body: "the order event".to_string(),
            properties: props(&[("kind", "event"), ("owner", ""), ("empty-meta", "")]),
            out: vec![out_edge("contains", "domain.value.order-id", props(&[]))],
            in_edges: vec![in_edge("contains", "domain.group.ordering", props(&[]))],
        };
        // The leaf Group owning the Service and the Entity.
        let ordering = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "group".to_string(),
            name: "ordering".to_string(),
            body: "the ordering group".to_string(),
            properties: props(&[("attribute", "core")]),
            out: vec![
                out_edge("contains", "domain.service.billing", props(&[])),
                out_edge("contains", "domain.entity.order", props(&[])),
            ],
            in_edges: Vec::new(),
        };
        // The Value the Entity owns.
        let order_id = layers::NodeFile {
            layer: "domain".to_string(),
            node_type: "value".to_string(),
            name: "order-id".to_string(),
            body: "the order id".to_string(),
            properties: props(&[]),
            out: Vec::new(),
            in_edges: vec![in_edge("contains", "domain.entity.order", props(&[]))],
        };
        // The `requires` source: untouched by the mutation, so its out half
        // persists while the target's in half is restored by the incident seam.
        let customer = layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "stakeholder".to_string(),
            name: "customer".to_string(),
            body: "the customer".to_string(),
            properties: props(&[("tier", "gold"), ("empty-meta", "")]),
            out: vec![out_edge(
                "requires",
                "requirements.requirement.checkout",
                contains_props(),
            )],
            in_edges: Vec::new(),
        };

        // A Component realising the Service (the whole-model minimum) plus its
        // exactly-one Role — the new §4/§6 solution-coverage rules.
        let worker = layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "component".to_string(),
            name: "worker".to_string(),
            body: "the worker component".to_string(),
            properties: props(&[]),
            out: vec![
                out_edge("realises", "domain.service.billing", props(&[])),
                out_edge("is-role", "solution.role.app-service", props(&[])),
            ],
            in_edges: Vec::new(),
        };
        let app_role = layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "role".to_string(),
            name: "app-service".to_string(),
            body: "the application service role".to_string(),
            properties: props(&[("applies-to", "component")]),
            out: Vec::new(),
            in_edges: vec![in_edge("is-role", "solution.component.worker", props(&[]))],
        };

        for node in [
            &checkout, &billing, &order, &ordering, &order_id, &customer, &worker, &app_role,
        ] {
            layers::write_node(&wt_apg, node).unwrap();
        }
        wt_commit_paths(&wt, &["apg/layers"], "seed the lossless node set");
        // Project the durable tree through the canonical full scan; the session
        // then owns this DB and reconstructs its base from it.
        testutil::scan_checkout(&wt).unwrap();

        // Precondition: the base full-scan projection already round-trips exactly.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            let base = db.node_files_from_db().unwrap();
            drop(db);
            assert_lossless_round_trip(
                &base,
                &[
                    checkout.clone(),
                    billing.clone(),
                    order.clone(),
                    ordering.clone(),
                    order_id.clone(),
                    customer.clone(),
                    worker.clone(),
                    app_role.clone(),
                ],
            );
        }

        // A routed `node update` through the live session: edge-preserving, so the
        // delta is `records_for_nodes` (checkout + its `driven-by` out-edge) plus
        // `incident_edge_records_from_db` (the `contains` in-edge from the
        // untouched customer), applied via `project_only`.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        let update = testutil::ApgCommand::new(&[
            "node",
            "update",
            "requirements",
            "requirement",
            "checkout",
            "--body",
            "updated through the partial delta",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            update.status.success(),
            "routed update must admit through the partial-delta path: {}",
            String::from_utf8_lossy(&update.stderr)
        );

        // The single durability point (the DB projection already landed at
        // admission), then release the DB so the direct Rust read can take the
        // lbug file lock a live session holds exclusively.
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

        // The partial admission projection survives: the held DB reconstructs the
        // full authored set exactly, with the mutated body.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let reconstructed = db.node_files_from_db().unwrap();
        drop(db);

        let expected_checkout = layers::NodeFile {
            body: "updated through the partial delta".to_string(),
            ..checkout.clone()
        };
        assert_lossless_round_trip(
            &reconstructed,
            &[
                expected_checkout,
                billing.clone(),
                order.clone(),
                ordering.clone(),
                order_id.clone(),
                customer.clone(),
                worker.clone(),
                app_role.clone(),
            ],
        );

        // Direct assertions naming each fidelity the partial path must preserve.
        let find = |layer: &str, node_type: &str, name: &str| -> layers::NodeFile {
            reconstructed
                .iter()
                .find(|n| n.layer == layer && n.node_type == node_type && n.name == name)
                .cloned()
                .unwrap_or_else(|| {
                    panic!("node_files_from_db is missing `{layer}.{node_type}.{name}`")
                })
        };
        let r = find("requirements", "requirement", "checkout");
        assert_eq!(r.body, "updated through the partial delta");
        assert_eq!(
            r.properties,
            props(&[
                ("id", "REQ-1"),
                ("feature", "checkout"),
                ("owner", ""),
                ("empty-meta", "")
            ]),
            "the mutated node's arbitrary + empty-valued keys must survive the partial delta"
        );
        let drives_out = r
            .in_edges
            .iter()
            .find(|e| e.kind == "driven-by" && e.source == "domain.service.billing")
            .cloned()
            .expect("the driven-by in half must survive the partial delta");
        assert_eq!(drives_out.properties, drives_props());
        let requires_in = r
            .in_edges
            .iter()
            .find(|e| e.kind == "requires" && e.source == "requirements.stakeholder.customer")
            .cloned()
            .expect("the restored requires in-edge must survive the partial delta");
        assert_eq!(requires_in.properties, contains_props());

        let b = find("domain", "service", "billing");
        assert_eq!(
            b.properties,
            props(&[("runtime", "rust"), ("empty-meta", "")])
        );
        let drives_in = b
            .out
            .iter()
            .find(|e| e.kind == "driven-by" && e.target == "requirements.requirement.checkout")
            .cloned()
            .expect("the driven-by out half must survive the partial delta");
        assert_eq!(
            drives_in.properties,
            drives_props(),
            "the driven-by properties must survive on BOTH halves"
        );

        let o = find("domain", "entity", "order");
        assert_eq!(
            o.properties,
            props(&[("kind", "event"), ("owner", ""), ("empty-meta", "")])
        );

        testutil::remove(&repo);
    }

    /// Phase-02 task-3: live-session admission resolves the durable node set and
    /// the `implemented-by` code universes SOLELY from the held `db.lbug`.
    ///
    /// While the session is live, BOTH disk-side sources the pre-phase-2
    /// admission path could have leaned on are unavailable: the mutated node's
    /// own `apg/layers/**` file (deleted) and the database's own path on disk
    /// (`db.lbug` renamed aside — the session keeps serving the handle it
    /// already holds). A routed mutation touching the node still admits — it
    /// resolves the node's base content from the held DB, not the gone file —
    /// and its `implemented-by` target validates against the DB's scanned code
    /// universe: the REAL target (`go.fixture.mod.Store`, the scanned struct)
    /// admits, while a bogus target is refused as spec drift, never silently
    /// recorded unchecked/pending (which is what an empty universe would do). A
    /// routed query then sees the projected change.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/process); run via cargo test-e2e"]
    fn session_admission_resolves_the_node_set_and_code_universes_from_the_held_db() {
        let (repo, wt, wt_apg) = testutil::project_with_db("db-only-admission");
        let home = repo.root.join("home");

        // The whole-model-valid base (SPEC §4), plus an `implemented-by` claim on
        // its Component (`order-handler`) whose target is REAL scanned code from
        // the fixture payload (`go.fixture.mod.Store`). The scan projects both
        // into `db.lbug`, so the session holds them when the node file and the
        // database path are later destroyed.
        let mut model = testutil::valid_model();
        model
            .iter_mut()
            .find(|n| n.layer == "solution" && n.node_type == "component")
            .expect("valid_model carries a component")
            .out
            .push(layers::OutEdge {
                kind: "implemented-by".to_string(),
                target: "go.fixture.mod.Store".to_string(),
                properties: NodeProperties::default(),
            });
        layers::write_project(&wt_apg, &model, &[]).unwrap();
        testutil::scan_checkout(&wt).unwrap();

        // The DB precondition admission relies on: the Component is a durable
        // node and the real target is in its scanned code universe.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("solution.component.order-handler"));
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
        // node's file and the database path. The database is renamed aside (an
        // open handle keeps its inode/mmap live on macOS), never written with
        // garbage — the session must serve what it already holds.
        let node_file =
            layers::node_file_path(&wt_apg, Layer::Solution, "component", "order-handler");
        assert!(
            node_file.exists(),
            "the node file must exist before it is deleted"
        );
        std::fs::remove_file(&node_file).unwrap();
        let db_path = wt_apg.join(specs::TRANS).join("db.lbug");
        assert!(
            db_path.exists(),
            "the database must exist before it is moved aside"
        );
        std::fs::rename(
            &db_path,
            wt_apg.join(specs::TRANS).join("db.lbug.held-aside"),
        )
        .unwrap();
        assert!(
            !db_path.exists(),
            "the database path must be gone while the session holds the handle"
        );

        // (1) A routed mutation touching the (file-deleted) node still admits:
        // its base content and its `implemented-by` ref resolve from the held DB.
        // The edge-preserving update re-validates the existing REAL target.
        let update = testutil::ApgCommand::new(&[
            "node",
            "update",
            "solution",
            "component",
            "order-handler",
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
                "MATCH (n:Component {fqn: 'solution.component.order-handler'}) \
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
            "solution.component.order-handler",
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
                "MATCH (n:Component {fqn: 'solution.component.order-handler'})-[:SpecImplementedBy]->(c) \
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

    /// Phase-02 task-15: `apg session save` is the database delta contract over
    /// the held `db.lbug`. Each admission applies its projection to the worktree
    /// database at once, marks exactly the node rows it changed dirty, and
    /// records each removed identity on the pending-removal ledger — while
    /// `apg/layers/**` and git stay at the last saved state. One
    /// `apg session save` then:
    ///   * writes exactly the marked rows at their `apg/layers` node-file paths;
    ///   * removes exactly the ledger identity's node file;
    ///   * leaves every untouched node file byte-identical; and
    ///   * lands the whole set in exactly ONE git commit,
    ///
    /// then drains the pending state — no marked row and no ledger identity —
    /// so the database a following save reads is clean.
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

        // Reads a routed query's data rows through the live session (the CSV
        // header dropped, sorted so the comparison is order-independent).
        let routed_rows = |cypher: &str| -> Vec<String> {
            let out = testutil::spawn_apg(&["query", cypher], &wt);
            assert!(
                out.status.success(),
                "routed read `{cypher}`: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let mut rows: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            rows.sort();
            rows
        };

        // A durable baseline: `survivor` (a later update target), `doomed` (a
        // later delete target) and `bystander` (which the run never touches, so
        // the save must leave its node file byte-identical), added through the
        // session and saved. Lone `global.constraint` anchors keep the model
        // whole-model-valid (SPEC §8.1): the Constraint type carries no
        // cardinality minimums.
        for name in ["survivor", "doomed", "bystander"] {
            run(&[
                "node",
                "add",
                "global",
                "constraint",
                name,
                "--body",
                &format!("{name} baseline"),
            ]);
        }
        let seed_save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            seed_save.status.success(),
            "{}",
            String::from_utf8_lossy(&seed_save.stderr)
        );

        // The save is measured against the saved base: a node file's path
        // relative to `apg/layers/` (the snapshot key, and the same path the
        // save commit records), the base's bytes, commit count and HEAD.
        let layers_root = wt_apg.join(layers::LAYERS_DIR);
        let layer_path = |name: &str| -> String {
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", name)
                .strip_prefix(&layers_root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let snap =
            |root: &Path| -> BTreeMap<String, Vec<u8>> { snapshot(root).into_iter().collect() };
        let saved = snap(&layers_root);
        assert_eq!(
            saved.len(),
            3,
            "the three-node base must be on disk before the run: {saved:?}"
        );
        let commits_before = testutil::commit_count(&wt);
        let saved_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();

        // The run's write set — two adds and an update over a durable node —
        // plus an accepted delete of a durable node. The expected node files are
        // the rows the database holds once admission has projected them.
        let bare = |name: &str, body: &str| layers::NodeFile {
            layer: "global".to_string(),
            node_type: "constraint".to_string(),
            name: name.to_string(),
            body: body.to_string(),
            properties: NodeProperties::default(),
            out: Vec::new(),
            in_edges: Vec::new(),
        };
        let kept_a = bare("kept-a", "kept a");
        let kept_b = bare("kept-b", "kept b");
        let updated_survivor = bare("survivor", "survivor updated");

        run(&[
            "node",
            "add",
            "global",
            "constraint",
            "kept-a",
            "--body",
            "kept a",
        ]);
        run(&[
            "node",
            "add",
            "global",
            "constraint",
            "kept-b",
            "--body",
            "kept b",
        ]);
        run(&[
            "node",
            "update",
            "global",
            "constraint",
            "survivor",
            "--body",
            "survivor updated",
        ]);
        run(&["node", "rm", "global", "constraint", "doomed"]);

        // Before save nothing is durable: `apg/layers/**` is still the saved
        // base and no commit landed.
        assert_eq!(
            snap(&layers_root),
            saved,
            "an admission must not write a node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before,
            "an admission must not create a commit before save"
        );
        assert_eq!(
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string(),
            saved_head,
            "git history must stay at the last saved state until save"
        );

        // The held database's pending state IS save's write and delete set: the
        // run marked exactly the three rows it changed, and left exactly the
        // removed identity on the pending-removal ledger.
        assert_eq!(
            routed_rows("MATCH (n:Constraint) WHERE n.updated = 1 RETURN n.fqn"),
            vec![
                "global.constraint.kept-a",
                "global.constraint.kept-b",
                "global.constraint.survivor",
            ],
            "the run must mark exactly the rows it changed"
        );
        assert_eq!(
            routed_rows("MATCH (n:PendingRemoval) RETURN n.fqn"),
            vec!["global.constraint.doomed"],
            "the delete must leave exactly the removed identity on the ledger"
        );

        // The single durability point: exactly the marked rows at their
        // `apg/layers` paths plus the ledger identity's file, in ONE commit.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );

        // EXACTLY the marked rows were written: the tree differs from the saved
        // base at those paths alone, so every untouched node file is still
        // byte-identical and no file appeared for an untouched identity.
        let after = snap(&layers_root);
        let marked: BTreeSet<String> = ["kept-a", "kept-b", "survivor"]
            .iter()
            .map(|name| layer_path(name))
            .collect();
        let changed: BTreeSet<String> = after
            .iter()
            .filter(|(node_path, bytes)| saved.get(*node_path) != Some(*bytes))
            .map(|(node_path, _)| node_path.to_string())
            .collect();
        assert_eq!(
            changed, marked,
            "save must write exactly the marked rows at their apg/layers paths"
        );

        // … and the ledger identity's file alone was removed.
        let removed: BTreeSet<String> = saved
            .keys()
            .filter(|node_path| !after.contains_key(*node_path))
            .cloned()
            .collect();
        assert_eq!(
            removed,
            BTreeSet::from([layer_path("doomed")]),
            "save must remove exactly the ledger identity's node file"
        );
        assert_eq!(
            after.get(&layer_path("bystander")),
            saved.get(&layer_path("bystander")),
            "every untouched node file must stay byte-identical"
        );

        // Each written node file is the row save serialised: the admitted body
        // lands at its path verbatim.
        for expected in [&kept_a, &kept_b, &updated_survivor] {
            assert_eq!(
                layers::read_node_file(&wt_apg, Layer::Global, "constraint", &expected.name)
                    .unwrap(),
                *expected,
                "the on-disk node file for `{}` must be its database row",
                expected.name
            );
        }

        // The held DB agrees with disk: routed reads report the admitted bodies
        // and the deleted node is gone.
        let assert_body = |fqn: &str, body: &str| {
            let query = format!(
                "MATCH (n:Constraint {{fqn: '{fqn}'}}) WHERE n.body = '{body}' RETURN count(*)"
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
        assert_body("global.constraint.kept-a", "kept a");
        assert_body("global.constraint.kept-b", "kept b");
        assert_body("global.constraint.survivor", "survivor updated");

        let gone = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN count(*)",
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

        // The whole pending set landed in ONE commit: exactly one new
        // single-parent commit whose delta is the marked rows plus the removed
        // file — nothing else.
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 1,
            "the whole pending set must land in exactly one commit"
        );
        let head_repo = git2::Repository::open(&wt).unwrap();
        let head = head_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            head.parent_count(),
            1,
            "the save commit is a normal single-parent commit"
        );
        let parent = head.parent(0).unwrap();
        assert_eq!(
            parent.id().to_string(),
            saved_head,
            "the save commit's parent must be the last saved state"
        );
        let layers_prefix = format!("{}/{}", specs::LAYOUT, layers::LAYERS_DIR);
        let committed: BTreeSet<String> = head_repo
            .diff_tree_to_tree(
                Some(&parent.tree().unwrap()),
                Some(&head.tree().unwrap()),
                None,
            )
            .unwrap()
            .deltas()
            .filter_map(|delta| {
                let path = delta.new_file().path()?;
                let rel = path.strip_prefix(&layers_prefix).ok()?;
                Some(rel.to_string_lossy().into_owned())
            })
            .collect();
        let mut expected_commit = marked.clone();
        expected_commit.insert(layer_path("doomed"));
        assert_eq!(
            committed, expected_commit,
            "the one save commit must carry exactly the marked rows and the removed file"
        );

        // Clean release: save drained the pending state (a pending `end` would
        // refuse).
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

        // The database's pending state is empty afterwards: both the dirty
        // marks and the pending-removal ledger, read back through the same two
        // reads save consumes.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(
            db.dirty_node_files_from_db().unwrap().is_empty(),
            "save must clear every dirty mark"
        );
        assert!(
            db.pending_removals().unwrap().is_empty(),
            "save must empty the pending-removal ledger"
        );
        drop(db);

        testutil::remove(&repo);
    }

    /// Phase-03 task-6: after `apg session save` over a NON-EMPTY buffer, the
    /// durability commit moves HEAD and save's DB `Scan`-row refresh is
    /// observable:
    ///   * the database's own `Scan` row names the NEW HEAD, with the new
    ///     state's cleanliness / content-identity key, read through a routed
    ///     query while the session still owns `db.lbug`;
    ///   * exactly one `Scan` row carries the SAME sha — read through a routed
    ///     query while the session still owns `db.lbug`; and
    ///   * the worktree is NOT stale: the full freshness check (DB `Scan` row +
    ///     authored/transient digest vs the tree) succeeds after the session
    ///     releases, with no rescan.
    ///
    /// This pins save's EXISTING refresh — no new mechanism, no `graph.jsonl`
    /// lead is read or written, and no `apg scan` is run to reach the fresh
    /// state.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/git/process); run via cargo test-e2e"]
    fn session_save_reanchors_scan_meta_to_the_new_head() {
        let (repo, wt, wt_apg) = testutil::project_with_db("session-save-reanchor");
        let home = repo.root.join("home");

        // The scan that built the DB recorded the pre-save HEAD in the DB's own
        // `Scan` row (read before a live session takes the database's lock).
        let before = git::db_recorded_scan(&wt_apg).expect("pre-save DB Scan row");
        let before_head = git::git_state(&wt_apg).sha.expect("pre-save HEAD");
        assert_eq!(
            before.sha, before_head,
            "the fixture scan's DB Scan row must name the pre-save HEAD"
        );
        let commits_before = testutil::commit_count(&wt);

        let session = testutil::start_session_process(&wt, &home);

        // Route one durable mutation: applied to the worktree database at
        // admission (projected) but NOT durable until save. A lone
        // `global.constraint` keeps the model whole-model-valid (SPEC §8.1).
        let add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "global",
            "constraint",
            "reanchored",
            "--body",
            "a buffered durable node",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "routed node add: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before,
            "a buffered mutation must not commit before save"
        );

        // The single durability point: one commit, which moves HEAD.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 1,
            "save must land exactly one durability commit"
        );

        // The commit moved HEAD; capture the new state save re-anchored to.
        let new_state = git::git_state(&wt_apg);
        let new_head = new_state.sha.clone().expect("post-save HEAD");
        assert_ne!(new_head, before_head, "save's commit must move HEAD");

        // (1) The database's own `Scan` row names the NEW HEAD (and the new
        // state's cleanliness / content-identity key), read through a routed
        // query while the session still owns db.lbug — no `graph.jsonl` lead is
        // read or written.
        let row_query = "MATCH (s:Scan) RETURN s.git_sha, s.git_clean, s.content_key";
        let routed_row = testutil::spawn_apg(&["query", row_query], &wt);
        assert!(
            routed_row.status.success(),
            "routed Scan-row read: {}",
            String::from_utf8_lossy(&routed_row.stderr)
        );
        let stdout = String::from_utf8_lossy(&routed_row.stdout);
        let mut stdout_lines = stdout.lines();
        let _header = stdout_lines.next();
        let cells: Vec<&str> = stdout_lines
            .next()
            .expect("the single Scan row")
            .split(',')
            .map(str::trim)
            .collect();
        assert_eq!(
            cells[0], new_head,
            "the database's Scan row must name the new HEAD"
        );
        assert_eq!(
            cells[1],
            if new_state.clean { "true" } else { "false" },
            "the database's Scan row must re-anchor its cleanliness"
        );
        assert_eq!(
            cells[2],
            new_state.content_key.as_deref().unwrap_or(""),
            "the database's Scan row must re-anchor its content-identity key"
        );

        // (2) The HELD DB's own Scan row names the SAME sha, read through a
        // routed query while the session still owns db.lbug.
        let scan_query = format!("MATCH (s:Scan) WHERE s.git_sha = '{new_head}' RETURN count(*)");
        let routed = testutil::spawn_apg(&["query", &scan_query], &wt);
        assert!(
            routed.status.success(),
            "routed Scan read: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the held DB's Scan row must carry the new HEAD"
        );

        // (3) Release the session, then the FULL freshness check (DB `Scan` row +
        // authored/transient digest vs the tree) succeeds — the worktree is not
        // stale and needs no rescan.
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
            !apg::session::live_session(&wt_apg),
            "the session must have released the DB"
        );
        assert!(
            git::is_fresh(&wt_apg),
            "a saved worktree must not be stale after the re-anchor"
        );
        assert!(
            !git::is_stale(&wt_apg),
            "the staleness gate must not report the saved worktree stale"
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-15: the session's PARTIAL DELTA converges to the state a
    /// FULL SCAN REBUILD produces — no residue, no lost incident edge.
    ///
    /// A live session is seeded with durable authored nodes (one carrying an
    /// incoming durable edge from an untouched source and one reviewed by a
    /// `Feedback -[:Reviews]->` pair, plus a plan whose `Gates`/`Satisfies` rows
    /// the start seed projects), then admits N routed mutations spanning every
    /// mutation shape: two adds, an update of the reviewed node, an `edge add`,
    /// an `edge rm`, and a `node rm`. A set of ROUTED queries captures the full
    /// session DB state; `session save` + `session end` make the buffer durable
    /// and a full scan rebuilds `db.lbug` from source. The SAME queries against
    /// the rebuilt DB must return the identical rows — every authored durable
    /// table, their durable edges, and the transient `Feedback`/`Reviews`/
    /// `Gates`/`Satisfies` rows. The named assertions pin the two failure modes:
    /// the `rm` left no residue, and the untouched `requires`/`Reviews` in-edges
    /// on a mutated node survived the partial delta.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/temp dir/git/process); run via cargo test-e2e"]
    fn session_partial_delta_converges_to_a_full_rebuild() {
        let (repo, wt, wt_apg) = testutil::project_with_db("session-db-only-convergence");
        let home = repo.root.join("home");

        let props = |pairs: &[(&str, &str)]| -> NodeProperties {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let out_edge = |kind: &str, target: &str, properties: NodeProperties| layers::OutEdge {
            kind: kind.to_string(),
            target: target.to_string(),
            properties,
        };
        let in_edge = |kind: &str, source: &str, properties: NodeProperties| layers::InEdge {
            kind: kind.to_string(),
            source: source.to_string(),
            properties,
        };
        let nf = |layer: &str,
                  node_type: &str,
                  name: &str,
                  body: &str,
                  out: Vec<layers::OutEdge>,
                  in_edges: Vec<layers::InEdge>| layers::NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: body.to_string(),
            properties: NodeProperties::default(),
            out,
            in_edges,
        };

        // checkout — the node the Feedback reviews and the node every non-add
        // mutation touches. Its incoming `requires` (from the untouched
        // customer) and its incoming `Reviews` (from the Feedback) are the
        // incident edges the partial delta must restore.
        let checkout = nf(
            "requirements",
            "requirement",
            "checkout",
            "checkout original",
            Vec::new(),
            vec![
                in_edge(
                    "requires",
                    "requirements.stakeholder.customer",
                    NodeProperties::default(),
                ),
                in_edge(
                    "driven-by",
                    "domain.service.billing",
                    props(&[("flavor", "map")]),
                ),
                in_edge(
                    "driven-by",
                    "domain.service.scratch",
                    NodeProperties::default(),
                ),
            ],
        );
        let customer = nf(
            "requirements",
            "stakeholder",
            "customer",
            "the customer",
            vec![out_edge(
                "requires",
                "requirements.requirement.checkout",
                NodeProperties::default(),
            )],
            Vec::new(),
        );
        let billing = nf(
            "domain",
            "service",
            "billing",
            "the billing service",
            vec![out_edge(
                "driven-by",
                "requirements.requirement.checkout",
                props(&[("flavor", "map")]),
            )],
            vec![
                in_edge(
                    "contains",
                    "domain.group.ordering",
                    NodeProperties::default(),
                ),
                in_edge(
                    "realises",
                    "solution.component.worker",
                    NodeProperties::default(),
                ),
            ],
        );
        let scratch = nf(
            "domain",
            "service",
            "scratch",
            "a scratch service",
            vec![out_edge(
                "driven-by",
                "requirements.requirement.checkout",
                NodeProperties::default(),
            )],
            vec![
                in_edge(
                    "contains",
                    "domain.group.ordering",
                    NodeProperties::default(),
                ),
                in_edge(
                    "realises",
                    "solution.component.worker",
                    NodeProperties::default(),
                ),
            ],
        );
        // order — an entity owned by the Group and owning a Value, so the
        // seeded model is whole-model-valid (SPEC §8.1).
        let mut order = nf(
            "domain",
            "entity",
            "order",
            "the order event",
            vec![
                out_edge(
                    "contains",
                    "domain.value.order-id",
                    NodeProperties::default(),
                ),
                out_edge("contains", "domain.value.doomed", NodeProperties::default()),
            ],
            vec![in_edge(
                "contains",
                "domain.group.ordering",
                NodeProperties::default(),
            )],
        );
        // An Entity requires `kind` (SPEC §4 node rule), and the routed `rm`
        // rewrites `order`'s file, so it must carry one.
        order
            .properties
            .insert("kind".to_string(), "entity".to_string());
        // The Group owning the services and the entity (a leaf Group holding
        // leaf contents).
        let ordering = nf(
            "domain",
            "group",
            "ordering",
            "the ordering group",
            vec![
                out_edge(
                    "contains",
                    "domain.service.billing",
                    NodeProperties::default(),
                ),
                out_edge(
                    "contains",
                    "domain.service.scratch",
                    NodeProperties::default(),
                ),
                out_edge("contains", "domain.entity.order", NodeProperties::default()),
            ],
            Vec::new(),
        );
        // The Value `order` owns.
        let order_id = nf(
            "domain",
            "value",
            "order-id",
            "the order id",
            Vec::new(),
            vec![in_edge(
                "contains",
                "domain.entity.order",
                NodeProperties::default(),
            )],
        );
        // doomed — owned by `order`; its routed `rm` rewrites `order` to drop
        // the incident `contains` half: the no-residue half.
        let doomed = nf(
            "domain",
            "value",
            "doomed",
            "a doomed value",
            Vec::new(),
            vec![in_edge(
                "contains",
                "domain.entity.order",
                NodeProperties::default(),
            )],
        );
        // A Component realising the two seeded services (the §6 coverage
        // minimum) plus its exactly-one Role.
        let worker = nf(
            "solution",
            "component",
            "worker",
            "the worker component",
            vec![
                out_edge(
                    "realises",
                    "domain.service.billing",
                    NodeProperties::default(),
                ),
                out_edge(
                    "realises",
                    "domain.service.scratch",
                    NodeProperties::default(),
                ),
                out_edge(
                    "is-role",
                    "solution.role.app-service",
                    NodeProperties::default(),
                ),
            ],
            Vec::new(),
        );
        let mut app_role = nf(
            "solution",
            "role",
            "app-service",
            "the application service role",
            Vec::new(),
            vec![in_edge(
                "is-role",
                "solution.component.worker",
                NodeProperties::default(),
            )],
        );
        app_role
            .properties
            .insert("applies-to".to_string(), "component".to_string());

        for node in [
            &checkout, &customer, &billing, &scratch, &order, &ordering, &order_id, &doomed,
            &worker, &app_role,
        ] {
            layers::write_node(&wt_apg, node).unwrap();
        }

        // The transient review pair + the plan's Gates/Satisfies rows are
        // written into `.trans` BEFORE the session starts, so the start seed
        // projects them into the held DB (and the full rebuild re-projects the
        // same records).
        let feedback_path = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        specs::write_jsonl(
            &feedback_path,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "reviewing the checkout requirement".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: "requirements.requirement.checkout".to_string(),
                },
            ],
        )
        .unwrap();
        let plan_path = specs::plan_jsonl_path(&wt_apg, "foo");
        specs::write_jsonl(
            &plan_path,
            &[
                Record::Plan {
                    fqn: "foo/plan".to_string(),
                    title: "Foo".to_string(),
                    strategy: "converge".to_string(),
                },
                Record::PlanPhase {
                    fqn: "foo/plan.phase-01".to_string(),
                    number: 1,
                    title: "P1".to_string(),
                    deliverable: "D1".to_string(),
                    status: "pending".to_string(),
                },
                Record::PlanPhase {
                    fqn: "foo/plan.phase-02".to_string(),
                    number: 2,
                    title: "P2".to_string(),
                    deliverable: "D2".to_string(),
                    status: "pending".to_string(),
                },
                Record::Contains {
                    from: "foo/plan".to_string(),
                    to: "foo/plan.phase-01".to_string(),
                    properties: NodeProperties::default(),
                },
                Record::Contains {
                    from: "foo/plan".to_string(),
                    to: "foo/plan.phase-02".to_string(),
                    properties: NodeProperties::default(),
                },
                Record::Satisfies {
                    from: "foo/plan.phase-01".to_string(),
                    to: "requirements.requirement.checkout".to_string(),
                },
                Record::Gates {
                    from: "foo/plan.phase-02".to_string(),
                    to: "foo/plan.phase-01".to_string(),
                },
            ],
        )
        .unwrap();

        // Commit + scan: the branch DB carries the durable nodes and the
        // transient rows the session's held DB starts from.
        wt_commit_paths(&wt, &["apg/layers"], "seed the convergence node set");
        testutil::scan_checkout(&wt).unwrap();

        // The routed queries whose FULL results the session state and the
        // rebuilt state must agree on: every authored durable table, their
        // durable edges, and the transient Feedback/Reviews/Gates/Satisfies
        // rows.
        let queries: &[&str] = &[
            "MATCH (n:Requirement) RETURN n.fqn, n.body ORDER BY n.fqn",
            "MATCH (n:Stakeholder) RETURN n.fqn, n.body ORDER BY n.fqn",
            "MATCH (n:Entity) RETURN n.fqn, n.body ORDER BY n.fqn",
            "MATCH (n:Value) RETURN n.fqn, n.body ORDER BY n.fqn",
            "MATCH (a:Service)-[r:DrivenBy]->(b:Requirement) RETURN a.fqn, b.fqn ORDER BY a.fqn, b.fqn",
            "MATCH (a:Stakeholder)-[r:Requires]->(b) RETURN a.fqn, b.fqn ORDER BY a.fqn, b.fqn",
            "MATCH (f:Feedback) RETURN f.fqn, f.body, f.status, f.disposition ORDER BY f.fqn",
            "MATCH (f:Feedback)-[r:Reviews]->(t) RETURN f.fqn, t.fqn ORDER BY f.fqn, t.fqn",
            "MATCH (a:PlanPhase)-[r:Gates]->(b) RETURN a.fqn, b.fqn ORDER BY a.fqn, b.fqn",
            "MATCH (a:PlanPhase)-[r:Satisfies]->(b) RETURN a.fqn, b.fqn ORDER BY a.fqn, b.fqn",
        ];

        // One routed durable mutation, asserting success.
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
        // Runs one query, returning its full result rows (header included, empty
        // lines dropped). While the session is live this routes to the held DB;
        // after `end` it reads the rebuilt DB directly — the same query text.
        let read = |q: &str| -> Vec<String> {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect()
        };

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // (1) N routed mutations: adds, an update of the reviewed node (which
        // also carries the untouched incoming `contains`), an `edge add`, an
        // `edge rm`, and a `node rm`.
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "added-req",
            "--body",
            "added requirement",
        ]);
        run(&[
            "node",
            "add",
            "domain",
            "service",
            "added-svc",
            "--body",
            "added service",
        ]);
        run(&[
            "node",
            "update",
            "requirements",
            "requirement",
            "checkout",
            "--body",
            "checkout updated",
        ]);
        run(&[
            "edge",
            "add",
            "driven-by",
            "domain.service.added-svc",
            "requirements.requirement.checkout",
        ]);
        // Keep the added requirement rooted and the added service owned, so the
        // assembled model satisfies the whole-model minimums at save (SPEC §8.1).
        run(&[
            "edge",
            "add",
            "requires",
            "requirements.stakeholder.customer",
            "requirements.requirement.added-req",
        ]);
        run(&[
            "edge",
            "add",
            "contains",
            "domain.group.ordering",
            "domain.service.added-svc",
        ]);
        run(&[
            "edge",
            "add",
            "realises",
            "solution.component.worker",
            "domain.service.added-svc",
        ]);
        run(&[
            "edge",
            "rm",
            "driven-by",
            "domain.service.scratch",
            "requirements.requirement.checkout",
        ]);
        // `scratch` must stay driven by a Requirement after the `driven-by` rm.
        run(&[
            "edge",
            "add",
            "driven-by",
            "domain.service.scratch",
            "requirements.requirement.added-req",
        ]);
        run(&["node", "rm", "domain", "value", "doomed"]);

        // Capture the live session DB state through ROUTED queries.
        let captured: Vec<(&str, Vec<String>)> = queries.iter().map(|q| (*q, read(q))).collect();

        // (2) The single durability point, the session release, then a full scan
        // rebuild of `db.lbug` from source.
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
        assert!(
            !apg::session::live_session(&wt_apg),
            "the session must have released the DB"
        );

        testutil::scan_checkout(&wt).unwrap();

        // (3) The SAME queries against the rebuilt DB must equal the captured
        // session state, row for row: the partial delta converged to the full
        // rebuild on every durable table and every transient Reviews/Gates/
        // Satisfies row.
        for (q, before) in &captured {
            let after = read(q);
            assert_eq!(
                &after, before,
                "the partial delta must converge to the full rebuild for `{q}`"
            );
        }

        // The two failure modes, named directly.
        let gone = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Value {fqn: 'domain.value.doomed'}) RETURN count(*)",
            ],
            &wt,
        );
        assert_eq!(
            String::from_utf8_lossy(&gone.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("0"),
            "the routed rm must leave no residue after the rebuild"
        );
        let incident = testutil::spawn_apg(
            &[
                "query",
                "MATCH (a:Stakeholder)-[:Requires]->(b:Requirement {fqn: 'requirements.requirement.checkout'}) RETURN count(*)",
            ],
            &wt,
        );
        assert_eq!(
            String::from_utf8_lossy(&incident.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the untouched incoming contains edge must survive the partial delta"
        );
        let reviewed = testutil::spawn_apg(
            &[
                "query",
                "MATCH (f:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(b:Requirement {fqn: 'requirements.requirement.checkout'}) RETURN count(*)",
            ],
            &wt,
        );
        assert_eq!(
            String::from_utf8_lossy(&reviewed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the Reviews pairing on the mutated node must survive the partial delta"
        );

        testutil::remove(&repo);
    }

    /// scan-force phase-01 task-4 (acceptance, scratch /tmp repo, CANDIDATE
    /// binary only — `global.constraint.no-real-project-test`): `apg scan
    /// --force` is a TRUE REBUILD. It discards `db.lbug` (with its `.wal`/`.shm`
    /// sidecars) through the one shared discard and rebuilds it from freshly
    /// emitted facts, while every `apg/layers/**` node file and every source
    /// file stays byte-identical and the shared fact store's `scan.json`
    /// survives untouched. The database is the only derived artifact: a scan
    /// writes no `graph.jsonl`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug/facts store); run via cargo test-e2e"]
    fn forced_scan_discards_the_derived_index_and_rebuilds_it_from_scratch() {
        use std::os::unix::fs::MetadataExt;

        let repo = Repo::new("forced-rebuild");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write("main.go", "package main\n\nfunc main() {}\n");
        repo.commit_all("source");
        // A real project worktree (branch `foo`): durable node-file writes are
        // refused on the default branch, and the whole forced rebuild runs here.
        let wt = repo.start_project("foo");
        let apg_root = wt.join(specs::LAYOUT);
        // One durable node file: the authored state the rebuild must preserve.
        // Authored with the single-file writer, not the validated multi-file
        // `write_project`: the latter resolves its code universes from the
        // worktree database, which does not exist before the cold scan — yet the
        // authored `apg/layers/**` node must already be on disk when that scan
        // records the shared fact store's `scan.json` baseline, or the forced
        // rebuild cannot reproduce it byte-identically.
        layers::write_node(
            &apg_root,
            &layers::NodeFile {
                layer: "requirements".to_string(),
                node_type: "requirement".to_string(),
                name: "keep".to_string(),
                body: "survives --force".to_string(),
                properties: std::collections::BTreeMap::new(),
                out: Vec::new(),
                in_edges: Vec::new(),
            },
        )
        .unwrap();

        let run = |args: &[&str]| -> String {
            let out = testutil::ApgCommand::new(args).cwd(&wt).output();
            assert!(
                out.status.success(),
                "{args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stderr).into_owned()
        };

        // The cold scan: a real pipeline run that builds the database and
        // records the shared store's baseline.
        assert!(
            run(&["scan", "."]).contains("[scan] running go frontend"),
            "the cold scan must run the go frontend"
        );
        let trans = apg_root.join(specs::TRANS);
        let db = trans.join("db.lbug");
        assert!(db.exists(), "the cold scan must produce db.lbug");
        assert!(
            !trans.join("graph.jsonl").exists(),
            "a scan writes no graph.jsonl export"
        );

        // Plant the sidecars so their discard is observable whatever lbug
        // leaves behind after a clean close.
        let sentinel: &[u8] = b"planted sidecar";
        std::fs::write(trans.join("db.lbug.wal"), sentinel).unwrap();
        std::fs::write(trans.join("db.lbug.shm"), sentinel).unwrap();

        let layers_before = snapshot(&apg_root.join(layers::LAYERS_DIR));
        assert!(!layers_before.is_empty(), "the node files must exist");
        let sources_before: Vec<(String, Vec<u8>)> =
            ["go.mod", "main.go", ".gitignore", "apg/config.json"]
                .iter()
                .map(|rel| (rel.to_string(), std::fs::read(wt.join(rel)).unwrap()))
                .collect();
        let store = apg::cache::FactStore::resolve(&apg_root).unwrap().root;
        let record = store.join("scan.json");
        let record_before = std::fs::read(&record).unwrap();
        let inode_before = std::fs::metadata(&db).unwrap().ino();

        // The forced rebuild: the pre-force verdict is still printed, then the
        // discard + full pipeline run (no fast path, no warm reuse).
        let err = run(&["scan", ".", "--force"]);
        assert!(
            err.contains("[scan] forced rebuild (--force) — full scan"),
            "the forced reason must be logged: {err}"
        );
        assert!(
            err.contains("[scan] running go frontend"),
            "the forced scan must re-run the frontend: {err}"
        );
        assert!(
            !err.contains("fast-path") && !err.contains("warm cache"),
            "the forced scan must bypass both reuse paths: {err}"
        );

        // The derived artifact is back, `db.lbug` as a NEW file — and the scan
        // writes no export beside it.
        assert!(db.exists(), "the forced scan must rebuild db.lbug");
        assert!(
            !trans.join("graph.jsonl").exists(),
            "a scan writes no graph.jsonl export"
        );
        assert_ne!(
            std::fs::metadata(&db).unwrap().ino(),
            inode_before,
            "db.lbug must be discarded and rebuilt, never reused"
        );
        for sidecar in ["db.lbug.wal", "db.lbug.shm"] {
            let path = trans.join(sidecar);
            assert!(
                !path.exists() || std::fs::read(&path).unwrap() != sentinel,
                "the planted {sidecar} must be discarded, not carried over"
            );
        }

        // The durable node files, every source file and the shared store record
        // are untouched.
        assert_eq!(
            snapshot(&apg_root.join(layers::LAYERS_DIR)),
            layers_before,
            "apg/layers/** must stay byte-identical"
        );
        let sources_after: Vec<(String, Vec<u8>)> =
            ["go.mod", "main.go", ".gitignore", "apg/config.json"]
                .iter()
                .map(|rel| (rel.to_string(), std::fs::read(wt.join(rel)).unwrap()))
                .collect();
        assert_eq!(
            sources_after, sources_before,
            "the scanned sources must stay byte-identical"
        );
        assert_eq!(
            std::fs::read(&record).unwrap(),
            record_before,
            "the shared fact store's scan.json must survive untouched"
        );

        testutil::remove(&repo);
    }

    /// phase-05 task-2: the pre-scan ordering regression. A fresh project
    /// worktree seeds only the `.trans` marker and has no `db.lbug`. The
    /// validated multi-file `write_project` refuses there (its code-identity
    /// source is the database), while the single-file `write_node` authors
    /// `apg/layers/**` without one — and that authored node must survive into
    /// the cold scan's graph.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/cold scan/db.lbug/git); run via cargo test-e2e"]
    fn pre_scan_write_node_lands_for_the_cold_scan_while_write_project_refuses() {
        let repo = Repo::new("pre-scan-write-node");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write("main.go", "package main\n\nfunc main() {}\n");
        repo.commit_all("source");
        let wt = repo.start_project("foo");
        let apg_root = wt.join(specs::LAYOUT);

        // The fresh worktree is DATABASE-LESS by construction: `start_project`
        // seeds only the `.trans` marker, so the refusal below is asserted
        // against a checkout that never had a database.
        let db = apg_root.join(specs::TRANS).join("db.lbug");
        assert!(
            !db.exists(),
            "a fresh project worktree must have no db.lbug"
        );
        let head_before = testutil::commit_count(&wt);

        let nf = |layer: &str, node_type: &str, name: &str, body: &str| layers::NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: body.to_string(),
            properties: NodeProperties::default(),
            out: Vec::new(),
            in_edges: Vec::new(),
        };

        // (1) FIRST, before any scan: a two-file validated model (a component
        // `realises` its service, both halves paired) refuses through
        // `write_project`'s `validate_change` → `artifacts::code_universes`
        // database read, naming the missing database and the remedy — and lands
        // neither node file nor commit.
        let mut comp = nf(
            "solution",
            "component",
            "pre-scan-sys",
            "realises the service",
        );
        comp.out.push(layers::OutEdge {
            kind: "realises".to_string(),
            target: "domain.service.pre-scan-svc".to_string(),
            properties: NodeProperties::default(),
        });
        let mut svc = nf(
            "domain",
            "service",
            "pre-scan-svc",
            "realised by the component",
        );
        svc.in_edges.push(layers::InEdge {
            kind: "realises".to_string(),
            source: "solution.component.pre-scan-sys".to_string(),
            properties: NodeProperties::default(),
        });

        let err = layers::write_project(&apg_root, &[comp, svc], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("db.lbug"),
            "the refusal must name the missing database: {msg}"
        );
        assert!(
            msg.contains("run `apg scan` first"),
            "the refusal must name the remedy: {msg}"
        );
        for path in [
            layers::node_file_path(&apg_root, Layer::Solution, "component", "pre-scan-sys"),
            layers::node_file_path(&apg_root, Layer::Domain, "service", "pre-scan-svc"),
        ] {
            assert!(
                !path.exists(),
                "a refused no-database write must not land {}",
                path.display()
            );
        }
        assert_eq!(
            testutil::commit_count(&wt),
            head_before,
            "a refused no-database write must not commit"
        );

        // (2) THEN the single-file writer authors one valid node without a
        // database and lands it on disk.
        let authored = layers::write_node(
            &apg_root,
            &layers::NodeFile {
                body: "authored before the cold scan".to_string(),
                ..nf("requirements", "requirement", "pre-scan", "")
            },
        )
        .unwrap();
        assert!(
            authored.exists(),
            "write_node must land {} without a database",
            authored.display()
        );
        assert!(
            std::fs::read_to_string(&authored)
                .unwrap()
                .contains("authored before the cold scan"),
            "the authored body must be on disk before the scan"
        );

        // (3) THEN the cold scan builds the database.
        let out = testutil::ApgCommand::new(&["scan", "."]).cwd(&wt).output();
        assert!(
            out.status.success(),
            "the cold scan failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(db.exists(), "the cold scan must produce db.lbug");

        // (4) THEN the cold-built database carries the pre-scan authored node.
        let out = testutil::spawn_apg(
            &[
                "query",
                "MATCH (r:Requirement {fqn: 'requirements.requirement.pre-scan'}) RETURN r.body",
            ],
            &wt,
        );
        assert!(
            out.status.success(),
            "query failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rows = String::from_utf8_lossy(&out.stdout);
        assert!(
            rows.contains("authored before the cold scan"),
            "the cold-built database must carry the authored node: {rows}"
        );

        testutil::remove(&repo);
    }
}
