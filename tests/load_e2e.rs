mod common;

use ::parquet::file::reader::{FileReader, SerializedFileReader};
use apg::graph::{Graph, Location, Node, NodeKind};
use apg::load::*;
use apg::splice::query_rows;
use lbug::{Connection, Database, SystemConfig};

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

/// A fixture graph with one `Scan` node added (a scan that ran in a clean
/// git repo at `abc123`).
fn fixture_graph_with_scan() -> Graph {
    let mut g = fixture_graph();
    g.nodes.insert(
        apg::schema::SCAN_HEAD.to_string(),
        Node {
            kind: NodeKind::Scan,
            git_sha: Some("abc123".to_string()),
            git_clean: Some(true),
            content_key: Some("feedface".to_string()),
            scanned_at: Some("2026-09-07T00:00:00Z".to_string()),
            ..Node::default()
        },
    );
    g
}

/// e2e tier -- real I/O: every test here builds parquet load files or opens
/// `db.lbug` on disk. Each is `#[ignore]`d, so a plain `cargo test` never runs
/// one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn parquet_copy_from_roundtrip() {
        let dir = std::env::temp_dir().join(format!("apg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // One node per durable (`apg/layers/**`) table, so the `updated` column
        // and its clean-base value are observable after the round trip.
        let durable = [
            (
                "Requirement",
                NodeKind::Requirement,
                "requirements.requirement.pinned",
            ),
            ("Note", NodeKind::Note, "implementation.note.pinned"),
            (
                "Stakeholder",
                NodeKind::Stakeholder,
                "requirements.stakeholder.pinned",
            ),
            ("Entity", NodeKind::Entity, "domain.entity.pinned"),
            ("System", NodeKind::System, "solution.system.pinned"),
            (
                "Container",
                NodeKind::Container,
                "solution.container.pinned",
            ),
            (
                "Component",
                NodeKind::Component,
                "solution.component.pinned",
            ),
            ("User", NodeKind::User, "requirements.user.pinned"),
            ("DomainGroup", NodeKind::Group, "domain.group.pinned"),
            ("Value", NodeKind::Value, "domain.value.pinned"),
            ("Service", NodeKind::Service, "domain.service.pinned"),
            ("Person", NodeKind::Person, "solution.person.pinned"),
            ("Actor", NodeKind::Actor, "domain.actor.pinned"),
            ("Role", NodeKind::Role, "solution.role.pinned"),
            (
                "Constraint",
                NodeKind::Constraint,
                "global.constraint.pinned",
            ),
        ];
        let mut graph = fixture_graph();
        for &(_, kind, fqn) in &durable {
            graph.nodes.insert(
                fqn.to_string(),
                Node {
                    kind,
                    name: Some("pinned".to_string()),
                    body: Some(format!("body of {fqn}")),
                    ..Node::default()
                },
            );
        }
        build_load_files(&graph, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        // Node tables loaded with correct columns.
        let out = conn
            .query("MATCH (s:Struct) RETURN s.fqn, s.code_type")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A"), "struct rows: {out}");
        assert!(out.contains("src"), "struct code_type: {out}");

        // `start`/`end` INT64 columns (including the reserved `end` name) and the
        // line columns load.
        let out = conn
            .query("MATCH (s:Struct) WHERE s.fqn = 'mod.A' RETURN s.start, s.`end`, s.start_line, s.end_line")
            .unwrap()
            .to_string();
        assert!(out.contains("0|50|1|50"), "struct span: {out}");

        let out = conn
            .query("MATCH (t:UnresolvedTarget) RETURN t.fqn, t.category")
            .unwrap()
            .to_string();
        assert!(
            out.contains("ext.Foo") && out.contains("external"),
            "unresolved rows: {out}"
        );

        // File node table with line columns (fqn == absolute path).
        let out = conn
            .query("MATCH (f:File) RETURN f.fqn, f.start_line, f.end_line, f.code_type")
            .unwrap()
            .to_string();
        assert!(
            out.contains("/x/a.go") && out.contains("80") && out.contains("src"),
            "file rows: {out}"
        );

        // Multi-pair rel table: Module -> File.
        let out = conn
            .query("MATCH (a:Module)-[:Contains]->(b:File) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("/x/a.go"), "contains Mod->File: {out}");

        // Multi-pair rel table: File -> Struct and File -> Function.
        let out = conn
            .query("MATCH (a:File)-[:Contains]->(b:Struct) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A"), "contains File->Struct: {out}");

        let out = conn
            .query("MATCH (a:File)-[:Contains]->(b:Function) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A.f"), "contains File->Function: {out}");

        // Multi-pair rel table: Struct -> Function.
        let out = conn
            .query("MATCH (a:Struct)-[:Contains]->(b:Function) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A.f"), "contains Struct->Function: {out}");

        // Uses with property-free rel table.
        let out = conn
            .query("MATCH (f:Function)-[:Uses]->(s:Struct) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A"), "uses: {out}");

        // UnresolvedCall with target_type property.
        let out = conn
            .query("MATCH (f:Function)-[r:UnresolvedCall]->(t:UnresolvedTarget) RETURN t.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("ext.Foo"), "unresolved_call: {out}");

        // The durable `updated` dirty flag loads as the clean base (0) and sits
        // LAST in every durable table's DDL: COPY maps parquet columns by
        // position, so the trailing INT64 must line up with the trailing flag.
        for &(label, _, fqn) in &durable {
            let (_, cols) =
                query_rows(&conn, &format!("CALL table_info('{label}') RETURN name")).unwrap();
            assert_eq!(
                cols.last().and_then(|row| row.first()).map(String::as_str),
                Some("updated"),
                "{label} column order: {cols:?}"
            );
            let out = conn
                .query(&format!("MATCH (n:{label}) RETURN n.fqn, n.updated"))
                .unwrap()
                .to_string();
            assert!(out.contains(&format!("{fqn}|0")), "{label} row: {out}");
        }

        // The pending-removal ledger table exists, loads zero rows, and its
        // load file is emitted empty (COPY skips it on a fresh load).
        let out = conn
            .query("MATCH (p:PendingRemoval) RETURN count(*)")
            .unwrap()
            .to_string();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("0"),
            "pending_removal rows: {out}"
        );
        let ledger = dir.join("pending_removal.parquet");
        let reader = SerializedFileReader::new(std::fs::File::open(&ledger).unwrap()).unwrap();
        assert_eq!(
            reader.metadata().file_metadata().num_rows(),
            0,
            "pending_removal load file is not empty"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn graph_jsonl_carries_only_repo_relative_identities() {
        // fix-module-identity task-14: the database carries repo-relative
        // identities only — no absolute checkout path may appear as a
        // File fqn or a Struct/Function `path`. The graph is assembled by
        // the real ingestor over a fake repo base, then loaded into a DB.
        use apg::schema::Record;
        let abs_base = std::env::temp_dir().join(format!(
            "apg-jsonl-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let base_s = abs_base.to_string_lossy().into_owned();
        let file = format!("{base_s}/pkg/a.go");
        let records = vec![
            Record::Module {
                fqn: "pkg".to_string(),
            },
            Record::File {
                path: file.clone(),
                parent: "pkg".to_string(),
                start_line: 1,
                end_line: 9,
            },
            Record::Struct {
                id: "s1".to_string(),
                parent: "pkg".to_string(),
                name: "A".to_string(),
                path: file.clone(),
                start: 0,
                end: 9,
                start_line: 3,
                end_line: 5,
            },
            Record::Function {
                id: "f1".to_string(),
                parent: "pkg".to_string(),
                name: "Leaf".to_string(),
                params: vec![],
                file: file.clone(),
                path: file.clone(),
                start: 7,
                end: 9,
                start_line: 7,
                end_line: 8,
            },
        ];
        let (graph, _) = apg::ingest::ingest(
            records,
            &apg::ingest::IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: Some(&abs_base),
            },
            None,
        );

        // The identities the assembled graph stores are repo-relative: load
        // the graph into a database and assert the File fqn and the
        // Struct/Function paths never carry the absolute scanner path.
        let dir = std::env::temp_dir().join(format!(
            "apg-identity-load-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        build_load_files(&graph, &dir).unwrap();
        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        let out = conn
            .query("MATCH (f:File {fqn: 'pkg/a.go'}) RETURN count(*)")
            .unwrap()
            .to_string();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "relative file fqn: {out}"
        );
        for q in [
            "MATCH (f:File) WHERE f.fqn STARTS WITH '/' RETURN count(*)",
            "MATCH (s:Struct) WHERE s.path STARTS WITH '/' RETURN count(*)",
            "MATCH (f:Function) WHERE f.path STARTS WITH '/' RETURN count(*)",
        ] {
            let out = conn.query(q).unwrap().to_string();
            assert_eq!(out.lines().last().map(str::trim), Some("0"), "{q}: {out}");
        }
        for q in [
            "MATCH (f:File) RETURN f.fqn",
            "MATCH (s:Struct) RETURN s.path",
            "MATCH (f:Function) RETURN f.path",
        ] {
            let out = conn.query(q).unwrap().to_string();
            assert!(
                !out.contains(&base_s),
                "{q} embeds the checkout path: {out}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The edge-modelled transient task round-trips through a real scan: the
    /// `Task` row carries only fqn/title/status, and its `AssignedTo` /
    /// `Implements` / per-kind operation rel rows join the Agent, Component and
    /// live/planned code targets with both endpoints exact.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/fs); run via cargo test-e2e"]
    fn spec_plan_schema_roundtrip() {
        use apg::artifacts::ArtifactDb;
        use apg::testutil::{self, Repo};

        let repo = Repo::new("task-edge-db");
        // The live code an operation edge targets, plus the agent definition
        // record the agent stream emits (the tier-4 Agent node).
        let mut code = testutil::code_payload("fixture.mod", "/abs/store.go", &["Store"]);
        code.push_str(
            "{\"type\":\"agent\",\"name\":\"go-frontend-implementer\",\"path\":\".opencode/agents/go-frontend-implementer.md\",\"start_line\":1,\"end_line\":10}\n",
        );
        repo.write("code/seed.scan.jsonl", &code);
        repo.commit_all("seed code");
        let wt = repo.start_project("task-edge-db");
        let apg_root = wt.join(apg::specs::LAYOUT);
        // A durable Component (with the Requirement/Service spine) gives the
        // `implements` edge a real Solution node to join. The direct write path
        // resolves its code universes from the worktree database, so a fresh
        // empty (but current) database is seeded first; the scan below rebuilds
        // it from the seeded `apg/layers/**` tree and the code payload.
        testutil::touch_db(&apg_root);
        testutil::write_scan_meta(
            &apg_root,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
        testutil::seed_valid_model(&apg_root);

        // The transient plan store: a task carries title+status alone; its
        // assignment, implemented Component, operation kinds and renames
        // destination are edge records written beside it.
        let plan_path = wt
            .join(apg::specs::LAYOUT)
            .join(apg::specs::TRANS)
            .join("plans")
            .join("task-edge-db.jsonl");
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(
            &plan_path,
            concat!(
                "{\"type\":\"plan\",\"fqn\":\"task-edge-db/plan\",\"title\":\"Task edges\",\"strategy\":\"fixture\"}\n",
                "{\"type\":\"plan_phase\",\"fqn\":\"task-edge-db/plan.phase-01\",\"number\":1,\"title\":\"P1\",\"deliverable\":\"D\",\"status\":\"pending\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-edge-db/plan\",\"to\":\"task-edge-db/plan.phase-01\"}\n",
                // task-1: modifies live code, implemented by a real Component.
                "{\"type\":\"task\",\"fqn\":\"task-edge-db/plan.phase-01.task-1\",\"title\":\"Modify live\",\"status\":\"pending\"}\n",
                "{\"type\":\"assigned-to\",\"from\":\"task-edge-db/plan.phase-01.task-1\",\"to\":\"agent.go-frontend-implementer\"}\n",
                "{\"type\":\"implements\",\"from\":\"task-edge-db/plan.phase-01.task-1\",\"to\":\"solution.component.order-handler\"}\n",
                "{\"type\":\"struct_modify\",\"from\":\"task-edge-db/plan.phase-01.task-1\",\"to\":\"go.fixture.mod.Store\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-edge-db/plan.phase-01\",\"to\":\"task-edge-db/plan.phase-01.task-1\"}\n",
                // task-2: creates planned code.
                "{\"type\":\"task\",\"fqn\":\"task-edge-db/plan.phase-01.task-2\",\"title\":\"Create planned\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_create\",\"from\":\"task-edge-db/plan.phase-01.task-2\",\"to\":\"fixture.mod.Planned\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-edge-db/plan.phase-01\",\"to\":\"task-edge-db/plan.phase-01.task-2\"}\n",
                "{\"type\":\"planned_node\",\"fqn\":\"fixture.mod.Planned\",\"kind\":\"struct\",\"name\":\"Planned\"}\n",
                // task-3: renames live code to a planned destination.
                "{\"type\":\"task\",\"fqn\":\"task-edge-db/plan.phase-01.task-3\",\"title\":\"Rename\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_rename\",\"from\":\"task-edge-db/plan.phase-01.task-3\",\"to\":\"go.fixture.mod.Store\"}\n",
                "{\"type\":\"target\",\"from\":\"task-edge-db/plan.phase-01.task-3\",\"to\":\"fixture.mod.Renamed\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-edge-db/plan.phase-01\",\"to\":\"task-edge-db/plan.phase-01.task-3\"}\n",
                "{\"type\":\"planned_node\",\"fqn\":\"fixture.mod.Renamed\",\"kind\":\"struct\",\"name\":\"Renamed\"}\n",
            ),
        )
        .unwrap();

        testutil::scan_checkout(&wt).unwrap();

        let db = ArtifactDb::open(&wt.join(apg::specs::LAYOUT)).unwrap();

        // The Task row carries fqn/title/status only; every retired field has
        // no column, so its old query shape no longer binds.
        let out = db
            .q("MATCH (t:Task) RETURN t.fqn, t.title, t.status ORDER BY t.fqn")
            .unwrap();
        assert!(
            out.contains("task-edge-db/plan.phase-01.task-1")
                && out.contains("Modify live")
                && out.contains("pending"),
            "task rows: {out}"
        );
        for retired in ["t.kind", "t.verb", "t.target", "t.new_fqn"] {
            let q = format!("MATCH (t:Task) RETURN {retired}");
            assert!(
                db.q(&q).is_err(),
                "the retired task property `{retired}` must not bind"
            );
        }

        // The assignment and implementation rel rows join real nodes.
        let out = db
            .q("MATCH (t:Task)-[:AssignedTo]->(a:Agent) RETURN t.fqn, a.fqn")
            .unwrap();
        assert!(
            out.contains("task-edge-db/plan.phase-01.task-1")
                && out.contains("agent.go-frontend-implementer"),
            "assigned-to rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:Implements]->(c:Component) RETURN c.fqn")
            .unwrap();
        assert!(
            out.contains("solution.component.order-handler"),
            "implements rows: {out}"
        );

        // The per-kind operation rows join both live and planned code targets.
        let out = db
            .q("MATCH (t:Task)-[:StructModify]->(s:Struct) RETURN s.fqn")
            .unwrap();
        assert!(
            out.contains("go.fixture.mod.Store"),
            "struct_modify rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:StructCreate]->(s:Struct) RETURN s.fqn, s.status")
            .unwrap();
        assert!(
            out.contains("fixture.mod.Planned") && out.contains("planned"),
            "struct_create planned row: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:StructRename]->(s:Struct) RETURN s.fqn")
            .unwrap();
        assert!(
            out.contains("go.fixture.mod.Store"),
            "struct_rename rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:Target]->(s:Struct) RETURN s.fqn")
            .unwrap();
        assert!(
            out.contains("fixture.mod.Renamed"),
            "renames destination rows: {out}"
        );
        // The database holds the Task row and every operation rel row with
        // both endpoints exact — the live/planned code target and the task
        // that claims it, plus the renames/moves `Target` destination.
        let out = db
            .q("MATCH (t:Task) WHERE t.fqn = 'task-edge-db/plan.phase-01.task-1' RETURN t.title, t.status")
            .unwrap();
        assert!(
            out.contains("Modify live") && out.contains("pending"),
            "task-1 row: {out}"
        );
        let out = db
            .q("MATCH (p:Struct) WHERE p.fqn = 'fixture.mod.Planned' RETURN p.status")
            .unwrap();
        assert!(out.contains("planned"), "planned struct row: {out}");
        for (kind, task, label, target) in [
            (
                "AssignedTo",
                "task-1",
                "Agent",
                "agent.go-frontend-implementer",
            ),
            (
                "Implements",
                "task-1",
                "Component",
                "solution.component.order-handler",
            ),
            ("StructModify", "task-1", "Struct", "go.fixture.mod.Store"),
            ("StructCreate", "task-2", "Struct", "fixture.mod.Planned"),
            ("StructRename", "task-3", "Struct", "go.fixture.mod.Store"),
            ("Target", "task-3", "Struct", "fixture.mod.Renamed"),
        ] {
            let from = format!("task-edge-db/plan.phase-01.{task}");
            let out = db
                .q(&format!(
                    "MATCH (t:Task {{fqn: '{from}'}})-[:{kind}]->(n:{label} {{fqn: '{target}'}}) RETURN count(*)"
                ))
                .unwrap();
            assert_eq!(
                out.lines().last().map(str::trim),
                Some("1"),
                "{kind} {from} -> {target}: {out}"
            );
        }
        drop(db);

        testutil::remove(&repo);
    }

    /// The edge-modelled task projects end to end: a scanned `Task` row carries
    /// fqn/title/status only, each per-kind operation edge and the
    /// renames/moves `target` destination land as rel rows against live and
    /// planned code, and a target-less task keeps an empty edge set. The
    /// retired verb/target/new_fqn columns and the pre-verb record fallback
    /// are gone.
    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn scan_projects_task_verb_target_and_new_fqn_into_the_db() {
        use apg::artifacts::ArtifactDb;
        use apg::testutil::{self, Repo};

        let repo = Repo::new("task-verb-db");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload("fixture.mod", "/abs/store.go", &["Store"]),
        );
        repo.commit_all("seed code");
        let wt = repo.start_project("task-verb-db");
        // The transient plan fixture: one task per operation verb, each its
        // title+status record plus the matching edge, and a target-less task
        // that carries no operation edge at all (the record the retired format
        // defaulted to `creates`).
        let plan_path = wt
            .join(apg::specs::LAYOUT)
            .join(apg::specs::TRANS)
            .join("plans")
            .join("task-verb-db.jsonl");
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(
            &plan_path,
            concat!(
                "{\"type\":\"plan\",\"fqn\":\"task-verb-db/plan\",\"title\":\"Task edges\",\"strategy\":\"fixture\"}\n",
                "{\"type\":\"plan_phase\",\"fqn\":\"task-verb-db/plan.phase-01\",\"number\":1,\"title\":\"P1\",\"deliverable\":\"D\",\"status\":\"pending\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan\",\"to\":\"task-verb-db/plan.phase-01\"}\n",
                // task-1: modifies live code.
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-1\",\"title\":\"Modify live\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_modify\",\"from\":\"task-verb-db/plan.phase-01.task-1\",\"to\":\"go.fixture.mod.Store\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-1\"}\n",
                // task-2: creates planned code.
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-2\",\"title\":\"Create planned\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_create\",\"from\":\"task-verb-db/plan.phase-01.task-2\",\"to\":\"fixture.mod.Planned\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-2\"}\n",
                "{\"type\":\"planned_node\",\"fqn\":\"fixture.mod.Planned\",\"kind\":\"struct\",\"name\":\"Planned\"}\n",
                // task-3: deletes live code.
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-3\",\"title\":\"Delete live\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_delete\",\"from\":\"task-verb-db/plan.phase-01.task-3\",\"to\":\"go.fixture.mod.Store\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-3\"}\n",
                // task-4: moves live code to a planned destination.
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-4\",\"title\":\"Move\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_move\",\"from\":\"task-verb-db/plan.phase-01.task-4\",\"to\":\"go.fixture.mod.Store\"}\n",
                "{\"type\":\"target\",\"from\":\"task-verb-db/plan.phase-01.task-4\",\"to\":\"fixture.mod.Moved\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-4\"}\n",
                "{\"type\":\"planned_node\",\"fqn\":\"fixture.mod.Moved\",\"kind\":\"struct\",\"name\":\"Moved\"}\n",
                // task-5: renames live code to a planned destination.
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-5\",\"title\":\"Rename\",\"status\":\"pending\"}\n",
                "{\"type\":\"struct_rename\",\"from\":\"task-verb-db/plan.phase-01.task-5\",\"to\":\"go.fixture.mod.Store\"}\n",
                "{\"type\":\"target\",\"from\":\"task-verb-db/plan.phase-01.task-5\",\"to\":\"fixture.mod.Renamed\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-5\"}\n",
                "{\"type\":\"planned_node\",\"fqn\":\"fixture.mod.Renamed\",\"kind\":\"struct\",\"name\":\"Renamed\"}\n",
                // task-6: no operation edge — the target-less task.
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-6\",\"title\":\"Untouched\",\"status\":\"pending\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-6\"}\n",
            ),
        )
        .unwrap();

        testutil::scan_checkout(&wt).unwrap();

        let db = ArtifactDb::open(&wt.join(apg::specs::LAYOUT)).unwrap();

        // Every scanned Task row carries fqn/title/status; every retired field
        // has no column, so its old query shape no longer binds.
        let out = db
            .q("MATCH (t:Task) RETURN t.fqn, t.title, t.status ORDER BY t.fqn")
            .unwrap();
        for (i, title) in [
            "Modify live",
            "Create planned",
            "Delete live",
            "Move",
            "Rename",
            "Untouched",
        ]
        .iter()
        .enumerate()
        {
            let fqn = format!("task-verb-db/plan.phase-01.task-{}", i + 1);
            assert!(out.contains(&fqn), "task row {fqn}: {out}");
            assert!(out.contains(title), "task title {title}: {out}");
        }
        assert!(out.contains("pending"), "task status: {out}");
        for retired in ["t.kind", "t.tier", "t.verb", "t.target", "t.new_fqn"] {
            let q = format!("MATCH (t:Task) RETURN {retired}");
            assert!(
                db.q(&q).is_err(),
                "the retired task property `{retired}` must not bind"
            );
        }
        assert!(
            db.q("MATCH (t:Task) RETURN t.verb, t.target, t.new_fqn")
                .is_err(),
            "the old suite query shape must no longer bind"
        );

        // The per-kind operation edges join live and planned code targets.
        let out = db
            .q("MATCH (t:Task)-[:StructModify]->(s:Struct) RETURN t.fqn, s.fqn")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-1")
                && out.contains("go.fixture.mod.Store"),
            "struct_modify rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:StructCreate]->(s:Struct) RETURN t.fqn, s.fqn, s.status")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-2")
                && out.contains("fixture.mod.Planned")
                && out.contains("planned"),
            "struct_create planned row: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:StructDelete]->(s:Struct) RETURN t.fqn, s.fqn")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-3")
                && out.contains("go.fixture.mod.Store"),
            "struct_delete rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:StructMove]->(s:Struct) RETURN t.fqn, s.fqn")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-4")
                && out.contains("go.fixture.mod.Store"),
            "struct_move rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:StructRename]->(s:Struct) RETURN t.fqn, s.fqn")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-5")
                && out.contains("go.fixture.mod.Store"),
            "struct_rename rows: {out}"
        );
        let out = db
            .q("MATCH (t:Task)-[:Target]->(s:Struct) RETURN t.fqn, s.fqn")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-4") && out.contains("fixture.mod.Moved"),
            "moves destination rows: {out}"
        );
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-5")
                && out.contains("fixture.mod.Renamed"),
            "renames destination rows: {out}"
        );

        // A task with no operation record keeps an empty edge set (no
        // synthesised `creates` edge for the pre-verb record).
        let out = db
            .q("MATCH (t:Task {fqn: 'task-verb-db/plan.phase-01.task-6'})-[r]->(n) RETURN count(*)")
            .unwrap();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("0"),
            "target-less task keeps an empty edge set: {out}"
        );
        // The database keeps the same operation/`target` rel rows, with both
        // endpoints exact: the task that claims each live/planned target.
        for (kind, task, target) in [
            ("StructModify", "task-1", "go.fixture.mod.Store"),
            ("StructCreate", "task-2", "fixture.mod.Planned"),
            ("StructDelete", "task-3", "go.fixture.mod.Store"),
            ("StructMove", "task-4", "go.fixture.mod.Store"),
            ("StructRename", "task-5", "go.fixture.mod.Store"),
            ("Target", "task-4", "fixture.mod.Moved"),
            ("Target", "task-5", "fixture.mod.Renamed"),
        ] {
            let from = format!("task-verb-db/plan.phase-01.{task}");
            let out = db
                .q(&format!(
                    "MATCH (t:Task {{fqn: '{from}'}})-[:{kind}]->(s:Struct {{fqn: '{target}'}}) RETURN count(*)"
                ))
                .unwrap();
            assert_eq!(
                out.lines().last().map(str::trim),
                Some("1"),
                "{kind} {from} -> {target}: {out}"
            );
        }
        let out = db
            .q("MATCH (s:Struct) WHERE s.fqn = 'fixture.mod.Planned' RETURN s.status")
            .unwrap();
        assert!(out.contains("planned"), "planned struct row: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    /// The scan metadata is one `Scan` node-table row: the recorded git fields
    /// land in the database exactly once, and the `scan/HEAD` identity is not
    /// duplicated into any other node-label table.
    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn scan_meta_is_the_database_scan_row() {
        let dir = std::env::temp_dir().join(format!("apg-test-scanmeta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph_with_scan();
        build_load_files(&graph, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        // The one Scan row carries every recorded git/content field.
        let out = conn
            .query(
                "MATCH (s:Scan) RETURN s.fqn, s.git_sha, s.git_clean, s.content_key, s.scanned_at",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("scan/HEAD")
                && out.contains("abc123")
                && out.contains("true")
                && out.contains("feedface")
                && out.contains("2026-09-07T00:00:00Z"),
            "scan row: {out}"
        );
        let out = conn
            .query("MATCH (s:Scan) RETURN count(*)")
            .unwrap()
            .to_string();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "scan rows: {out}"
        );
        // The scan identity is stored once, not projected into another table.
        let out = conn
            .query("MATCH (n) WHERE n.fqn = 'scan/HEAD' RETURN count(*)")
            .unwrap()
            .to_string();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "scan identity duplicated across node tables: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn graph_jsonl_roundtrips_tier_nodes_and_spine() {
        // The database is the artefact: every §3.1 catalog node and both halves
        // of every §3.3 hierarchy/spine edge land as rows, and a real `Scan`
        // node loads too.
        let dir =
            std::env::temp_dir().join(format!("apg-test-spine-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut g = fixture_graph_with_scan();
        let sp = |kind: NodeKind| Node {
            kind,
            code_type: String::new(),
            ..Node::default()
        };
        let mut n = |fqn: &str, node: Node| {
            g.nodes.insert(fqn.to_string(), node);
        };
        // The §3.1 catalog (requirements + domain + solution).
        n(
            "requirements.requirement.auth",
            Node {
                id: Some("R1".to_string()),
                title: Some("Users authenticate".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "requirements.stakeholder.ops",
            Node {
                name: Some("ops".to_string()),
                ..sp(NodeKind::Stakeholder)
            },
        );
        n(
            "requirements.user.customer",
            Node {
                name: Some("customer".to_string()),
                ..sp(NodeKind::User)
            },
        );
        n(
            "domain.group.sales",
            Node {
                name: Some("sales".to_string()),
                attribute: Some("core".to_string()),
                ..sp(NodeKind::Group)
            },
        );
        n(
            "domain.entity.user",
            Node {
                name: Some("user".to_string()),
                ..sp(NodeKind::Entity)
            },
        );
        n(
            "domain.value.money",
            Node {
                name: Some("money".to_string()),
                ..sp(NodeKind::Value)
            },
        );
        n(
            "domain.service.checkout",
            Node {
                name: Some("checkout".to_string()),
                ..sp(NodeKind::Service)
            },
        );
        n(
            "solution.system.platform",
            Node {
                name: Some("platform".to_string()),
                ..sp(NodeKind::System)
            },
        );
        n(
            "solution.container.api",
            Node {
                name: Some("api".to_string()),
                sub_kind: Some("app".to_string()),
                ..sp(NodeKind::Container)
            },
        );
        n(
            "solution.component.gateway",
            Node {
                name: Some("gateway".to_string()),
                ..sp(NodeKind::Component)
            },
        );
        n(
            "solution.person.alice",
            Node {
                name: Some("alice".to_string()),
                ..sp(NodeKind::Person)
            },
        );
        n(
            "domain.actor.buyer",
            Node {
                name: Some("buyer".to_string()),
                ..sp(NodeKind::Actor)
            },
        );
        n(
            "solution.role.app-service",
            Node {
                name: Some("app-service".to_string()),
                ..sp(NodeKind::Role)
            },
        );
        // Tier 4: the code the component is implemented by.
        n(
            "mod.Gateway",
            Node {
                kind: NodeKind::Struct,
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        // The §3.3 hierarchy + target-model §4 spine.
        g.contains.extend([
            ("domain.group.sales".into(), "domain.entity.user".into()),
            ("domain.entity.user".into(), "domain.value.money".into()),
            (
                "domain.group.sales".into(),
                "domain.service.checkout".into(),
            ),
            (
                "solution.system.platform".into(),
                "solution.container.api".into(),
            ),
            (
                "solution.container.api".into(),
                "solution.component.gateway".into(),
            ),
        ]);
        g.requires.insert((
            "requirements.stakeholder.ops".to_string(),
            "requirements.requirement.auth".to_string(),
        ));
        g.requires.insert((
            "requirements.user.customer".to_string(),
            "requirements.requirement.auth".to_string(),
        ));
        g.driven_by.insert((
            "domain.service.checkout".to_string(),
            "requirements.requirement.auth".to_string(),
        ));
        g.represents.insert((
            "domain.actor.buyer".to_string(),
            "requirements.user.customer".to_string(),
        ));
        g.represents.insert((
            "solution.person.alice".to_string(),
            "domain.actor.buyer".to_string(),
        ));
        g.uses.insert((
            "solution.person.alice".to_string(),
            "solution.component.gateway".to_string(),
        ));
        g.realises.insert((
            "solution.component.gateway".to_string(),
            "domain.service.checkout".to_string(),
        ));
        g.reads.insert((
            "domain.service.checkout".to_string(),
            "domain.entity.user".to_string(),
        ));
        g.is_role.insert((
            "solution.container.api".to_string(),
            "solution.role.app-service".to_string(),
        ));
        g.spec_implemented_by.insert((
            "solution.component.gateway".to_string(),
            "mod.Gateway".to_string(),
        ));

        // The closed loop lands in a queryable DB: load the assembled graph
        // and resolve every catalog label plus both halves of every edge.
        let ldir = dir.join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        build_load_files(&g, &ldir).unwrap();
        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &ldir).unwrap();

        for (label, fqn) in [
            ("Stakeholder", "requirements.stakeholder.ops"),
            ("User", "requirements.user.customer"),
            ("Requirement", "requirements.requirement.auth"),
            ("DomainGroup", "domain.group.sales"),
            ("Entity", "domain.entity.user"),
            ("Value", "domain.value.money"),
            ("Service", "domain.service.checkout"),
            ("System", "solution.system.platform"),
            ("Container", "solution.container.api"),
            ("Component", "solution.component.gateway"),
            ("Person", "solution.person.alice"),
            ("Actor", "domain.actor.buyer"),
            ("Role", "solution.role.app-service"),
        ] {
            let out = conn
                .query(&format!("MATCH (n:{label} {{fqn: '{fqn}'}}) RETURN n.fqn"))
                .unwrap()
                .to_string();
            assert!(
                !out.contains("(empty)"),
                "{label} {fqn} missing from the database: {out}"
            );
        }
        let out = conn
            .query(
                "MATCH (:Stakeholder)-[:Requires]->(:Requirement)<-[:DrivenBy]-(:Service)<-[:Realises]-(c:Component)-[:SpecImplementedBy]->(impl) RETURN impl.fqn",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("mod.Gateway"),
            "spine to code in the database: {out}"
        );
        let out = conn
            .query("MATCH (s:Scan) RETURN s.git_sha, s.git_clean, s.content_key")
            .unwrap()
            .to_string();
        assert!(
            out.contains("abc123") && out.contains("true") && out.contains("feedface"),
            "scan row: {out}"
        );

        // Both halves of every stored edge: each exact (from, to) pair has a
        // rel row in the database.
        let check = |rel: &str, from: &str, to: &str| {
            let out = conn
                .query(&format!(
                    "MATCH (a {{fqn: '{from}'}})-[:{rel}]->(b {{fqn: '{to}'}}) RETURN count(*)"
                ))
                .unwrap()
                .to_string();
            assert_eq!(
                out.lines().last().map(str::trim),
                Some("1"),
                "{rel} {from} -> {to}: {out}"
            );
        };
        for (from, to) in &g.contains {
            check("Contains", from, to);
        }
        for (from, to) in &g.calls {
            check("Calls", from, to);
        }
        for (from, to) in &g.uses {
            check("Uses", from, to);
        }
        for (from, to, _) in &g.unresolved_calls {
            check("UnresolvedCall", from, to);
        }
        for (from, to) in &g.unresolved_uses {
            check("UnresolvedUse", from, to);
        }
        for (from, to) in &g.requires {
            check("Requires", from, to);
        }
        for (from, to) in &g.driven_by {
            check("DrivenBy", from, to);
        }
        for (from, to) in &g.represents {
            check("Represents", from, to);
        }
        for (from, to) in &g.reads {
            check("Reads", from, to);
        }
        for (from, to) in &g.is_role {
            check("IsRole", from, to);
        }
        for (from, to) in &g.realises {
            check("Realises", from, to);
        }
        for (from, to) in &g.spec_implemented_by {
            check("SpecImplementedBy", from, to);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn agent_label_projects_to_its_table_and_round_trips_through_the_export() {
        // The tier-4 `Agent` label is fqn-keyed but file-shaped: an
        // `agent.<file-stem>` node located at its definition file's
        // repo-relative path and 1-based line span. It projects into the
        // `Agent` node table with that located field set.
        let dir =
            std::env::temp_dir().join(format!("apg-test-agent-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = ".opencode/agents/go-frontend-implementer.md";
        let mut graph = Graph::default();
        graph.nodes.insert(
            "agent.go-frontend-implementer".to_string(),
            Node {
                kind: NodeKind::Agent,
                location: Some(Location {
                    path: path.into(),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 274,
                }),
                code_type: "docs".to_string(),
                ..Node::default()
            },
        );

        // DB projection: the Agent table carries the located field set.
        let ldir = dir.join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        build_load_files(&graph, &ldir).unwrap();
        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &ldir).unwrap();
        let out = conn
            .query("MATCH (a:Agent) RETURN a.fqn, a.path, a.start_line, a.end_line, a.code_type")
            .unwrap()
            .to_string();
        assert!(
            out.contains(&format!("agent.go-frontend-implementer|{path}|1|274|docs")),
            "agent row: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/fs); run via cargo test-e2e"]
    fn scan_node_loads_into_db() {
        let dir = std::env::temp_dir().join(format!("apg-test-scandb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph_with_scan();
        build_load_files(&graph, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        let out = conn
            .query(
                "MATCH (s:Scan) RETURN s.fqn, s.git_sha, s.git_clean, s.content_key, s.scanned_at",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("scan/HEAD") && out.contains("abc123") && out.contains("true"),
            "scan rows: {out}"
        );
        assert!(
            out.contains("feedface"),
            "the content-identity key must round-trip into the DB Scan node: {out}"
        );
        assert!(
            out.contains("2026-09-07T00:00:00Z"),
            "scan scanned_at: {out}"
        );

        // A graph without a Scan node still loads (empty Scan table).
        let dir2 = std::env::temp_dir().join(format!("apg-test-scandb2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir2);
        std::fs::create_dir_all(&dir2).unwrap();
        build_load_files(&fixture_graph(), &dir2).unwrap();
        let db2 = Database::in_memory(SystemConfig::default()).unwrap();
        let conn2 = Connection::new(&db2).unwrap();
        create_schema(&conn2).unwrap();
        copy_from(&conn2, &dir2).unwrap();
        let out2 = conn2
            .query("MATCH (s:Scan) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out2.contains("0"), "expected empty Scan table, got: {out2}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
