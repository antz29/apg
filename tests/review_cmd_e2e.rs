mod common;

use apg::artifacts;
use apg::artifacts::parse_args;
use apg::graph::{Graph, Location, Node, NodeKind};
use apg::load;
use apg::review_cmd::*;
use apg::schema::Record;
use apg::specs;
use apg::testutil::{self, Repo, spawn_apg};
use common::with_cwd;
use lbug::{Connection, Database};
use std::path::{Path, PathBuf};

/// A temp repo with a real project context for `foo` (R4 — non-git
/// fixtures are gone): the worktree on branch `foo` carries a real DB
/// with a minimal code graph and a fresh scan_meta.
fn fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = Repo::new(&format!("review-{name}"));
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

/// Builds a real DB + load files under `dir/apg` (used by `fixture`).
fn db_at(dir: &Path) {
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

    // A durable layer node (SPEC §3.1): a requirement under
    // `apg/layers/requirements/requirement/`, FQN without a project
    // prefix. Durable-node reviews resolve against it in the graph.
    g.nodes.insert(
        "requirements.requirement.timer".to_string(),
        Node {
            kind: NodeKind::Requirement,
            ..Node::default()
        },
    );

    // One durable node per remaining file-backed tier (SPEC §3.1) — a
    // domain Entity, a solution System, a global Constraint — so reviews
    // of every tier route to their own `.trans` mirror.
    g.nodes.insert(
        "domain.entity.order".to_string(),
        Node {
            kind: NodeKind::Entity,
            ..Node::default()
        },
    );
    g.nodes.insert(
        "solution.system.checkout".to_string(),
        Node {
            kind: NodeKind::System,
            ..Node::default()
        },
    );
    g.nodes.insert(
        "global.constraint.law".to_string(),
        Node {
            kind: NodeKind::Constraint,
            ..Node::default()
        },
    );

    // A durable Note (SPEC §3.1) — reviewable, but deliberately absent from
    // the shared `Details` target list. A review on it exercises the
    // Reviews-only `(Feedback, Note)` pair.
    g.nodes.insert(
        "requirements.note.design".to_string(),
        Node {
            kind: NodeKind::Note,
            ..Node::default()
        },
    );

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

/// e2e tier -- real I/O: every test here writes/reads the transient tier
/// mirrors on disk and opens `db.lbug`. Each is `#[ignore]`d, so a plain
/// `cargo test` never runs one; the only entry point is the named guard
/// `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn wont_fix_is_not_terminal_until_reviewer_resolves() {
        let (apg_root, repo, _wt) = fixture("wont-fix");

        // A transient plan with a task, plus an open Feedback reviewing it.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        // Seeded through the funnel (auto-committed on the branch, DB fresh).
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();

        // Writer actions with --wont-fix: a *proposal*, not terminal.
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "actioned",
            Some("wont-fix".to_string()),
        )
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        let f = recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback {
                    fqn,
                    status,
                    disposition,
                    ..
                } if fqn == "foo/feedback-1" => Some((status.clone(), disposition.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, ("actioned".to_string(), "wont-fix".to_string()));

        // A wont-fix actioned item is NOT terminal: the reviewer must approve.
        // Reject reopens it (writer must rework).
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "open",
            Some("rejected".to_string()),
        )
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        let f = recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback {
                    fqn,
                    status,
                    disposition,
                    ..
                } if fqn == "foo/feedback-1" => Some((status.clone(), disposition.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, ("open".to_string(), "rejected".to_string()));

        // The reviewer may instead approve (resolve) — terminal.
        set_feedback_at(&apg_root, "foo/feedback-1", "foo", "resolved", None).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        let f = recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback { fqn, status, .. } if fqn == "foo/feedback-1" => {
                    Some(status.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(f, "resolved");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn first_review_without_transient_files_numbers_feedback_1() {
        // Numbering quirk fix (note-27): the shared `<project>/feedback-<n>`
        // namespace starts at 1 even when NO transient file exists yet — a
        // plan-less project reviewing a durable/code node. The first review
        // must land `foo/feedback-1` (never `feedback-0`); a second review
        // lands `foo/feedback-2` (the counter is shared across mirrors).
        let (apg_root, repo, _wt) = fixture("first-number");

        // The fixture starts with no plan store and no tier mirrors at all.
        for f in specs::project_transient_files(&apg_root, "foo") {
            assert!(
                !f.exists(),
                "fixture must start with no transient file: {}",
                f.display()
            );
        }

        // First review: a code node → the implementation tier mirror.
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // Second review: a durable node → the requirements tier mirror.
        let p = parse_args(&[
            "requirements.requirement.timer".to_string(),
            "--body".to_string(),
            "req review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // The mirrors carry feedback-1 then feedback-2 — no 0 slot.
        let impl_mirror = apg_root
            .join(specs::TRANS)
            .join("implementation")
            .join("foo.jsonl");
        let req_mirror = apg_root
            .join(specs::TRANS)
            .join("requirements")
            .join("foo.jsonl");
        let impl_recs = specs::read_jsonl(&impl_mirror).unwrap();
        let req_recs = specs::read_jsonl(&req_mirror).unwrap();
        assert!(
            impl_recs.iter().any(|r| matches!(
                r,
                Record::Feedback { fqn, .. } if fqn == "foo/feedback-1"
            )),
            "the first review of a plan-less project must number feedback-1"
        );
        assert!(
            req_recs.iter().any(|r| matches!(
                r,
                Record::Feedback { fqn, .. } if fqn == "foo/feedback-2"
            )),
            "the second review must number feedback-2"
        );

        // Both round-trip into the branch DB; the 0 slot never exists.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-1"), "feedback-1 node dropped");
        assert!(db.has_node("foo/feedback-2"), "feedback-2 node dropped");
        assert!(
            !db.has_node("foo/feedback-0"),
            "feedback-0 must never be created"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("github.com/x/y.Store"),
            "feedback-1 reviews edge dropped: {out}"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-2'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("requirements.requirement.timer"),
            "feedback-2 reviews edge dropped: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn review_add_roundtrips_reviews_edge_to_task_and_code() {
        // `apg review add <target>` attaches an open Feedback with a Reviews
        // edge; the edge survives the re-ingest for both a plan/task target
        // and a code target (which needs an explicit --project). Both halves
        // land in `.trans` — the plan store for the task, the implementation
        // tier mirror for the code node.
        let (apg_root, repo, _wt) = fixture("roundtrip");

        // A transient plan with one task.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();

        // Review the task (a plan target — routes to the transient plan
        // store, co-located with the plan records).
        let p = parse_args(&[
            "foo/plan.phase-01.task-1".to_string(),
            "--body".to_string(),
            "task review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // Review a code node (needs --project — routes to the implementation
        // tier mirror, NOT the plan file).
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // The Feedback nodes round-trip WITH their Reviews edges.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-1"), "feedback-1 node dropped");
        assert!(db.has_node("foo/feedback-2"), "feedback-2 node dropped");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan.phase-01.task-1"),
            "task reviews edge dropped: {out}"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-2'})-[:Reviews]->(n:Struct) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("github.com/x/y.Store"),
            "code reviews edge dropped: {out}"
        );
        drop(db);

        // Both halves live in `.trans`: the task review in the plan store,
        // the code review in the implementation tier mirror — and the legacy
        // durable stores never exist.
        let task_recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(
            task_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
            )),
            "task review must co-locate with the plan records"
        );
        let impl_mirror = apg_root
            .join(specs::TRANS)
            .join("implementation")
            .join("foo.jsonl");
        let code_recs = specs::read_jsonl(&impl_mirror).unwrap();
        assert!(
            code_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-2" && to == "github.com/x/y.Store"
            )),
            "code review must land in the implementation tier mirror"
        );
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );
        assert!(
            !apg_root.join("notes").exists(),
            "apg/notes must never be written"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn review_add_roundtrips_reviews_edge_to_note() {
        // A review of a durable `Note` target must land a
        // `Feedback-[:Reviews]->Note` edge in db.lbug. Before the Reviews-only
        // `(Feedback, Note)` pair existed, `rel_pair_allowed` refused the pair
        // and `merge_edge` silently dropped the edge — the Feedback survived
        // but `apg review list` could not show it.
        let (apg_root, repo, _wt) = fixture("note-roundtrip");

        let p = parse_args(&[
            "requirements.note.design".to_string(),
            "--body".to_string(),
            "note review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // A fresh DB read (equivalent to a new apg_query process) sees both the
        // Feedback node and the Reviews edge to the Note.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-1"), "feedback-1 node dropped");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n:Note) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("requirements.note.design"),
            "note reviews edge dropped: {out}"
        );
        drop(db);

        // Both halves live in the requirements tier mirror (the reviewed
        // node's tier), not the plan store.
        let mirror = apg_root
            .join(specs::TRANS)
            .join("requirements")
            .join("foo.jsonl");
        let recs = specs::read_jsonl(&mirror).unwrap();
        assert!(
            recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "requirements.note.design"
            )),
            "the note review must land in the requirements tier mirror"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn review_add_routes_to_transient_mirrors_and_never_commits() {
        // Transience enforcement (SPEC §5): a review of a durable layer node,
        // a code node, and a plan node each lands its Feedback + Reviews
        // halves under `apg/.trans/` — the tier mirror of the attached node
        // (requirements/implementation) or the plan store — never in
        // `apg/specs/`/`apg/notes/`, never polluting the committed node file,
        // and never committed (the branch HEAD does not move).
        let (apg_root, repo, _wt) = fixture("mirrors");

        // The durable side of the requirement review: the node file in the
        // layers store (committed identity — must stay untouched).
        let req_file = apg::layers::node_file_path(
            &apg_root,
            apg::layers::Layer::Requirements,
            "requirement",
            "timer",
        );
        std::fs::create_dir_all(req_file.parent().unwrap()).unwrap();
        let req_json = r#"{"name":"timer","type":"requirement","layer":"requirements","body":"x","properties":{},"out":[],"in":[]}"#;
        std::fs::write(&req_file, req_json).unwrap();
        // The untracked node file dirties the worktree tree: re-record the
        // scan_meta as dirty so the branch DB stays fresh under the mutations
        // (a recorded dirty matching a dirty tree is fresh — git.rs).
        testutil::write_scan_meta(
            &apg_root,
            Some(&repo.head_sha()),
            false,
            "2026-09-07T00:00:00Z",
        );

        // A transient plan with one task (plan-family target).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();
        let head_before = repo.head_sha();

        // 1. A durable requirement (--project required: layer FQNs carry no
        //    project prefix) → the requirements tier mirror.
        let p = parse_args(&[
            "requirements.requirement.timer".to_string(),
            "--body".to_string(),
            "req review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // 2. A code node → the implementation tier mirror.
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // 3. A plan task → the plan store (co-located with the plan records).
        let p = parse_args(&[
            "foo/plan.phase-01.task-1".to_string(),
            "--body".to_string(),
            "task review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // Both halves land in the tier dir of the attached node.
        let req_mirror = apg_root
            .join(specs::TRANS)
            .join("requirements")
            .join("foo.jsonl");
        let impl_mirror = apg_root
            .join(specs::TRANS)
            .join("implementation")
            .join("foo.jsonl");
        let req_recs = specs::read_jsonl(&req_mirror).unwrap();
        assert!(
            req_recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-1")),
            "requirement review Feedback must land in the requirements mirror"
        );
        assert!(
            req_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "requirements.requirement.timer"
            )),
            "requirement review Reviews edge must land beside its Feedback"
        );
        let impl_recs = specs::read_jsonl(&impl_mirror).unwrap();
        assert!(
            impl_recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-2")),
            "code review Feedback must land in the implementation mirror"
        );
        assert!(
            impl_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-2" && to == "github.com/x/y.Store"
            )),
            "code review Reviews edge must land beside its Feedback"
        );
        let plan_recs = specs::read_jsonl(&path).unwrap();
        assert!(
            plan_recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-3")),
            "task review Feedback must co-locate with the plan records"
        );
        assert!(
            plan_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-3" && to == "foo/plan.phase-01.task-1"
            )),
            "task review Reviews edge must land beside its Feedback"
        );

        // Never in the legacy durable stores; the committed node file is
        // byte-identical (transient references never pollute node files).
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );
        assert!(
            !apg_root.join("notes").exists(),
            "apg/notes must never be written"
        );
        assert_eq!(
            std::fs::read_to_string(&req_file).unwrap(),
            req_json,
            "the durable node file must never carry transient references"
        );

        // Review state dies with the branch: nothing was committed.
        assert_eq!(
            repo.head_sha(),
            head_before,
            "feedback writes are transient — the branch HEAD must not move"
        );

        // The write-throughs re-ingested every mirror: all three feedback
        // nodes + their edges are in the branch DB, and the requirement
        // review pairs against the durable node.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        for f in ["foo/feedback-1", "foo/feedback-2", "foo/feedback-3"] {
            assert!(db.has_node(f), "{f} must survive the re-ingest");
        }
        let out = db
            .conn()
            .unwrap()
            .query(
                "MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(:Requirement {fqn: 'requirements.requirement.timer'}) RETURN count(*)",
            )
            .unwrap()
            .to_string();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the requirement review must pair against the durable node: {out}"
        );
        drop(db);

        // Status updates also find feedback across the mirrors: action the
        // mirror-resident review (feedback-1, requirements mirror).
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "actioned",
            Some("fixed".to_string()),
        )
        .unwrap();
        let req_recs = specs::read_jsonl(&req_mirror).unwrap();
        let f = req_recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback {
                    fqn,
                    status,
                    disposition,
                    ..
                } if fqn == "foo/feedback-1" => Some((status.clone(), disposition.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, ("actioned".to_string(), "fixed".to_string()));
        assert_eq!(
            repo.head_sha(),
            head_before,
            "feedback status writes are transient too"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn review_all_tier_mirrors_and_status_verbs_never_commit() {
        // Transience surface completion (SPEC §5): the task-1 test covers the
        // requirements/implementation tier mirrors, the plan store, and one
        // action; this closes the rest — a review of a domain, a solution,
        // and a global target each land their Feedback + Reviews halves in
        // that tier's `.trans` mirror, and the action/resolve/reject status
        // verbs write through the same transient files. Review state dies
        // with the branch: the project branch HEAD (where an auto-commit
        // would land), the main HEAD, and the tree all stay untouched.
        let (apg_root, repo, wt) = fixture("all-tiers");
        let head_before = repo.head_sha();
        let branch_head_before = wt_head(&wt);

        // Seed the plan store (the realistic project shape — a plan exists
        // before any review) so the shared feedback namespace starts at
        // `feedback-1` (the task-1 fixture does the same).
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        artifacts::write_jsonl_and_reingest(
            &apg_root,
            &plan_path,
            "foo",
            &[Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
                strategy: String::new(),
            }],
        )
        .unwrap();

        // One review per untested tier mirror (the fixture DB carries one
        // durable node per file-backed tier).
        for (target, tier, n) in [
            ("domain.entity.order", "domain", 1u64),
            ("solution.system.checkout", "solution", 2),
            ("global.constraint.law", "global", 3),
        ] {
            let p = parse_args(&[
                target.to_string(),
                "--body".to_string(),
                format!("{tier} review"),
                "--project".to_string(),
                "foo".to_string(),
            ]);
            apply_review_add(&apg_root, &p).unwrap();

            // Both halves land in the tier dir of the attached node.
            let mirror = apg_root.join(specs::TRANS).join(tier).join("foo.jsonl");
            let recs = specs::read_jsonl(&mirror).unwrap();
            assert!(
                recs.iter().any(|r| matches!(
                    r,
                    Record::Feedback { fqn, .. } if fqn == &format!("foo/feedback-{n}")
                )),
                "{tier} mirror must carry feedback-{n}"
            );
            assert!(
                recs.iter().any(|r| matches!(
                    r,
                    Record::Reviews { from, to }
                        if from == &format!("foo/feedback-{n}") && to == target
                )),
                "{tier} mirror must carry the Reviews edge beside its Feedback"
            );
        }

        // The status verbs find feedback across the mirrors and write through
        // the same transient files: action, resolve (terminal), reject.
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "actioned",
            Some("fixed".to_string()),
        )
        .unwrap();
        set_feedback_at(&apg_root, "foo/feedback-2", "foo", "resolved", None).unwrap();
        set_feedback_at(
            &apg_root,
            "foo/feedback-3",
            "foo",
            "open",
            Some("rejected".to_string()),
        )
        .unwrap();
        let status_of = |recs: &[Record], fqn: &str| -> (String, String) {
            recs.iter()
                .find_map(|r| match r {
                    Record::Feedback {
                        fqn: f,
                        status,
                        disposition,
                        ..
                    } if f == fqn => Some((status.clone(), disposition.clone())),
                    _ => None,
                })
                .unwrap()
        };
        let dom = specs::read_jsonl(&apg_root.join(specs::TRANS).join("domain").join("foo.jsonl"))
            .unwrap();
        let sol = specs::read_jsonl(
            &apg_root
                .join(specs::TRANS)
                .join("solution")
                .join("foo.jsonl"),
        )
        .unwrap();
        let glo = specs::read_jsonl(&apg_root.join(specs::TRANS).join("global").join("foo.jsonl"))
            .unwrap();
        assert_eq!(
            status_of(&dom, "foo/feedback-1"),
            ("actioned".to_string(), "fixed".to_string())
        );
        assert_eq!(
            status_of(&sol, "foo/feedback-2"),
            ("resolved".to_string(), String::new())
        );
        assert_eq!(
            status_of(&glo, "foo/feedback-3"),
            ("open".to_string(), "rejected".to_string())
        );

        // Never in the legacy durable stores.
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );
        assert!(
            !apg_root.join("notes").exists(),
            "apg/notes must never be written"
        );

        // Review state dies with the branch: nothing was committed.
        assert_eq!(
            wt_head(&wt),
            branch_head_before,
            "feedback writes are transient — the project branch HEAD must not move"
        );
        assert_eq!(
            repo.head_sha(),
            head_before,
            "feedback writes must never move the main HEAD either"
        );
        assert!(
            repo.is_clean(),
            "the tree must stay clean — the mirrors are gitignored"
        );

        testutil::remove(&repo);
    }

    /// Phase-7 task-5 (E2E, top-level dispatch): the `apg review` surface is
    /// unchanged — `cmd_review` dispatches exactly
    /// `add`/`action`/`resolve`/`reject`/`list`, rejects any other subcommand,
    /// and the feedback body is immutable across the status verbs (only
    /// status/disposition change). Review writes stay transient.
    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn review_surface_dispatch_unchanged() {
        let (apg_root, repo, wt) = fixture("surface");

        // No subcommand: usage names exactly the five verbs.
        let err = cmd_review(&[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("add|action|resolve|reject|list"), "{msg}");

        // Any other subcommand is rejected (no create/update/rm vocabulary).
        for bogus in ["update", "rm", "create", "attach"] {
            let err = cmd_review(&av(&[bogus])).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("unknown apg review subcommand: {bogus}")),
                "{err}"
            );
        }

        let head_before = repo.head_sha();
        let branch_head_before = wt_head(&wt);

        // One cwd hold for the whole surface: `cmd_review` resolves `apg/`
        // by walking up from cwd, so every command fits under a single
        // `CWD_LOCK` acquisition instead of one per call. The test's own
        // work is sub-second; it was the five separate re-queues on the
        // process-wide lock that pushed it past libtest's 60s warning.
        with_cwd(&wt, || {
            // `add` dispatches and routes the Feedback + Reviews halves to
            // the attached node's tier mirror.
            cmd_review(&av(&[
                "add",
                "domain.entity.order",
                "--body",
                "surface body",
                "--project",
                "foo",
            ]))
            .unwrap();
            let mirror = apg_root.join(specs::TRANS).join("domain").join("foo.jsonl");
            let (feedback_fqn, body) = {
                let recs = specs::read_jsonl(&mirror).unwrap();
                let feedback = recs
                    .iter()
                    .find_map(|r| match r {
                        Record::Feedback {
                            fqn, body, status, ..
                        } => Some((fqn.clone(), body.clone(), status.clone())),
                        _ => None,
                    })
                    .expect("add lands a Feedback record");
                assert_eq!(feedback.2, "open", "a new Feedback starts open");
                assert!(
                    recs.iter().any(|r| matches!(
                        r,
                        Record::Reviews { from, to }
                            if from == &feedback.0 && to == "domain.entity.order"
                    )),
                    "the Reviews edge lands beside its Feedback"
                );
                (feedback.0, feedback.1)
            };
            assert_eq!(body, "surface body");

            // `list` dispatches (read-only) — no store change.
            let mirror_before = std::fs::read_to_string(&mirror).unwrap();
            cmd_review(&av(&["list", "domain.entity.order"])).unwrap();
            assert_eq!(std::fs::read_to_string(&mirror).unwrap(), mirror_before);

            // `action` → `actioned`/`fixed`; the body never changes.
            cmd_review(&av(&["action", &feedback_fqn, "--fix"])).unwrap();
            assert_eq!(
                feedback_status(&mirror, &feedback_fqn),
                ("actioned".to_string(), "fixed".to_string())
            );
            assert_eq!(
                feedback_body(&mirror, &feedback_fqn),
                "surface body",
                "the feedback body is immutable across action"
            );

            // `reject` → back to `open`/`rejected`; body still immutable.
            cmd_review(&av(&["reject", &feedback_fqn])).unwrap();
            assert_eq!(
                feedback_status(&mirror, &feedback_fqn),
                ("open".to_string(), "rejected".to_string())
            );
            assert_eq!(feedback_body(&mirror, &feedback_fqn), "surface body");

            // `resolve` → terminal `resolved`; the prior disposition is preserved
            // (resolve passes no disposition — unchanged semantics); body immutable.
            cmd_review(&av(&["resolve", &feedback_fqn])).unwrap();
            assert_eq!(
                feedback_status(&mirror, &feedback_fqn),
                ("resolved".to_string(), "rejected".to_string())
            );
            assert_eq!(feedback_body(&mirror, &feedback_fqn), "surface body");
        });

        // Review writes stay transient: neither the main nor the project branch
        // HEAD moves and the tree stays clean.
        assert_eq!(repo.head_sha(), head_before, "the main HEAD must not move");
        assert_eq!(
            wt_head(&wt),
            branch_head_before,
            "the project branch HEAD must not move"
        );
        assert!(
            repo.is_clean(),
            "the mirror is gitignored — tree stays clean"
        );

        testutil::remove(&repo);
    }

    /// Phase-02 task-6 (E2E): after a durable node reviewed by an item is
    /// removed, the item stays visible to `apg review list` — the transient
    /// record set is authoritative, so the Feedback and its reviewed-target
    /// reference survive in the requirements mirror even though the DB
    /// projection drops the node and its `Reviews` edge. The list marks the
    /// absent target with the exact `<target> (removed target)` cell, and the
    /// item still closes through the same `apg review resolve <fqn>` surface.
    #[test]
    #[ignore = "e2e tier: real I/O (transient mirrors/db.lbug/git); run via cargo test-e2e"]
    fn review_list_marks_orphaned_target_and_closes_by_fqn() {
        let (_apg_root, repo, wt) = fixture("orphan");

        // Add the durable node through the real CLI (auto-committed on the
        // branch, projected into the branch DB).
        let added = spawn_apg(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "gone",
                "--body",
                "x",
            ],
            &wt,
        );
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );

        // Review it while it exists: both halves land in the requirements
        // mirror and the Reviews edge resolves in the DB.
        let reviewed = spawn_apg(
            &[
                "review",
                "add",
                "requirements.requirement.gone",
                "--body",
                "orphan me",
                "--project",
                "foo",
            ],
            &wt,
        );
        assert!(
            reviewed.status.success(),
            "{}",
            String::from_utf8_lossy(&reviewed.stderr)
        );

        // Remove the reviewed node: the record set survives in `.trans`, but
        // the DB projection drops the node — and the dangling Reviews edge
        // with it.
        let removed = spawn_apg(&["node", "rm", "requirements", "requirement", "gone"], &wt);
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );

        // The record set is authoritative: the orphan is still listed, with the
        // exact marker cell for its now-absent target.
        let listed = spawn_apg(&["review", "list"], &wt);
        assert!(
            listed.status.success(),
            "{}",
            String::from_utf8_lossy(&listed.stderr)
        );
        let out = String::from_utf8(listed.stdout).unwrap();
        assert!(
            out.lines().any(
                |l| l == "foo/feedback-1,open,,requirements.requirement.gone (removed target)"
            ),
            "the orphaned item must be listed with its removed-target marker: {out}"
        );

        // It still closes through the same resolve-by-FQN surface, and the
        // marker persists after the item turns terminal.
        let resolved = spawn_apg(&["review", "resolve", "foo/feedback-1"], &wt);
        assert!(
            resolved.status.success(),
            "{}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let listed = spawn_apg(&["review", "list"], &wt);
        assert!(
            listed.status.success(),
            "{}",
            String::from_utf8_lossy(&listed.stderr)
        );
        let out = String::from_utf8(listed.stdout).unwrap();
        assert!(
            out.lines()
                .any(|l| l
                    == "foo/feedback-1,resolved,,requirements.requirement.gone (removed target)"),
            "resolving by FQN must close the orphaned item: {out}"
        );

        testutil::remove(&repo);
    }
}

/// The worktree branch's HEAD sha — the branch an auto-commit would land
/// on. `repo.head_sha()` reads the main checkout's HEAD, which a worktree
/// commit could never move, so transience assertions must check this one.
fn wt_head(wt: &Path) -> String {
    git2::Repository::open(wt)
        .unwrap()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string()
}

/// The `Vec<String>` argv shape `cmd_review` takes (the slice the top-level
/// dispatch hands it).
fn av(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// The `(status, disposition)` of `fqn` in a transient mirror.
fn feedback_status(mirror: &Path, fqn: &str) -> (String, String) {
    specs::read_jsonl(mirror)
        .unwrap()
        .iter()
        .find_map(|r| match r {
            Record::Feedback {
                fqn: f,
                status,
                disposition,
                ..
            } if f == fqn => Some((status.clone(), disposition.clone())),
            _ => None,
        })
        .expect("feedback record present")
}

/// The body of `fqn` in a transient mirror.
fn feedback_body(mirror: &Path, fqn: &str) -> String {
    specs::read_jsonl(mirror)
        .unwrap()
        .iter()
        .find_map(|r| match r {
            Record::Feedback { fqn: f, body, .. } if f == fqn => Some(body.clone()),
            _ => None,
        })
        .expect("feedback record present")
}
