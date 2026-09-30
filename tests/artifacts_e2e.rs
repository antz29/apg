mod common;

use apg::artifacts::*;
use apg::git;
use apg::graph::{Graph, Location, Node, NodeKind};
use apg::layers::{self, Layer};
use apg::load;
use apg::schema::Record;
use apg::specs;
use apg::testutil::{self, Repo};
use common::wt_commit_paths;
use lbug::{Connection, Database};
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
        },
        Record::Note {
            fqn: "foo/note-1".into(),
            body: "first".into(),
            kind: "background".into(),
        },
        Record::Details {
            from: "foo/note-1".into(),
            to: "foo/plan".into(),
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
        });
        mutated.push(Record::Details {
            from: "foo/note-2".into(),
            to: "foo/note-1".into(),
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
            });
            r.push(Record::Details {
                from: "foo/note-2".into(),
                to: "foo/plan".into(),
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
    /// JSONL holds the new state while the projection stays prior — and the
    /// next apply reproduces the committed state.
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
        });
        mutated.push(Record::Details {
            from: "foo/note-2".into(),
            to: "foo/plan".into(),
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

        // The failure was one-shot: the next apply reproduces the committed
        // state.
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
}
