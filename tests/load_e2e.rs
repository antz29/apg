mod common;

use apg::graph::{Graph, Location, Node, NodeKind};
use apg::load::*;
use apg::testutil::read_graph_jsonl;
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

/// e2e tier -- real I/O: every test here builds parquet load files, opens
/// `db.lbug` or writes/reads `graph.jsonl` on disk. Each is `#[ignore]`d, so
/// a plain `cargo test` never runs one; the only entry point is the named
/// guard `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn parquet_copy_from_roundtrip() {
        let dir = std::env::temp_dir().join(format!("apg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph();
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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn graph_jsonl_carries_only_repo_relative_identities() {
        // fix-module-identity task-14: the export carries repo-relative
        // identities only — no absolute checkout path may appear as a
        // File fqn or a Struct/Function `path`. The graph is assembled by
        // the real ingestor over a fake repo base, then serialized.
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
        );

        let out = std::env::temp_dir().join(format!(
            "apg-jsonl-identity-out-{}.jsonl",
            std::process::id()
        ));
        write_graph_jsonl(&graph, &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();

        // The File fqn and the Struct/Function paths are the repo-relative
        // identity — never the absolute scanner path.
        assert!(
            text.contains("\"fqn\":\"pkg/a.go\""),
            "relative file fqn: {text}"
        );
        assert!(!text.contains(&base_s), "no absolute checkout path: {text}");
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let located = match ty {
                "file" => v.get("fqn").and_then(|f| f.as_str()).map(str::to_string),
                "struct" | "function" => v.get("path").and_then(|p| p.as_str()).map(str::to_string),
                _ => None,
            };
            if let Some(p) = located {
                assert!(!p.starts_with('/'), "{ty} identity has no leading `/`: {p}");
                assert!(
                    !p.contains(&base_s),
                    "{ty} identity embeds no checkout path: {p}"
                );
            }
        }

        let _ = std::fs::remove_file(&out);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn spec_plan_schema_roundtrip() {
        // A plan + requirement graph survives the PARQUET load path: node
        // tables with their own columns, multi-pair Contains, and the
        // spec/plan rel tables (one COPY per pair, empty pairs too).
        let mut g = Graph::default();
        let sp = |kind: NodeKind| Node {
            kind,
            code_type: String::new(),
            ..Node::default()
        };
        let mut n = |fqn: &str, node: Node| {
            g.nodes.insert(fqn.to_string(), node);
        };
        n(
            "foo/plan",
            Node {
                title: Some("Plan".to_string()),
                strategy: Some("Layer-first".to_string()),
                ..sp(NodeKind::Plan)
            },
        );
        n(
            "foo/plan.phase-1",
            Node {
                number: Some(1),
                title: Some("P1".to_string()),
                deliverable: Some("Core".to_string()),
                ..sp(NodeKind::PlanPhase)
            },
        );
        n(
            "foo/plan.phase-1.task-1",
            Node {
                title: Some("Add RootStore".to_string()),
                sub_kind: Some("source".to_string()),
                status: Some("pending".to_string()),
                verb: Some("renames".to_string()),
                target: Some("foo.Old".to_string()),
                new_fqn: Some("foo.New".to_string()),
                ..sp(NodeKind::Task)
            },
        );
        // An old-format Task node (pre-verb shape): no verb/target/new_fqn.
        n(
            "foo/plan.phase-1.task-2",
            Node {
                title: Some("Legacy".to_string()),
                sub_kind: Some("source".to_string()),
                status: Some("pending".to_string()),
                ..sp(NodeKind::Task)
            },
        );
        n(
            "foo/spec.R1",
            Node {
                id: Some("R1".to_string()),
                title: Some("Timer".to_string()),
                feature: Some("feature-a".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "foo/gateway",
            Node {
                name: Some("Gateway".to_string()),
                status: Some("planned".to_string()),
                ..sp(NodeKind::Struct)
            },
        );
        n(
            "foo/note-1",
            Node {
                body: Some("Background prose".to_string()),
                sub_kind: Some("background".to_string()),
                ..sp(NodeKind::Note)
            },
        );
        n(
            "foo/feedback-1",
            Node {
                body: Some("Split R1".to_string()),
                status: Some("open".to_string()),
                ..sp(NodeKind::Feedback)
            },
        );

        g.contains.extend([
            ("foo/plan".into(), "foo/plan.phase-1".into()),
            ("foo/plan.phase-1".into(), "foo/plan.phase-1.task-1".into()),
        ]);
        g.details.insert(("foo/note-1".into(), "foo/plan".into()));
        g.reviews
            .insert(("foo/feedback-1".into(), "foo/spec.R1".into()));
        g.depends_on
            .insert(("foo/spec.R1".into(), "foo/spec.R1".into()));
        g.satisfies
            .insert(("foo/plan.phase-1".into(), "foo/spec.R1".into()));

        let dir = std::env::temp_dir().join(format!("apg-test-spec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        build_load_files(&g, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        // Node tables carry their own columns.
        let out = conn
            .query("MATCH (p:Plan) RETURN p.fqn, p.title, p.strategy")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan") && out.contains("Layer-first"),
            "plan rows: {out}"
        );
        let out = conn
            .query("MATCH (r:Requirement) RETURN r.id, r.feature")
            .unwrap()
            .to_string();
        assert!(
            out.contains("R1") && out.contains("feature-a"),
            "req rows: {out}"
        );
        let out = conn
            .query("MATCH (p:PlanPhase) RETURN p.number, p.deliverable")
            .unwrap()
            .to_string();
        assert!(
            out.contains("1") && out.contains("Core"),
            "plan phase rows: {out}"
        );
        let out = conn
            .query("MATCH (t:Task) RETURN t.kind, t.status")
            .unwrap()
            .to_string();
        assert!(
            out.contains("source") && out.contains("pending"),
            "task rows: {out}"
        );
        // The Task table carries the Task→Implementation verb fields the suite
        // tools query (apg_plan_tasks/apg_plan): verb/target/new_fqn.
        let out = conn
            .query("MATCH (t:Task) RETURN t.fqn, t.verb, t.target, t.new_fqn ORDER BY t.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan.phase-1.task-1")
                && out.contains("renames")
                && out.contains("foo.Old")
                && out.contains("foo.New"),
            "task verb rows: {out}"
        );
        // An old-format Task (no verb fields) projects empty strings, not a
        // binder error.
        let out = conn
            .query("MATCH (t:Task {fqn: 'foo/plan.phase-1.task-2'}) RETURN t.verb, t.target, t.new_fqn")
            .unwrap()
            .to_string();
        assert!(
            !out.contains("(empty)"),
            "old-format task must still be a row: {out}"
        );
        let out = conn
            .query("MATCH (t:Task) RETURN t.verb, t.target, t.new_fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("renames") && out.contains("foo.New"),
            "suite query shape must bind: {out}"
        );
        let out = conn
            .query("MATCH (s:Struct {fqn: 'foo/gateway'}) RETURN s.status")
            .unwrap()
            .to_string();
        assert!(out.contains("planned"), "planned struct row: {out}");

        // Multi-pair Contains: Plan -> PlanPhase -> Task.
        let out = conn
            .query("MATCH (p:Plan)-[:Contains]->(pp:PlanPhase) RETURN pp.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan.phase-1"),
            "contains plan->phase: {out}"
        );

        // Spec/plan rel tables.
        let out = conn
            .query("MATCH (:Note)-[:Details]->(p:Plan) RETURN p.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/plan"), "details: {out}");
        let out = conn
            .query("MATCH (:Feedback)-[:Reviews]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "reviews: {out}");
        let out = conn
            .query("MATCH (a:Requirement)-[:DependsOn]->(b:Requirement) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "depends_on: {out}");
        let out = conn
            .query("MATCH (:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "satisfies: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The suite tools (`apg_plan_tasks`/`apg_plan`) query
    /// `MATCH (t:Task) RETURN t.verb, t.target, t.new_fqn`; the DB projection
    /// must carry those columns end to end. A plan fixture in the project
    /// worktree's `.trans/plans` is scanned through the real fixture pipeline
    /// (ingest → parquet → COPY), the exact suite query shapes bind, an
    /// old-format Task record (no verb keys) inserts with its serde defaults
    /// (`verb = "creates"`, empty target/new_fqn), and graph.jsonl round-trips
    /// the fields.
    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
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
        // The transient plan fixture: one full verb/target/new_fqn task plus an
        // old-format record with no verb keys at all.
        let plan_path = wt
            .join(apg::specs::LAYOUT)
            .join(apg::specs::TRANS)
            .join("plans")
            .join("task-verb-db.jsonl");
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(
            &plan_path,
            concat!(
                "{\"type\":\"plan\",\"fqn\":\"task-verb-db/plan\",\"title\":\"Task verb\",\"strategy\":\"fixture\"}\n",
                "{\"type\":\"plan_phase\",\"fqn\":\"task-verb-db/plan.phase-01\",\"number\":1,\"title\":\"P1\",\"deliverable\":\"D\",\"status\":\"pending\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan\",\"to\":\"task-verb-db/plan.phase-01\"}\n",
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-1\",\"title\":\"Rename\",\"kind\":\"source\",\"tier\":\"\",\"status\":\"pending\",\"verb\":\"renames\",\"target\":\"fixture.mod.Old\",\"new_fqn\":\"fixture.mod.New\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-1\"}\n",
                "{\"type\":\"task\",\"fqn\":\"task-verb-db/plan.phase-01.task-2\",\"title\":\"Legacy\",\"kind\":\"source\",\"tier\":\"\",\"status\":\"pending\"}\n",
                "{\"type\":\"contains\",\"from\":\"task-verb-db/plan.phase-01\",\"to\":\"task-verb-db/plan.phase-01.task-2\"}\n",
            ),
        )
        .unwrap();

        testutil::scan_checkout(&wt).unwrap();

        let db = ArtifactDb::open(&wt.join(apg::specs::LAYOUT)).unwrap();
        // The exact suite query shapes must bind (the defect was a binder
        // error: `Cannot find property verb for t`).
        let out = db
            .q("MATCH (t:Task) RETURN t.fqn, t.verb, t.target, t.new_fqn ORDER BY t.fqn")
            .unwrap();
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-1")
                && out.contains("renames")
                && out.contains("fixture.mod.Old")
                && out.contains("fixture.mod.New"),
            "task verb projection: {out}"
        );
        assert!(
            out.contains("task-verb-db/plan.phase-01.task-2") && out.contains("creates"),
            "old-format task defaults to creates: {out}"
        );
        let out = db
            .q("MATCH (t:Task) RETURN t.verb, t.target, t.new_fqn")
            .unwrap();
        assert!(
            out.contains("renames") && out.contains("fixture.mod.New"),
            "suite query shape: {out}"
        );
        drop(db);

        // The export (graph.jsonl) round-trips the new fields; an old-format
        // task stays empty on target/new_fqn.
        let back = read_graph_jsonl(
            &wt.join(apg::specs::LAYOUT)
                .join(apg::specs::TRANS)
                .join("graph.jsonl"),
        )
        .unwrap();
        let t1 = back
            .nodes
            .get("task-verb-db/plan.phase-01.task-1")
            .expect("task-1 in graph.jsonl");
        assert_eq!(t1.verb.as_deref(), Some("renames"));
        assert_eq!(t1.target.as_deref(), Some("fixture.mod.Old"));
        assert_eq!(t1.new_fqn.as_deref(), Some("fixture.mod.New"));
        let t2 = back
            .nodes
            .get("task-verb-db/plan.phase-01.task-2")
            .expect("task-2 in graph.jsonl");
        assert_eq!(t2.verb.as_deref(), Some("creates"));
        assert_eq!(t2.target, None);
        assert_eq!(t2.new_fqn, None);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn graph_jsonl_is_valid_and_self_contained() {
        let dir = std::env::temp_dir().join(format!("apg-test-jsonl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph();
        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&graph, &out_path).unwrap();

        let text = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            graph.nodes.len()
                + graph.contains.len()
                + graph.calls.len()
                + graph.uses.len()
                + graph.unresolved_calls.len()
                + graph.unresolved_uses.len()
        );
        // Every line is valid JSON with a `type` discriminator.
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("type").is_some(), "missing type: {line}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn scan_meta_is_graph_jsonl_line_one() {
        let dir = std::env::temp_dir().join(format!("apg-test-scanmeta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph_with_scan();
        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&graph, &out_path).unwrap();

        let text = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Line 1 is the scan_meta control record with the git fields.
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "scan_meta");
        assert_eq!(first["git_sha"], "abc123");
        assert_eq!(first["git_clean"], true);
        assert_eq!(first["content_key"], "feedface");
        assert_eq!(first["scanned_at"], "2026-09-07T00:00:00Z");
        assert!(
            first.get("fqn").is_none(),
            "scan_meta is a control record, not a node"
        );
        // The Scan node is exported exactly once (line 1), not again as a node.
        assert_eq!(
            lines.len(),
            graph.nodes.len()
                + graph.contains.len()
                + graph.calls.len()
                + graph.uses.len()
                + graph.unresolved_calls.len()
                + graph.unresolved_uses.len()
        );
        // No later line is a scan node record.
        for line in &lines[1..] {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_ne!(v["type"], "scan", "scan node leaked into export: {line}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
    fn graph_jsonl_roundtrips_tier_nodes_and_spine() {
        // The export is round-trippable: write_graph_jsonl → read_graph_jsonl →
        // fresh DB, and every §3.1 catalog node + §3.3 spine edge survives. A
        // real `Scan` node round-trips too (scan_meta control record on line 1).
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
        // Tier 4: the code the component is implemented by.
        n(
            "mod.Gateway",
            Node {
                kind: NodeKind::Struct,
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        // The §3.3 hierarchy + spine.
        g.contains.extend([
            (
                "requirements.stakeholder.ops".into(),
                "requirements.requirement.auth".into(),
            ),
            (
                "requirements.user.customer".into(),
                "requirements.requirement.auth".into(),
            ),
            ("domain.group.sales".into(), "domain.entity.user".into()),
            ("domain.group.sales".into(), "domain.value.money".into()),
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
        g.drives.insert((
            "requirements.requirement.auth".to_string(),
            "domain.group.sales".to_string(),
        ));
        g.represents.insert((
            "requirements.user.customer".to_string(),
            "domain.entity.user".to_string(),
        ));
        g.represents.insert((
            "domain.entity.user".to_string(),
            "solution.person.alice".to_string(),
        ));
        g.realised_by.insert((
            "domain.group.sales".to_string(),
            "solution.system.platform".to_string(),
        ));
        g.spec_implemented_by.insert((
            "solution.component.gateway".to_string(),
            "mod.Gateway".to_string(),
        ));

        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&g, &out_path).unwrap();

        // Re-ingest the export: read_graph_jsonl rebuilds the graph (scan_meta
        // line 1 → the Scan node, catalog nodes, Contains + every spine edge).
        let back = read_graph_jsonl(&out_path).unwrap();

        // Every node survives with its kind.
        for (fqn, node) in &g.nodes {
            let seen = back
                .nodes
                .get(fqn)
                .unwrap_or_else(|| panic!("{fqn} lost in round-trip"));
            assert_eq!(seen.kind, node.kind, "{fqn} kind");
        }
        assert_eq!(back.nodes.len(), g.nodes.len(), "node count");
        // The Scan node's content-identity key round-trips through line 1 too.
        assert_eq!(
            back.nodes[apg::schema::SCAN_HEAD].content_key.as_deref(),
            Some("feedface"),
            "scan_meta content_key lost in round-trip"
        );
        // Edge sets are identical — nothing projected away.
        assert_eq!(back.contains, g.contains, "contains edges");
        for (name, a, b) in [
            ("drives", &back.drives, &g.drives),
            ("represents", &back.represents, &g.represents),
            ("realised_by", &back.realised_by, &g.realised_by),
            (
                "spec_implemented_by",
                &back.spec_implemented_by,
                &g.spec_implemented_by,
            ),
            ("calls", &back.calls, &g.calls),
            ("uses", &back.uses, &g.uses),
            ("unresolved_uses", &back.unresolved_uses, &g.unresolved_uses),
        ] {
            assert_eq!(a, b, "{name} edges");
        }
        assert_eq!(
            back.unresolved_calls, g.unresolved_calls,
            "unresolved_calls edges"
        );

        // The closed loop lands in a queryable DB: load the re-ingested graph
        // into a fresh DB and resolve every catalog label + the spine end to end.
        let ldir = dir.join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        build_load_files(&back, &ldir).unwrap();
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
        ] {
            let out = conn
                .query(&format!("MATCH (n:{label} {{fqn: '{fqn}'}}) RETURN n.fqn"))
                .unwrap()
                .to_string();
            assert!(
                !out.contains("(empty)"),
                "{label} {fqn} missing after round-trip: {out}"
            );
        }
        let out = conn
            .query(
                "MATCH (:Requirement)-[:Drives]->(g:DomainGroup)-[:RealisedBy]->(:System)-[:Contains]->(:Container)-[:Contains]->(c:Component)-[:SpecImplementedBy]->(impl) RETURN impl.fqn",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("mod.Gateway"),
            "spine to code after round-trip: {out}"
        );
        let out = conn
            .query("MATCH (s:Scan) RETURN s.git_sha, s.git_clean")
            .unwrap()
            .to_string();
        assert!(
            out.contains("abc123") && out.contains("true"),
            "scan survived round-trip: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (parquet/db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
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
