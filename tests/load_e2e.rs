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
            None,
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

    /// The edge-modelled transient task round-trips through a real scan: the
    /// `Task` row carries only fqn/title/status, its `AssignedTo` /
    /// `Implements` / per-kind operation rel rows join the Agent, Component and
    /// live/planned code targets, and `graph.jsonl` emits the same record and
    /// edges the database holds.
    #[test]
    #[ignore = "e2e tier: real I/O (db.lbug/graph.jsonl/fs); run via cargo test-e2e"]
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
        // A durable Component (with the Requirement/Service spine) gives the
        // `implements` edge a real Solution node to join.
        testutil::seed_valid_model(&wt.join(apg::specs::LAYOUT));

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
        drop(db);

        // graph.jsonl agrees with the database: the same Task record (no
        // retired keys) and the same edge rows.
        let export = wt
            .join(apg::specs::LAYOUT)
            .join(apg::specs::TRANS)
            .join("graph.jsonl");
        let text = std::fs::read_to_string(&export).unwrap();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("type").and_then(|t| t.as_str()) == Some("task") {
                for retired in ["kind", "tier", "verb", "target", "new_fqn"] {
                    assert!(
                        v.get(retired).is_none(),
                        "exported task carries the retired `{retired}` key: {line}"
                    );
                }
            }
        }
        let back = read_graph_jsonl(&export).unwrap();
        let t1 = back
            .nodes
            .get("task-edge-db/plan.phase-01.task-1")
            .expect("task-1 in graph.jsonl");
        assert_eq!(t1.kind, NodeKind::Task);
        assert_eq!(t1.title.as_deref(), Some("Modify live"));
        assert_eq!(t1.status.as_deref(), Some("pending"));
        assert!(
            back.assigned_to.contains(&(
                "task-edge-db/plan.phase-01.task-1".to_string(),
                "agent.go-frontend-implementer".to_string()
            )),
            "assigned-to export edge"
        );
        assert!(
            back.implements.contains(&(
                "task-edge-db/plan.phase-01.task-1".to_string(),
                "solution.component.order-handler".to_string()
            )),
            "implements export edge"
        );
        assert!(
            back.struct_modify.contains(&(
                "task-edge-db/plan.phase-01.task-1".to_string(),
                "go.fixture.mod.Store".to_string()
            )),
            "struct_modify export edge"
        );
        assert!(
            back.struct_create.contains(&(
                "task-edge-db/plan.phase-01.task-2".to_string(),
                "fixture.mod.Planned".to_string()
            )),
            "struct_create export edge"
        );
        assert!(
            back.struct_rename.contains(&(
                "task-edge-db/plan.phase-01.task-3".to_string(),
                "go.fixture.mod.Store".to_string()
            )),
            "struct_rename export edge"
        );
        assert!(
            back.target.contains(&(
                "task-edge-db/plan.phase-01.task-3".to_string(),
                "fixture.mod.Renamed".to_string()
            )),
            "target export edge"
        );

        testutil::remove(&repo);
    }

    /// The edge-modelled task projects end to end: a scanned `Task` row carries
    /// fqn/title/status only, each per-kind operation edge and the
    /// renames/moves `target` destination land as rel rows against live and
    /// planned code, a target-less task keeps an empty edge set, and
    /// `graph.jsonl` emits the same record and edges. The retired
    /// verb/target/new_fqn columns and the pre-verb record fallback are gone.
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
        drop(db);

        // graph.jsonl agrees with the database: the same Task record (no
        // retired keys) and the same operation/`target` edges.
        let export = wt
            .join(apg::specs::LAYOUT)
            .join(apg::specs::TRANS)
            .join("graph.jsonl");
        let text = std::fs::read_to_string(&export).unwrap();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("type").and_then(|t| t.as_str()) == Some("task") {
                for retired in ["kind", "tier", "verb", "target", "new_fqn"] {
                    assert!(
                        v.get(retired).is_none(),
                        "exported task carries the retired `{retired}` key: {line}"
                    );
                }
            }
        }
        let back = read_graph_jsonl(&export).unwrap();
        for i in 1..=6 {
            let fqn = format!("task-verb-db/plan.phase-01.task-{i}");
            let node = back.nodes.get(&fqn).expect("task in graph.jsonl");
            assert_eq!(node.kind, NodeKind::Task, "task node kind: {fqn}");
            assert_eq!(
                node.status.as_deref(),
                Some("pending"),
                "task status: {fqn}"
            );
        }
        assert_eq!(
            back.nodes["task-verb-db/plan.phase-01.task-1"]
                .title
                .as_deref(),
            Some("Modify live")
        );
        let planned = back
            .nodes
            .get("fixture.mod.Planned")
            .expect("planned struct in graph.jsonl");
        assert_eq!(planned.kind, NodeKind::Struct);
        assert_eq!(planned.status.as_deref(), Some("planned"));
        for (set, task, target) in [
            (&back.struct_modify, "task-1", "go.fixture.mod.Store"),
            (&back.struct_create, "task-2", "fixture.mod.Planned"),
            (&back.struct_delete, "task-3", "go.fixture.mod.Store"),
            (&back.struct_move, "task-4", "go.fixture.mod.Store"),
            (&back.struct_rename, "task-5", "go.fixture.mod.Store"),
            (&back.target, "task-4", "fixture.mod.Moved"),
            (&back.target, "task-5", "fixture.mod.Renamed"),
        ] {
            let from = format!("task-verb-db/plan.phase-01.{task}");
            assert!(
                set.contains(&(from.clone(), target.to_string())),
                "exported {task} edge to {target}"
            );
        }

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
            ("driven-by", &back.driven_by, &g.driven_by),
            ("represents", &back.represents, &g.represents),
            ("requires", &back.requires, &g.requires),
            ("reads", &back.reads, &g.reads),
            ("is_role", &back.is_role, &g.is_role),
            ("realises", &back.realises, &g.realises),
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
            ("Actor", "domain.actor.buyer"),
            ("Role", "solution.role.app-service"),
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
                "MATCH (:Stakeholder)-[:Requires]->(:Requirement)<-[:DrivenBy]-(:Service)<-[:Realises]-(c:Component)-[:SpecImplementedBy]->(impl) RETURN impl.fqn",
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
    fn agent_label_projects_to_its_table_and_round_trips_through_the_export() {
        // The tier-4 `Agent` label is fqn-keyed but file-shaped: an
        // `agent.<file-stem>` node located at its definition file's
        // repo-relative path and 1-based line span. It projects to the `Agent`
        // node table and round-trips through graph.jsonl's `agent` record.
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

        // Export: one `agent` record with the same located field set — not the
        // fqn-only `language` arm, and not skipped.
        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&graph, &out_path).unwrap();
        let text = std::fs::read_to_string(&out_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(record["type"], "agent");
        assert_eq!(record["fqn"], "agent.go-frontend-implementer");
        assert_eq!(record["path"], path);
        assert_eq!(record["start_line"], 1);
        assert_eq!(record["end_line"], 274);
        assert_eq!(record["code_type"], "docs");

        // Read back: the shared export reader rebuilds the located Agent node.
        let back = read_graph_jsonl(&out_path).unwrap();
        let node = back
            .nodes
            .get("agent.go-frontend-implementer")
            .expect("Agent node lost in round-trip");
        assert_eq!(node.kind, NodeKind::Agent);
        let location = node.location.as_ref().expect("Agent node is located");
        assert_eq!(location.path, std::path::PathBuf::from(path));
        assert_eq!((location.start_line, location.end_line), (1, 274));
        assert_eq!(node.code_type, "docs");

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
