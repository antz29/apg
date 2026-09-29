//! Relocated e2e tests for `apg plan` (from `src/plan_cmd.rs`'s inline
//! `#[cfg(test)] mod tests`). Real I/O only; every test is `#[ignore]`d and
//! runs through `cargo test-e2e`.

mod common;

use apg::artifacts;
use apg::graph::{Graph, Location, Node, NodeKind};
use apg::load;
use apg::plan_cmd::*;
use apg::schema::Record;
use apg::specs;
use apg::testutil::{self, Repo, av, nf, task_rec, with_cwd, wt_commit_paths};
use lbug::{Connection, Database};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A temp repo with a real project context for `foo` (R4 — non-git
/// fixtures are gone): the worktree on branch `foo` carries a real DB
/// (Module/File/Struct + an UnresolvedTarget) and a fresh scan_meta.
/// Returns `(wt_apg_root, repo, wt_root)`. Tags are module-prefixed so
/// parallel tests in other modules never collide on a temp dir.
fn fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = Repo::new(&format!("plan-{name}"));
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

/// A project context for plan tests that never open the branch DB: a real
/// git repo whose worktree `foo` on branch `foo` hosts the transient
/// `.trans/plans` store, but with NO code DB — so every plan write takes
/// the JSONL-only path (`write_jsonl_and_reingest` skips both the
/// projection and the stale gate when `db.lbug` is absent). Returns
/// `(wt_apg_root, repo, wt_root)`.
fn plan_store_fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = Repo::new(&format!("plan-{name}"));
    let wt = repo.start_project("foo");
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

    // An unresolved reference — real-code checks must NOT treat it as
    // implementation code (the apply gate's realization check, REVIEW).
    g.nodes.insert(
        "github.com/x/y.Missing".to_string(),
        Node {
            kind: NodeKind::UnresolvedTarget,
            category: Some("external".to_string()),
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

/// Writes a plan JSONL for `foo` with one phase containing one task.
fn write_plan(apg_root: &Path) -> PathBuf {
    let path = specs::plan_jsonl_path(apg_root, "foo");
    let records = vec![
        Record::Plan {
            fqn: "foo/plan".to_string(),
            title: "P".to_string(),
            strategy: String::new(),
        },
        Record::PlanPhase {
            fqn: "foo/plan.phase-01".to_string(),
            number: 1,
            title: "P1".to_string(),
            deliverable: "D".to_string(),
            status: "pending".to_string(),
        },
        Record::Contains {
            from: "foo/plan".to_string(),
            to: "foo/plan.phase-01".to_string(),
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
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        },
    ];
    specs::write_jsonl(&path, &records).unwrap();
    path
}

/// Writes one requirement node file (`requirements.requirement.<name>`)
/// under the layers store so `--satisfies <name>` resolves in the update
/// phase unit tests.
fn write_requirement(apg_root: &Path, name: &str) {
    let path = apg::layers::node_file_path(
        apg_root,
        apg::layers::Layer::Requirements,
        "requirement",
        name,
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = format!(
        r#"{{"name":"{name}","type":"requirement","layer":"requirements","body":"x","properties":{{}},"out":[],"in":[]}}"#
    );
    std::fs::write(&path, body).unwrap();
}

/// Writes one solution-layer node file under `apg/layers/solution/` with
/// the given `implemented-by` code-FQN targets (raw file write — the
/// fixture commits it; the production node-file path is exercised by
/// layers' own tests).
fn write_solution_node(apg_root: &Path, node_type: &str, name: &str, refs: &[&str]) {
    let nf = apg::layers::NodeFile {
        layer: "solution".to_string(),
        node_type: node_type.to_string(),
        name: name.to_string(),
        body: String::new(),
        properties: std::collections::BTreeMap::new(),
        out: refs
            .iter()
            .map(|t| apg::layers::OutEdge {
                kind: "implemented-by".to_string(),
                target: t.to_string(),
                properties: std::collections::BTreeMap::new(),
            })
            .collect(),
        in_edges: Vec::new(),
    };
    let path = apg::layers::node_file_path(apg_root, apg::layers::Layer::Solution, node_type, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&nf).unwrap()).unwrap();
}

/// Writes an arbitrary layers node file with the given `(kind, target)`
/// out-edges (raw file write; the fixture commits it).
fn write_node_file(
    apg_root: &Path,
    layer: &str,
    node_type: &str,
    name: &str,
    edges: &[(&str, &str)],
) {
    let l = match layer {
        "requirements" => apg::layers::Layer::Requirements,
        "domain" => apg::layers::Layer::Domain,
        "solution" => apg::layers::Layer::Solution,
        "implementation" => apg::layers::Layer::Implementation,
        "global" => apg::layers::Layer::Global,
        other => panic!("bad layer `{other}`"),
    };
    let node = nf(layer, node_type, name, edges);
    let path = apg::layers::node_file_path(apg_root, l, node_type, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&node).unwrap()).unwrap();
}

/// A minimal green plan: Plan + phase-01 (no tasks, no planned nodes, no
/// feedback) — the gates other than coverage are vacuous.
fn bare_plan() -> Vec<Record> {
    vec![
        Record::Plan {
            fqn: "foo/plan".to_string(),
            title: "P".to_string(),
            strategy: String::new(),
        },
        Record::PlanPhase {
            fqn: "foo/plan.phase-01".to_string(),
            number: 1,
            title: "P1".to_string(),
            deliverable: "D".to_string(),
            status: "pending".to_string(),
        },
        Record::Contains {
            from: "foo/plan".to_string(),
            to: "foo/plan.phase-01".to_string(),
        },
    ]
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

/// The `foo` plan record's `(title, strategy)` pair, read back from the
/// transient store.
fn plan_fields(apg_root: &Path) -> (String, String) {
    specs::read_jsonl(&specs::plan_jsonl_path(apg_root, "foo"))
        .unwrap()
        .iter()
        .find_map(|r| match r {
            Record::Plan {
                fqn,
                title,
                strategy,
            } if fqn == "foo/plan" => Some((title.clone(), strategy.clone())),
            _ => None,
        })
        .unwrap()
}

/// No `Feedback`/`Note` record is orphaned (each still has its
/// `Reviews`/`Details` edge) and no plan-family edge points at a record
/// that no longer exists — except a retained `Feedback`'s `Reviews` target,
/// which `cascade_remove` keeps by design even when the reviewed plan-family
/// record is removed (a deliberately reviewed-target reference).
fn assert_no_orphans(records: &[Record]) {
    let node_fqns: BTreeSet<&str> = records.iter().filter_map(artifacts::node_fqn).collect();
    for r in records {
        if let Record::Feedback { fqn, .. } = r {
            assert!(
                records
                    .iter()
                    .any(|e| matches!(e, Record::Reviews { from, .. } if from == fqn)),
                "orphan Feedback `{fqn}` (no Reviews edge)"
            );
        }
        if let Record::Note { fqn, .. } = r {
            assert!(
                records
                    .iter()
                    .any(|e| matches!(e, Record::Details { from, .. } if from == fqn)),
                "orphan Note `{fqn}` (no Details edge)"
            );
        }
        if let Some((from, to)) = artifacts::edge_endpoints(r) {
            // A retained Feedback keeps its Reviews edge, so a Reviews target
            // may name a removed plan-family record; every other endpoint
            // must still resolve.
            let reviewed_target_may_dangle = matches!(r, Record::Reviews { .. });
            for (endpoint, may_dangle) in [(from, false), (to, reviewed_target_may_dangle)] {
                if may_dangle {
                    continue;
                }
                // Durable requirement FQNs are not in the transient plan
                // store; every plan-family endpoint must resolve.
                if endpoint.starts_with("foo/plan") || endpoint.starts_with("foo/feedback-") {
                    assert!(
                        node_fqns.contains(endpoint),
                        "edge endpoint `{endpoint}` points at a removed record"
                    );
                }
            }
        }
    }
}

/// e2e tier -- real I/O: every test here writes the transient plan store
/// and node files on disk, opens `db.lbug`, runs git, or spawns the `apg`
/// binary. Each is `#[ignore]`d, so a plain `cargo test` never runs one;
/// the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_done_is_assertion_only_and_undone_reverses() {
        let (apg_root, repo, _wt) = fixture("assertion-done");
        let _path = write_plan(&apg_root);

        // Mark the task done.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == "foo/plan.phase-01.task-1" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "done");

        // Assertion-only: no promotion side effects — the plan file still
        // carries only the task's status flip.

        // undone reverses.
        plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == "foo/plan.phase-01.task-1" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "pending");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_complete_is_milestone_only_with_gate_and_no_retirement() {
        let (apg_root, repo, _wt) = fixture("milestone-complete");

        // Pending task → complete is rejected by the gate.
        let _path = write_plan(&apg_root);
        assert!(plan_complete_at(&apg_root, "foo", 1).is_err());

        // Mark the task done, add resolved feedback to prove the gate accepts.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();

        // Milestone-only: the plan file survives (no retirement) — the phase's
        // durable `status` flipped to done, nothing materialized elsewhere.
        assert!(specs::plan_jsonl_path(&apg_root, "foo").exists());
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(r, Record::PlanPhase { .. })));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_note_roundtrip_into_plan_jsonl() {
        let (apg_root, repo, _wt) = fixture("task-note");
        let _path = write_plan(&apg_root);

        plan_note_at(
            &apg_root,
            "foo",
            "foo/plan.phase-01.task-1",
            "deviation: the Store uses a byte slice, not a file handle",
            "note",
        )
        .unwrap();

        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Note { fqn, body, .. }
                if fqn == "foo/plan.note-1"
                    && body.contains("deviation")
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Details { from, to }
                if from == "foo/plan.note-1" && to == "foo/plan.phase-01.task-1"
        )));

        // The note + Details edge land in the live DB.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'foo/plan.note-1'})-[:Details]->(:Task) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "note→task Details edge: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn apply_gate_rejects_unrealized_planned_node() {
        let (apg_root, repo, _wt) = fixture("apply-gate");

        // A plan whose task Builds a planned node whose FQN has NO real code
        // in the graph (the code was claimed-done but is missing).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
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
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // The gate rejects: the planned node does not resolve to real code.
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("coherence gate blocked"), "{err}");
        assert!(err.to_string().contains("github.com/x/y.Gateway"), "{err}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn apply_gate_passes_when_planned_node_realized_and_feedback_resolved() {
        let (apg_root, repo, _wt) = fixture("apply-gate-ok");

        // The planned node's FQN resolves to the fixture's real Struct.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
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
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "resolved".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // Green: every planned node is realized, all feedback resolved.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn apply_gate_checks_every_planned_node_not_just_builds_targets() {
        let (apg_root, repo, _wt) = fixture("apply-gate-all-planned");

        // Two planned nodes, only one built: the gate must block on the
        // unrealized one even though every Builds edge's target resolves (the
        // per-plan realization check, not a Builds-target check).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
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
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("github.com/x/y.Gateway"), "{err}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn apply_gate_rejects_unresolved_feedback() {
        let (apg_root, repo, _wt) = fixture("apply-feedback");

        // Planned node realizes, but an open Feedback reviews the phase.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
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
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(
            err.to_string().contains("unresolved review feedback"),
            "{err}"
        );

        testutil::remove(&repo);
    }

    /// Unit: `plan_update_phase_at` updates a phase in place — its task
    /// Contains edges survive — while `--satisfies`/`--prereq` fold `link`'s
    /// set-semantics (a passed set replaces the phase's own outgoing bridge
    /// edges, an omitted dimension is preserved). A bogus `--satisfies` is
    /// refused before any mutation, and an absent target is refused for all
    /// three update cores with the record set left byte-identical.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_update_phase_at_preserves_tasks_validates_satisfies_and_sets_bridge_edges() {
        let (apg_root, repo, _wt) = fixture("update-phase");
        write_requirement(&apg_root, "R1");
        write_requirement(&apg_root, "R2");
        write_requirement(&apg_root, "R3");

        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D1".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D2".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "requirements.requirement.R1".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
            // An unrelated phase's own Satisfies must never be touched.
            Record::Satisfies {
                from: "foo/plan.phase-02".into(),
                to: "requirements.requirement.R3".into(),
            },
        ];
        let snapshot = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };

        // Title/deliverable only: the task Contains edge and both bridge edges
        // survive; phase-02's Satisfies is untouched.
        plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            Some("P1b"),
            Some("D1b"),
            None,
            None,
        )
        .unwrap();
        let phase = records
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase {
                    fqn,
                    title,
                    deliverable,
                    ..
                } if fqn == "foo/plan.phase-01" => Some((title.as_str(), deliverable.as_str())),
                _ => None,
            })
            .unwrap();
        assert_eq!(phase, ("P1b", "D1b"));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-01" && to == "requirements.requirement.R1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-02"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-02" && to == "requirements.requirement.R3"
        )));

        // A passed set replaces only the phase's own outgoing bridge edges;
        // the task Contains edge and the unrelated phase's edge survive.
        let satisfies = vec!["R2".to_string()];
        let prereqs = vec!["02".to_string()];
        plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            None,
            None,
            Some(&prereqs),
            Some(&satisfies),
        )
        .unwrap();
        let s: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(s, vec!["requirements.requirement.R2"]);
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-02" && to == "requirements.requirement.R3"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));

        // A bogus --satisfies is refused before any mutation (the passed title
        // must not land).
        let before = snapshot(&records);
        let err = plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            Some("NOPE"),
            None,
            None,
            Some(&["ghost".to_string()]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a requirement"), "{err}");
        assert_eq!(
            snapshot(&records),
            before,
            "a refused satisfies must not mutate the records"
        );

        // Refuse-absent for all three update cores: no bytes change.
        let before = snapshot(&records);
        assert!(
            plan_update_phase_at(
                &apg_root,
                "foo",
                &mut records,
                9,
                Some("X"),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            plan_update_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                9,
                Some("X"),
                None,
                None,
                None,
                None,
                None,
            )
            .is_err()
        );
        assert!(
            plan_update_planned_at(&apg_root, &mut records, "/nope.ts", None, Some("X"), None)
                .is_err()
        );
        assert_eq!(
            snapshot(&records),
            before,
            "every absent-target refusal must leave the store byte-identical"
        );

        testutil::remove(&repo);
    }

    /// Unit: `plan_update_task_at` preserves `status` + incident `Reviews` and
    /// re-validates kind/tier (`validate_task_kind_tier`) and verb/target
    /// (`validate_task_verb`). Every invalid classification, verb, or
    /// creates-over-real-code is refused before any write.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_update_task_at_preserves_status_reviews_and_revalidates() {
        let (apg_root, repo, _wt) = fixture("update-task");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
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
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
                verb: "modifies".into(),
                target: "github.com/x/y.Store".into(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "b".into(),
                status: "open".into(),
                disposition: String::new(),
            },
        ];
        let snapshot = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };

        // A title-only update preserves `status` + the incident Reviews edge.
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            Some("T2"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let task = records
            .iter()
            .find_map(|r| match r {
                Record::Task {
                    fqn, title, status, ..
                } if fqn == "foo/plan.phase-01.task-1" => Some((title.as_str(), status.as_str())),
                _ => None,
            })
            .unwrap();
        assert_eq!(task, ("T2", "done"));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Reviews { from, to }
                if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
        )));

        // A re-validated classification change (source -> test/unit) lands.
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            None,
            Some("test"),
            Some("unit"),
            None,
            None,
            None,
        )
        .unwrap();

        // Negatives — each refused before any write.
        let before = snapshot(&records);
        let mut try_case = |label: &str,
                            kind: Option<&str>,
                            tier: Option<&str>,
                            verb: Option<&str>,
                            target: Option<&str>| {
            let err = plan_update_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                None,
                kind,
                tier,
                verb,
                target,
                None,
            )
            .unwrap_err();
            assert!(!err.to_string().is_empty(), "{label}");
            assert_eq!(snapshot(&records), before, "{label}: no partial write");
        };
        try_case("invalid kind", Some("qa"), None, None, None);
        try_case("test needs a tier", Some("test"), Some(""), None, None);
        try_case(
            "tier only for test",
            Some("source"),
            Some("unit"),
            None,
            None,
        );
        try_case("invalid verb", None, None, Some("explodes"), None);
        try_case(
            "creates over real code",
            None,
            None,
            Some("creates"),
            Some("github.com/x/y.Store"),
        );

        // The valid classification landed and status/Reviews still survive.
        let status = records
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == "foo/plan.phase-01.task-1" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "done");

        testutil::remove(&repo);
    }

    /// Unit: an empty target is the core mechanism the CLI `--no-fqn` sentinel
    /// drives. An empty target CLEARS a task's target to the target-less
    /// `creates` form (preserving `status`), an omitted target leaves it
    /// untouched (omission ≠ clear), and a target-less task is legal ONLY for
    /// `creates`: an empty target with a non-`creates` effective verb is
    /// refused by `validate_task_verb` with no partial write. The `--no-fqn`
    /// mapping itself lives in `plan_update` (`Some(String::new())`), so this
    /// covers the semantics that flag feeds into `plan_update_task_at`.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_update_task_clears_target_to_targetless() {
        let (apg_root, repo, _wt) = fixture("clear-target");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
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
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
                verb: "modifies".into(),
                target: "github.com/x/y.Store".into(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
        ];
        let snapshot = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };
        let task_state = |recs: &[Record]| -> (String, String, String) {
            recs.iter()
                .find_map(|r| match r {
                    Record::Task {
                        fqn,
                        verb,
                        target,
                        status,
                        ..
                    } if fqn == "foo/plan.phase-01.task-1" => {
                        Some((verb.clone(), target.clone(), status.clone()))
                    }
                    _ => None,
                })
                .unwrap()
        };

        // 1. Omission ≠ clear: an update that passes neither `--verb` nor
        //    `--fqn` leaves the existing real target exactly as it was.
        let real = (
            "modifies".to_string(),
            "github.com/x/y.Store".to_string(),
            "done".to_string(),
        );
        assert_eq!(task_state(&records), real);
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            task_state(&records),
            real,
            "omitting --verb/--fqn must leave the target unchanged, not clear it"
        );

        // 2. The `--no-fqn` sentinel (an empty target) CLEARS to the
        //    target-less `creates` form; `status` survives the clear.
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            None,
            None,
            None,
            Some("creates"),
            Some(""),
            None,
        )
        .unwrap();
        assert_eq!(
            task_state(&records),
            ("creates".to_string(), String::new(), "done".to_string()),
            "an empty target clears the task to the target-less creates form"
        );

        // 3. Target-less is creates-only. Restore a real target, then empty it
        //    with a non-creates effective verb — both explicitly and by leaving
        //    the stored `modifies` in place — and confirm the refusal plus a
        //    byte-identical record set (no partial write).
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            None,
            None,
            None,
            Some("modifies"),
            Some("github.com/x/y.Store"),
            None,
        )
        .unwrap();
        let before = snapshot(&records);
        for (label, verb) in [
            ("explicit modifies", Some("modifies")),
            ("stored modifies", None),
        ] {
            let err = plan_update_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                None,
                None,
                None,
                verb,
                Some(""),
                None,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("requires --fqn"),
                "{label}: expected the validate_task_verb target-less refusal, got `{err}`"
            );
            assert_eq!(
                snapshot(&records),
                before,
                "{label}: a refused clear must leave the record set byte-identical"
            );
        }

        testutil::remove(&repo);
    }

    /// Unit: `plan_update_planned_at` repoints the parent `Contains` edge while
    /// preserving every other incident edge.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_update_planned_at_repoints_parent_and_preserves_other_edges() {
        let (apg_root, repo, _wt) = fixture("update-planned");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
            },
            Record::PlannedNode {
                fqn: "/todo/app.ts".into(),
                kind: "file".into(),
                name: "app.ts".into(),
                parent: "github.com/x/y.Missing".into(),
            },
            Record::Contains {
                from: "github.com/x/y.Missing".into(),
                to: "/todo/app.ts".into(),
            },
            // A non-Contains incident edge must survive the repoint.
            Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "/todo/app.ts".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
        ];

        plan_update_planned_at(
            &apg_root,
            &mut records,
            "/todo/app.ts",
            Some("function"),
            Some("app2.ts"),
            Some("github.com/x/y"),
        )
        .unwrap();

        let planned = records
            .iter()
            .find_map(|r| match r {
                Record::PlannedNode {
                    fqn,
                    kind,
                    name,
                    parent,
                } if fqn == "/todo/app.ts" => Some((kind.as_str(), name.as_str(), parent.as_str())),
                _ => None,
            })
            .unwrap();
        assert_eq!(planned, ("function", "app2.ts", "github.com/x/y"));
        let contains: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Contains { from, to } if to == "/todo/app.ts" => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            contains,
            vec!["github.com/x/y"],
            "exactly the repointed parent Contains edge"
        );
        assert!(
            records.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "/todo/app.ts"
            )),
            "the non-Contains incident edge survives"
        );
        assert!(
            records.iter().any(|r| matches!(
                r,
                Record::Gates { from, to }
                    if from == "foo/plan.phase-01" && to == "foo/plan.phase-02"
            )),
            "unrelated edges are untouched"
        );

        // An absent planned node is refused (no record count change).
        let before = records.len();
        assert!(
            plan_update_planned_at(&apg_root, &mut records, "/nope.ts", None, Some("x"), None)
                .is_err()
        );
        assert_eq!(records.len(), before);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn task_verb_creates_accepts_absent_and_planned_targets() {
        let (apg_root, repo, _wt) = fixture("verb-creates");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];

        // A creates against an FQN absent from the scanned graph — and not yet
        // declared anywhere — is accepted (the plan may declare the planned
        // node later; the verb's rule is only "not existing real code").
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap();
        match records
            .iter()
            .find(|r| matches!(r, Record::Task { fqn, .. } if fqn == "foo/plan.phase-01.task-1"))
        {
            Some(Record::Task {
                verb,
                target,
                new_fqn,
                ..
            }) => {
                assert_eq!(verb, "creates");
                assert_eq!(target, "github.com/x/y.Gateway");
                assert!(new_fqn.is_empty());
            }
            other => panic!("expected the creates task, got {other:?}"),
        }

        // A creates against a DB `status: planned` node is accepted: the
        // planned-node universe is the DB's planned nodes UNION the
        // `Record::PlannedNode` records — here the planned FQN was written
        // through (re-ingested `status: planned`) while the records handed to
        // the add carry no PlannedNode record, so the DB half must count.
        let mut with_planned = records.clone();
        with_planned.push(Record::PlannedNode {
            fqn: "github.com/x/y.Gateway".to_string(),
            kind: "struct".to_string(),
            name: "Gateway".to_string(),
            parent: String::new(),
        });
        write_through(&apg_root, "foo", &with_planned).unwrap();
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            2,
            "T2",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-2"
                    && verb == "creates"
                    && target == "github.com/x/y.Gateway"
        )));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn task_verb_creates_refuses_real_scanned_code() {
        let (apg_root, repo, _wt) = fixture("verb-creates-real");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];

        // `github.com/x/y.Store` is a real Struct in the fixture DB — a
        // creates against it is refused before any Task record lands.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Store",
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("already resolves to scanned code"),
            "{err}"
        );
        assert!(
            records.iter().all(
                |r| !matches!(r, Record::Task { fqn, .. } if fqn == "foo/plan.phase-01.task-1")
            ),
            "a refused creates must not leave a partial Task record"
        );

        testutil::remove(&repo);
    }

    /// Strict-surface repro: the plan's own `status: planned` placeholder must
    /// NOT be silently re-declared. Re-declaring the SAME FQN is refused
    /// (naming `apg plan update`/`apg plan rm`); the parent correction the old
    /// upsert performed now goes through `plan_update_planned_at`, which
    /// repoints the parent `Contains` edge in place.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_add_planned_redeclares_own_placeholder_with_corrected_parent() {
        let (apg_root, repo, _wt) = fixture("planned-redeclare");
        let plan_path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();

        // First declaration: a planned File whose parent is an UnresolvedTarget
        // (not code — and not a valid Contains pair, so no DB edge is ever
        // merged for it). The write-through re-ingests the placeholder as a
        // `status: planned` File row.
        plan_add_planned_at(
            &apg_root,
            &mut records,
            "file",
            "/todo/app.ts",
            "app.ts",
            Some("github.com/x/y.Missing"),
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // Re-declaring the SAME FQN is refused (no implicit upsert), naming the
        // update/rm follow-ups, and the on-disk store is byte-identical.
        let before = std::fs::read_to_string(&plan_path).unwrap();
        let err = plan_add_planned_at(
            &apg_root,
            &mut records,
            "file",
            "/todo/app.ts",
            "app.ts",
            Some("github.com/x/y"),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(
            msg.contains("apg plan update foo planned /todo/app.ts"),
            "{msg}"
        );
        assert!(
            msg.contains("apg plan rm foo planned /todo/app.ts"),
            "{msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before,
            "a refused planned re-declaration must not touch the plan store"
        );

        // The parent correction goes through the update core, which repoints
        // the parent Contains edge in place (set-semantics for parent).
        plan_update_planned_at(
            &apg_root,
            &mut records,
            "/todo/app.ts",
            None,
            None,
            Some("github.com/x/y"),
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // Plan records: one PlannedNode at the FQN with the corrected parent,
        // and exactly one Contains record to it (the old parent edge was
        // repointed, not duplicated).
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let parents: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::PlannedNode { fqn, parent, .. } if fqn == "/todo/app.ts" => {
                    Some(parent.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            parents,
            vec!["github.com/x/y"],
            "the planned record carries the corrected parent"
        );
        let contains: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::Contains { from, to } if to == "/todo/app.ts" => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            contains,
            vec!["github.com/x/y"],
            "exactly the corrected parent Contains edge"
        );

        // DB: the placeholder is still a `status: planned` File (not real
        // code), and the corrected parent's Contains edge is the only one.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .q("MATCH (n:File {fqn: '/todo/app.ts'}) RETURN n.status")
            .unwrap();
        assert!(out.contains("planned"), "DB placeholder row: {out}");
        let out = db
            .q("MATCH (p)-[:Contains]->(f:File {fqn: '/todo/app.ts'}) RETURN p.fqn")
            .unwrap();
        assert!(
            out.contains("github.com/x/y"),
            "DB corrected parent edge: {out}"
        );
        assert!(
            !out.contains("github.com/x/y.Missing"),
            "no stale parent edge: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    /// Positive control: a FQN that IS real scanned code is still refused,
    /// with the existing message wording.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_add_planned_refuses_real_scanned_code() {
        let (apg_root, repo, _wt) = fixture("planned-real");
        let _path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();

        // `github.com/x/y.Store` is a real Struct in the fixture DB — planning
        // over it is refused before any record lands.
        let err = plan_add_planned_at(
            &apg_root,
            &mut records,
            "struct",
            "github.com/x/y.Store",
            "Store",
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("already resolves to a `Struct` code node"),
            "{err}"
        );
        assert!(
            records.iter().all(
                |r| !matches!(r, Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Store")
            ),
            "a refused planned node must not leave a record"
        );

        testutil::remove(&repo);
    }

    /// An FQN that matches only an `UnresolvedTarget` is not code: the planned
    /// declaration lands (the old `code_label` probe counted the unresolved
    /// row and refused).
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_add_planned_ignores_unresolved_targets() {
        let (apg_root, repo, _wt) = fixture("planned-unresolved");
        let _path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();

        plan_add_planned_at(
            &apg_root,
            &mut records,
            "function",
            "github.com/x/y.Missing",
            "Missing",
            None,
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();
        assert!(records.iter().any(
            |r| matches!(r, Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Missing")
        ));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn task_verb_modifies_deletes_require_real_scanned_code() {
        let (apg_root, repo, _wt) = fixture("verb-modifies");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];

        // modifies/deletes with an unresolvable FQN are refused.
        for verb in ["modifies", "deletes"] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "github.com/x/y.Nope",
                "",
            )
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("does not resolve in the scanned graph"),
                "{verb}: {err}"
            );
        }
        // modifies/deletes against a still-planned FQN are refused too — only
        // a creates builds a planned node.
        let mut with_planned = records.clone();
        with_planned.push(Record::PlannedNode {
            fqn: "github.com/x/y.Gateway".to_string(),
            kind: "struct".to_string(),
            name: "Gateway".to_string(),
            parent: String::new(),
        });
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut with_planned,
            1,
            1,
            "T",
            "source",
            "",
            "modifies",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("is a planned node, not scanned code"),
            "{err}"
        );

        // modifies/deletes with a real scanned FQN are accepted; the verb and
        // target land on the task record.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "modifies",
            "github.com/x/y.Store",
            "",
        )
        .unwrap();
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            2,
            "T2",
            "source",
            "",
            "deletes",
            "github.com/x/y.Store",
            "",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && verb == "modifies"
                    && target == "github.com/x/y.Store"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-2"
                    && verb == "deletes"
                    && target == "github.com/x/y.Store"
        )));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn task_verb_renames_moves_validate_source_and_new_fqn() {
        let (apg_root, repo, _wt) = fixture("verb-rename");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];

        // renames/moves with an unresolvable source are refused.
        for verb in ["renames", "moves"] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "github.com/x/y.Nope",
                "github.com/x/y.Gateway",
            )
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("does not resolve in the scanned graph"),
                "{verb}: {err}"
            );
        }
        // A rename without the destination is refused.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "renames",
            "github.com/x/y.Store",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("requires --to"), "{err}");

        // A colliding destination is refused — both against another real node
        // and against the source itself.
        for (verb, to) in [
            ("renames", "/abs/store.go"),
            ("moves", "github.com/x/y.Store"),
        ] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "github.com/x/y.Store",
                to,
            )
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("must not collide with existing code"),
                "{verb}: {err}"
            );
        }

        // renames/moves with a resolving source and a free destination are
        // accepted; the pair is recorded on the task.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "renames",
            "github.com/x/y.Store",
            "github.com/x/y.Store2",
        )
        .unwrap();
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            2,
            "T2",
            "source",
            "",
            "moves",
            "github.com/x/y.Store",
            "github.com/x/y.Store2",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, new_fqn, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && verb == "renames"
                    && target == "github.com/x/y.Store"
                    && new_fqn == "github.com/x/y.Store2"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, new_fqn, .. }
                if fqn == "foo/plan.phase-01.task-2"
                    && verb == "moves"
                    && target == "github.com/x/y.Store"
                    && new_fqn == "github.com/x/y.Store2"
        )));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn task_verb_invalid_verbs_and_flag_combinations_refused() {
        let (apg_root, repo, _wt) = fixture("verb-flags");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];

        // Unknown verb refused.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "explodes",
            "github.com/x/y.Store",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid task verb"), "{err}");

        // A non-creates verb without a target FQN is meaningless — refused.
        for verb in ["modifies", "deletes", "renames", "moves"] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "",
                "",
            )
            .unwrap_err();
            assert!(err.to_string().contains("requires --fqn"), "{verb}: {err}");
        }

        // --to is only valid for renames/moves.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "github.com/x/y.Gateway2",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("only valid for renames/moves"),
            "{err}"
        );

        // An omitted verb defaults to creates-without-target — the pre-verb
        // task shape still authors.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "",
            "",
            "",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, new_fqn, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && verb == "creates"
                    && target.is_empty()
                    && new_fqn.is_empty()
        )));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn task_verb_old_format_records_default_to_creates() {
        // A task record authored before the verb model (no verb/target keys)
        // parses with the default verb `creates` and no target — the transient
        // plan store is forward-compatible with old-format tasks.
        let dir = std::env::temp_dir().join(format!("apg-plan-verb-parse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"task","fqn":"foo/plan.phase-01.task-1","title":"T","kind":"source","tier":"","status":"pending"}"#,
        )
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        match &recs[0] {
            Record::Task {
                verb,
                target,
                new_fqn,
                ..
            } => {
                assert_eq!(verb, "creates");
                assert!(target.is_empty(), "old tasks carry no target");
                assert!(new_fqn.is_empty(), "old tasks carry no destination");
            }
            other => panic!("expected a task record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_add_task_rejects_nonexistent_phase() {
        let (apg_root, repo, _wt) = fixture("task-no-phase");
        let _path = write_plan(&apg_root); // plan has phase-01 only

        let mut records = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        // A task under phase 02 (not authored) is rejected before any write.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            2,
            1,
            "T",
            "source",
            "",
            "creates",
            "",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("no such phase"), "{err}");
        assert!(
            records.iter().all(
                |r| !matches!(r, Record::Task { fqn, .. } if fqn.starts_with("foo/plan.phase-02"))
            ),
            "a rejected task must not leave a partial Task record"
        );

        testutil::remove(&repo);
    }

    /// `apg plan link` is retired along with `plan_link_at`; the set-semantics
    /// now live in `plan_update_phase_at`, which refuses an absent phase before
    /// any write — the plan store stays untouched.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_link_rejects_nonexistent_phase() {
        let (apg_root, repo, _wt) = fixture("link-no-phase");
        let plan_path = write_plan(&apg_root); // plan has phase-01 only
        let before = std::fs::read_to_string(&plan_path).unwrap();

        let err = plan_update_phase_at(
            &apg_root,
            "foo",
            &mut load_plan(&apg_root, "foo").unwrap(),
            2,
            Some("ghost"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a phase of `foo`"), "{err}");
        // The plan JSONL is untouched (no partial write).
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before,
            "an absent-phase update must not touch the plan store"
        );
        let recs = specs::read_jsonl(&plan_path).unwrap();
        assert!(
            recs.iter().all(|r| !matches!(r, Record::Satisfies { .. })),
            "a rejected update must not write Satisfies"
        );

        testutil::remove(&repo);
    }

    /// Int (fixture repo/branch DB): `plan add phase|task|planned` refuses an
    /// existing entity (naming `update`/`rm`), leaving the existing
    /// Contains/Reviews edges intact and the plan store untouched.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_add_subentities_refuse_existing_and_preserve_edges() {
        let (apg_root, repo, _wt) = fixture("add-refuse-existing");
        let plan_path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();
        records.push(Record::PlannedNode {
            fqn: "/todo/app.ts".into(),
            kind: "file".into(),
            name: "app.ts".into(),
            parent: "github.com/x/y".into(),
        });
        records.push(Record::Contains {
            from: "github.com/x/y".into(),
            to: "/todo/app.ts".into(),
        });
        records.push(Record::Reviews {
            from: "foo/feedback-1".into(),
            to: "foo/plan.phase-01.task-1".into(),
        });
        specs::write_jsonl(&plan_path, &records).unwrap();
        let before = std::fs::read_to_string(&plan_path).unwrap();

        // Re-adding the existing phase is refused, naming update/rm.
        let err = plan_add_phase_at(
            &apg_root,
            "foo",
            &mut records,
            "foo/plan",
            1,
            "P1b",
            "D",
            &[],
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            err.to_string().contains("apg plan update foo phase 1"),
            "{err}"
        );
        assert!(err.to_string().contains("apg plan rm foo phase 1"), "{err}");
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), before);

        // Re-adding the existing task is refused; its phase Contains edge and
        // incident Reviews edge survive.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T2",
            "source",
            "",
            "creates",
            "",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            err.to_string().contains("apg plan update foo task 1 1"),
            "{err}"
        );
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Reviews { from, to }
                if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
        )));

        // Re-adding the existing planned node is refused; its parent Contains
        // edge survives.
        let err = plan_add_planned_at(
            &apg_root,
            &mut records,
            "file",
            "/todo/app.ts",
            "app.ts",
            Some("github.com/x/y"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            err.to_string()
                .contains("apg plan update foo planned /todo/app.ts"),
            "{err}"
        );
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "github.com/x/y" && to == "/todo/app.ts"
        )));
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before,
            "every refused add leaves the plan store untouched"
        );

        testutil::remove(&repo);
    }

    /// Int: `apg plan link` is retired (unknown subcommand); `apg plan update
    /// <project> phase <n> --satisfies/--prereq` replaces only that phase's
    /// outgoing Satisfies/Gates edges (set-semantics), leaving other phases and
    /// incoming edges intact.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_link_retired_and_update_phase_sets_bridge_edges() {
        let (apg_root, repo, wt) = fixture("link-retired");
        // A `--satisfies` target must resolve in the layers store; author one
        // and re-anchor the recorded scan_meta dirty (the untracked node file
        // makes the tree dirty, which the write-through's staleness gate sees).
        let req = "phase-update-preserves-tasks";
        let req_fqn = format!("requirements.requirement.{req}");
        write_requirement(&apg_root, req);
        testutil::write_scan_meta(
            &apg_root,
            Some(&repo.head_sha()),
            false,
            "2026-09-07T00:00:00Z",
        );
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-03".into(),
                number: 3,
                title: "P3".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "requirements.requirement.R1".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
            // A later phase gating phase-01: an incoming edge, must survive.
            Record::Gates {
                from: "foo/plan.phase-03".into(),
                to: "foo/plan.phase-01".into(),
            },
            // An unrelated phase's own Satisfies must survive.
            Record::Satisfies {
                from: "foo/plan.phase-02".into(),
                to: "requirements.requirement.R3".into(),
            },
        ];
        specs::write_jsonl(&plan_path, &records).unwrap();

        // `apg plan link` is retired at dispatch: an unknown subcommand.
        let err = cmd_plan(&["link".to_string(), "foo".to_string(), "1".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown apg plan subcommand: link"),
            "{err}"
        );

        // The update-phase arm folds link's set-semantics through cmd_plan.
        with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "1".to_string(),
                "--satisfies".to_string(),
                req.to_string(),
                "--prereq".to_string(),
                "2".to_string(),
            ])
        })
        .unwrap();

        let recs = specs::read_jsonl(&plan_path).unwrap();
        let satisfies: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            satisfies,
            vec![req_fqn.as_str()],
            "the passed --satisfies replaces phase-01's outgoing set"
        );
        let gates: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::Gates { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            gates,
            vec!["foo/plan.phase-02"],
            "the passed --prereq replaces phase-01's outgoing gate set"
        );
        // Incoming gate + unrelated phase Satisfies + the task Contains edge
        // survive the set-semantics rewrite.
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-03" && to == "foo/plan.phase-01"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-02" && to == "requirements.requirement.R3"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_complete_writes_durable_phase_status() {
        let (apg_root, repo, _wt) = fixture("durable-phase");

        // Pending task → the phase-complete gate rejects (nothing written).
        let _path = write_plan(&apg_root);
        assert!(plan_complete_at(&apg_root, "foo", 1).is_err());
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase { fqn, status, .. } if fqn == "foo/plan.phase-01" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            status, "pending",
            "a rejected complete must not flip status"
        );

        // Gate green → the milestone is durably recorded: status flips to done
        // in the JSONL (distinguishable from tasks-done + feedback-resolved).

        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase { fqn, status, .. } if fqn == "foo/plan.phase-01" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "done", "complete must write the durable milestone");

        // The DB carries it too (the plan write-through re-ingests).
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .q(&format!(
                "MATCH (p:PlanPhase {{fqn: {}}}) RETURN p.status",
                artifacts::lit("foo/plan.phase-01")
            ))
            .unwrap()
            .to_string();
        assert!(out.contains("done"), "phase status in DB: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn apply_gate_rejects_unresolved_target_as_unrealized() {
        let (apg_root, repo, _wt) = fixture("apply-unresolved");

        // The planned node's FQN matches an UnresolvedTarget node in the graph
        // (an unresolved reference, NOT real code) — the gate must block.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Missing".to_string(),
                kind: "struct".to_string(),
                name: "Missing".to_string(),
                parent: String::new(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "resolved".to_string(),
                disposition: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("not realized"), "{err}");
        assert!(err.to_string().contains("github.com/x/y.Missing"), "{err}");

        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // coverage_check: derived solution coverage in plan verify — every
    // in-scope solution node's implemented-by FQN (reached from a satisfied
    // requirement, or added on this branch) must be touched by at least one
    // plan task (SPEC §5); the coherence gate refuses when it does not.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_holds_when_every_implemented_by_fqn_is_touched() {
        // One solution node whose implemented-by FQNs are a real scanned
        // Struct (covered by a modifies task) and an absent FQN (covered by a
        // creates task — the planned-node case: a creates over a still-absent
        // FQN counts exactly like a modifies over real code). The node is
        // ADDED ON THIS BRANCH, so it is in scope even though no satisfied
        // requirement reaches it; there is no cumulative-store assumption —
        // a pre-existing unreachable node would be exempt (task-7's int test).
        let (apg_root, repo, wt) = fixture("coverage-ok");
        write_solution_node(
            &apg_root,
            "system",
            "payments",
            &["github.com/x/y.Store", "github.com/x/y.Gateway"],
        );
        let sha = wt_commit_paths(
            &wt,
            &["apg/layers/solution/system/payments.json"],
            "author solution node",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "done".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-2".to_string(),
            title: "T2".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "creates".to_string(),
            target: "github.com/x/y.Gateway".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-2".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        // Every implemented-by FQN is touched -> the bridge is complete.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_refuses_when_an_implemented_by_fqn_is_untouched() {
        // Two branch-added solution nodes; the plan touches only the real
        // Struct — the container's absent Gateway FQN is uncovered, so verify
        // refuses, naming the FQN, its solution node, and the matching creates
        // suggestion.
        let (apg_root, repo, wt) = fixture("coverage-gap");
        write_solution_node(&apg_root, "container", "api", &["github.com/x/y.Gateway"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/container/api.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("coverage incomplete"), "{msg}");
        assert!(msg.contains("solution.container.api"), "{msg}");
        assert!(msg.contains("github.com/x/y.Gateway"), "{msg}");
        assert!(
            msg.contains("--verb creates --fqn github.com/x/y.Gateway"),
            "{msg}"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_renames_moves_destination_counts() {
        // A branch-added node whose renames task destination equals the
        // implemented-by FQN touches it (the new_fqn half of the pair) — the
        // bridge holds under branch-delta scoping.
        let (apg_root, repo, wt) = fixture("coverage-rename");
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store2"],
        );
        let sha = wt_commit_paths(
            &wt,
            &["apg/layers/solution/component/checkout.json"],
            "author solution node",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "renames".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: "github.com/x/y.Store2".to_string(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_trivially_holds_with_no_solution_nodes() {
        // No solution-layer node files at all — nothing is spine-reached and
        // the branch delta is empty, so coverage is a no-op and verify passes
        // on the other gates alone.
        let (apg_root, repo, _wt) = fixture("coverage-empty");
        let _path = write_plan(&apg_root);
        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_exempts_solution_node_without_implemented_by_edge() {
        // A BRANCH-ADDED solution node with NO implemented-by edge stays
        // exempt — the rule is over the node's implemented-by FQNs, and with
        // none there is nothing to touch (nothing blocks). The gap is surfaced
        // as a warning (no code claims the node), never a blocker: the
        // no-claims list is part of the report.
        let (apg_root, repo, wt) = fixture("coverage-no-claim");
        write_solution_node(&apg_root, "system", "payments", &[]);
        let sha = wt_commit_paths(
            &wt,
            &["apg/layers/solution/system/payments.json"],
            "author solution node",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let records = bare_plan();
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        let nodes = apg::layers::read_existing_nodes(&apg_root).unwrap();
        let branch_added: BTreeSet<String> = ["solution.system.payments".to_string()]
            .into_iter()
            .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert_eq!(report.no_claims, vec!["solution.system.payments"]);
        assert!(report.gaps.is_empty());

        // The same node, neither branch-added nor spine-reached, is ignored
        // entirely (the pre-existing exemption).
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.no_claims.is_empty(), "{report:?}");
        assert!(report.gaps.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_verify_at_supplies_spine_and_branch_delta() {
        // plan_verify_at computes the satisfied-requirement set from the
        // plan's Satisfies edges and the branch delta against the repo's
        // default branch (the public repo_identity default_branch). A
        // pre-existing unreachable node (committed on `main` BEFORE the
        // project branch) forces no gap; the reached + branch-added nodes'
        // implemented-by FQNs must be touched.
        let repo = Repo::new("verify-spine");
        // The pre-existing worktree-cleanup nodes: present on `main` before the
        // branch is cut, so they are not part of the branch delta and — being
        // unreachable from the plan's satisfied requirement — must force no
        // gap and no fake `modifies` task.
        for (node_type, name, code) in [
            (
                "component",
                "project-delete",
                "apg.project_cmd.delete_project",
            ),
            (
                "component",
                "project-dispatch",
                "apg.project_cmd.cmd_project",
            ),
            (
                "component",
                "project-merge",
                "apg.project_cmd.project_merge_at",
            ),
            (
                "container",
                "project-command",
                "apg.project_cmd.cmd_project",
            ),
        ] {
            repo.write(
                &format!("apg/layers/solution/{node_type}/{name}.json"),
                &serde_json::to_string_pretty(&nf(
                    "solution",
                    node_type,
                    name,
                    &[("implemented-by", code)],
                ))
                .unwrap(),
            );
        }
        repo.commit_all("author pre-existing worktree-cleanup nodes");
        let wt = repo.start_project("foo");
        db_at(&wt);
        let apg_root = wt.join(specs::LAYOUT);

        // The branch's spine + branch-added solution nodes.
        write_node_file(
            &apg_root,
            "requirements",
            "requirement",
            "cr",
            &[("drives", "domain.entity.plan-record")],
        );
        write_node_file(
            &apg_root,
            "domain",
            "entity",
            "plan-record",
            &[(
                "realised-by",
                "solution.component.coverage-spine-validation",
            )],
        );
        write_solution_node(
            &apg_root,
            "component",
            "coverage-spine-validation",
            &["github.com/x/y.Store"],
        );
        write_solution_node(&apg_root, "component", "plan-store-atomic-rewrite", &[]);
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/requirements/requirement/cr.json",
                "apg/layers/domain/entity/plan-record.json",
                "apg/layers/solution/component/coverage-spine-validation.json",
                "apg/layers/solution/component/plan-store-atomic-rewrite.json",
            ],
            "author the branch spine and solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        // Green: the reached/branch-added FQN is touched; the pre-existing
        // unreachable node and the no-claim node force no gap.
        let mut records = bare_plan();
        records.push(Record::Satisfies {
            from: "foo/plan.phase-01".to_string(),
            to: "requirements.requirement.cr".to_string(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        records.push(task_rec("modifies", "github.com/x/y.Store", ""));
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();
        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        // Refusal: drop the touching task -> the reached node's FQN is a gap,
        // named with its solution node; the pre-existing node is still not
        // named (no false gap).
        let mut records = bare_plan();
        records.push(Record::Satisfies {
            from: "foo/plan.phase-01".to_string(),
            to: "requirements.requirement.cr".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("coverage incomplete"), "{msg}");
        assert!(
            msg.contains("solution.component.coverage-spine-validation"),
            "{msg}"
        );
        assert!(msg.contains("github.com/x/y.Store"), "{msg}");
        // None of the pre-existing unreachable worktree-cleanup nodes is named.
        for name in [
            "project-delete",
            "project-dispatch",
            "project-merge",
            "project-command",
        ] {
            assert!(!msg.contains(name), "{name} named in: {msg}");
        }

        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // task-5 (coverage tests): the task-text audit gaps — the green path
    // across MULTIPLE solution nodes, both halves of a renames/moves pair as
    // touches, and a refusal that names ONLY the uncovered node.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_holds_across_multiple_solution_nodes() {
        // The bridge is complete only when EVERY in-scope solution node's
        // implemented-by FQNs are touched — this is the green end-to-end path
        // through plan_verify_at across MULTIPLE branch-added solution nodes
        // (two containers; one real FQN, one still-absent FQN), not just
        // several FQNs on a single node. Verify returns its green verdict and
        // the derived report agrees: no gaps, no no-claims warnings.
        let (apg_root, repo, wt) = fixture("coverage-multi-ok");
        write_solution_node(&apg_root, "container", "api", &["github.com/x/y.Gateway"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/container/api.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "creates".to_string(),
            target: "github.com/x/y.Gateway".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-2".to_string(),
            title: "T2".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-2".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        // Every in-scope solution node's every implemented-by FQN is touched
        // -> green.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        let nodes = apg::layers::read_existing_nodes(&apg_root).unwrap();
        let branch_added: BTreeSet<String> =
            ["solution.container.api", "solution.component.checkout"]
                .into_iter()
                .map(str::to_string)
                .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert!(report.gaps.is_empty(), "{report:?}");
        assert!(report.no_claims.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_refusal_names_only_the_uncovered_solution() {
        // Mixed coverage across two branch-added nodes: the component's real
        // FQN is covered by a modifies task, the container's absent FQN is
        // not. The refusal names the uncovered node/FQN only — the covered
        // node and its claim appear nowhere in the message (no false positives
        // for a partially-covered bridge).
        let (apg_root, repo, wt) = fixture("coverage-mixed-names");
        write_solution_node(&apg_root, "container", "api", &["github.com/x/y.Gateway"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/container/api.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("coverage incomplete"), "{msg}");
        assert!(msg.contains("solution.container.api"), "{msg}");
        assert!(msg.contains("github.com/x/y.Gateway"), "{msg}");
        // The covered component and its claim are NOT named.
        assert!(!msg.contains("solution.component.checkout"), "{msg}");
        assert!(!msg.contains("github.com/x/y.Store"), "{msg}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn coverage_moves_touches_source_and_destination_halves() {
        // A renames/moves task claims the code at BOTH FQNs: `target` (the
        // source, where the code was) and `new_fqn` (the destination, where it
        // lands). Here the two halves are different branch-added solution
        // nodes' implemented-by targets, so the single `moves` covers both and
        // the bridge holds under branch-delta scoping. (coverage_check counts
        // `target` for every verb plus `new_fqn` for renames/moves — the pair
        // is one claim across two locations; the existing renames test covers
        // the destination half.)
        let (apg_root, repo, wt) = fixture("coverage-rename-both");
        write_solution_node(&apg_root, "system", "payments", &["github.com/x/y.Store"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store2"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/system/payments.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "moves".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: "github.com/x/y.Store2".to_string(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        let nodes = apg::layers::read_existing_nodes(&apg_root).unwrap();
        let branch_added: BTreeSet<String> =
            ["solution.system.payments", "solution.component.checkout"]
                .into_iter()
                .map(str::to_string)
                .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert!(report.gaps.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn scoped_review_feedback_routes_and_gates_by_scope() {
        // Scoped-review routing (structural vs phase): feedback on the Plan
        // node is the structural scope, feedback on a PlanPhase is the phase
        // scope. The phase-complete gate only checks its phase's scope; the
        // apply gate checks every scope — so structural feedback does not
        // block a phase's completion milestone but blocks apply.
        let (apg_root, repo, _wt) = fixture("scoped-review");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
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
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            // Structural scope: an open review on the Plan node itself.
            Record::Feedback {
                fqn: "foo/feedback-structural".to_string(),
                body: "breakdown issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-structural".to_string(),
                to: "foo/plan".to_string(),
            },
            // Phase scope: an open review on the phase.
            Record::Feedback {
                fqn: "foo/feedback-phase".to_string(),
                body: "phase issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-phase".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // Phase completion is blocked by the PHASE-scope feedback but NOT the
        // structural (Plan-scope) feedback — the milestone routes by scope.
        // So: resolve the phase feedback, leave the structural open.
        let mut recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        for r in &mut recs {
            if let Record::Feedback { fqn, status, .. } = r
                && fqn == "foo/feedback-phase"
            {
                *status = "resolved".to_string();
            }
        }
        specs::write_jsonl(&path, &recs).unwrap();
        assert!(
            plan_complete_at(&apg_root, "foo", 1).is_ok(),
            "structural (Plan-scope) feedback must not block the phase milestone"
        );

        // Apply is blocked by the STRUCTURAL feedback — every scope must be
        // green at the coherence gate.
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(
            err.to_string().contains("unresolved review feedback"),
            "{err}"
        );
        assert!(err.to_string().contains("foo/feedback-structural"), "{err}");

        testutil::remove(&repo);
    }

    /// The coherence gate spans every feedback scope: an open Feedback in a
    /// project's tier mirror (here a durable-node review in
    /// `.trans/requirements/foo.jsonl`) blocks verify and is named, while the
    /// milestone-only `plan complete` stays phase-scoped (it reads only the
    /// plan store).
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_verify_gates_feedback_in_tier_mirrors() {
        let (apg_root, repo, _wt) = fixture("verify-tiers");

        // Plan store: a plan, a completed phase/task, and a realized planned
        // node so verify has no blocker of its own.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
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
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
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
        specs::write_jsonl(&path, &records).unwrap();

        // A durable-node review lands in the requirements tier mirror — NOT the
        // plan store, so the old plan-store-only check never saw it.
        let mirror =
            specs::transient_feedback_path(&apg_root, "foo", apg::layers::Layer::Requirements);
        let feedback = vec![
            Record::Feedback {
                fqn: "foo/feedback-durable".to_string(),
                body: "durable issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-durable".to_string(),
                to: "requirements.requirement.timer".to_string(),
            },
        ];
        specs::write_jsonl(&mirror, &feedback).unwrap();

        // `plan complete` is phase-scoped: feedback outside the phase/its tasks
        // does not gate the milestone.
        assert!(
            plan_complete_at(&apg_root, "foo", 1).is_ok(),
            "phase milestone must ignore feedback outside its scope"
        );

        // The coherence gate DOES see it: verify refuses, naming the FQN.
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(
            err.to_string().contains("unresolved review feedback"),
            "{err}"
        );
        assert!(err.to_string().contains("foo/feedback-durable"), "{err}");

        // Resolve it → verify passes.
        let resolved: Vec<Record> = feedback
            .iter()
            .map(|r| match r {
                Record::Feedback {
                    fqn,
                    body,
                    disposition,
                    ..
                } => Record::Feedback {
                    fqn: fqn.clone(),
                    body: body.clone(),
                    status: "resolved".to_string(),
                    disposition: disposition.clone(),
                },
                other => other.clone(),
            })
            .collect();
        specs::write_jsonl(&mirror, &resolved).unwrap();
        assert!(
            plan_verify_at(&apg_root, "foo").is_ok(),
            "verify must pass once every tier-mirror Feedback is resolved"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_init_works_and_warns_without_spec_jsonl() {
        // The legacy spec-exists gate is gone: no `apg/specs/<project>.jsonl`
        // (and no requirement node files yet) still allows init — the empty
        // spec is surfaced by the caller, never a blocker. With a requirement
        // in the layers store the gate resolves green.
        let (apg_root, repo, _wt) = fixture("init-no-spec");

        // No requirements yet: init works and reports the warning condition.
        let has = plan_init_at(&apg_root, "foo", "Plan Foo", "S").unwrap();
        assert!(!has, "no requirement node files -> the warning path");
        assert!(
            specs::plan_jsonl_path(&apg_root, "foo").exists(),
            "init must create the plan store even without requirements"
        );
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(
            recs.iter()
                .any(|r| matches!(r, Record::Plan { fqn, .. } if fqn == "foo/plan")),
            "the Plan record must land in .trans/plans"
        );
        // The legacy spec store is never created or referenced.
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );

        // A requirement node file in the layers store satisfies the gate —
        // under a second project context (init is a per-branch act).
        let wt2 = repo.start_project("bar");
        let bar_apg = wt2.join(specs::LAYOUT);
        let req_file = apg::layers::node_file_path(
            &bar_apg,
            apg::layers::Layer::Requirements,
            "requirement",
            "timer",
        );
        std::fs::create_dir_all(req_file.parent().unwrap()).unwrap();
        std::fs::write(
            &req_file,
            r#"{"name":"timer","type":"requirement","layer":"requirements","body":"x","properties":{},"out":[],"in":[]}"#,
        )
        .unwrap();
        let has = plan_init_at(&bar_apg, "bar", "Plan Bar", "S2").unwrap();
        assert!(
            has,
            "a requirement node file must satisfy the spec-exists gate"
        );
        assert!(specs::plan_jsonl_path(&bar_apg, "bar").exists());
        assert!(
            !bar_apg.join("specs").exists(),
            "apg/specs must never be written"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_mutations_never_commit() {
        // Transience (SPEC §4.2/§5): plan mutations write the gitignored
        // `.trans/plans/` store and NEVER auto-commit — the branch HEAD does
        // not move and the tree stays clean through add/done/note mutations.
        let (apg_root, repo, _wt) = fixture("never-commit");
        let _path = write_plan(&apg_root);
        let head_before = repo.head_sha();

        // plan add (a phase), plan done (assertion), plan note — three
        // mutations through the central funnel.
        let mut records = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let plan_fqn = "foo/plan".to_string();
        plan_add_phase_at(
            &apg_root,
            "foo",
            &mut records,
            &plan_fqn,
            2,
            "P2",
            "D2",
            &[],
            &[],
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_note_at(
            &apg_root,
            "foo",
            "foo/plan.phase-01.task-1",
            "concern noted",
            "note",
        )
        .unwrap();

        // The plan state landed in the transient store...
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::PlanPhase { fqn, .. } if fqn == "foo/plan.phase-02"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Note { fqn, .. } if fqn == "foo/plan.note-1"
        )));
        // ...and nothing was committed: `.trans` is gitignored and transient.
        assert_eq!(
            repo.head_sha(),
            head_before,
            "plan mutations must never auto-commit"
        );
        assert!(
            repo.is_clean(),
            "the tree must stay clean — plan files are gitignored"
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_remaining_mutation_paths_never_commit() {
        // Transience surface completion (SPEC §4.2/§5):
        // `plan_mutations_never_commit` covers add-phase/done/note; this
        // closes the rest of the mutation surface: init, task add (an
        // accepted creates through the write-through funnel, and a refused
        // modifies leaving the on-disk store byte-identical), planned-node
        // declaration, phase update (the retired link's successor), complete
        // (the durable milestone), and undone.
        // Every mutation writes only the gitignored `.trans/plans/` store:
        // the project branch HEAD (where an auto-commit would land), the main
        // HEAD, and the tree all stay untouched.
        let (apg_root, repo, wt) = fixture("never-commit-rest");
        let head_before = repo.head_sha();
        let branch_head_before = wt_head(&wt);

        // init writes the Plan record into the transient store.
        let has = plan_init_at(&apg_root, "foo", "P", "S").unwrap();
        assert!(!has, "no requirement node files -> the warning path");
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");

        // A phase add through the write-through funnel.
        let mut records = specs::read_jsonl(&plan_path).unwrap();
        plan_add_phase_at(
            &apg_root,
            "foo",
            &mut records,
            "foo/plan",
            1,
            "P1",
            "D1",
            &[],
            &[],
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // A refused modifies (unresolvable FQN) leaves the on-disk store
        // byte-identical — no partial Task record, no write, no commit.
        let before_file = std::fs::read_to_string(&plan_path).unwrap();
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "modifies",
            "github.com/x/y.Nope",
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not resolve in the scanned graph"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before_file,
            "a refused task add must not touch the plan store"
        );

        // An accepted creates against an unplanned FQN lands through the
        // funnel, verb + target on the task record.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // The planned-node declaration (the `plan add planned` write).
        records.push(Record::PlannedNode {
            fqn: "github.com/x/y.Gateway".to_string(),
            kind: "struct".to_string(),
            name: "Gateway".to_string(),
            parent: String::new(),
        });
        write_through(&apg_root, "foo", &records).unwrap();

        // update phase (an in-place write-through — the retired link's
        // successor), then done → complete (the durable milestone) → undone.
        plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            Some("P1b"),
            None,
            None,
            None,
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();
        plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();

        // The state landed in the transient store: the milestone, the task
        // (undone) with its verb + target, and the planned node.
        let recs = specs::read_jsonl(&plan_path).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::PlanPhase { fqn, status, .. }
                if fqn == "foo/plan.phase-01" && status == "done"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Task { fqn, status, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && status == "pending"
                    && verb == "creates"
                    && target == "github.com/x/y.Gateway"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Gateway"
        )));

        // ...and nothing was committed: `.trans` is gitignored and transient.
        assert_eq!(
            wt_head(&wt),
            branch_head_before,
            "plan mutations must never auto-commit on the project branch"
        );
        assert_eq!(
            repo.head_sha(),
            head_before,
            "plan mutations must never move the main HEAD either"
        );
        assert!(
            repo.is_clean(),
            "the tree must stay clean — plan files are gitignored"
        );

        testutil::remove(&repo);
    }

    /// Unit: `plan_update_at` MERGEs title/strategy independently and preserves
    /// every phase/task/planned record and every plan edge
    /// (`Contains`/`Gates`/`Satisfies`/`Reviews`); an absent plan is refused.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_update_at_merges_and_preserves_every_record_and_edge() {
        let (apg_root, repo, _wt) = fixture("plan-update-merge");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: "S0".to_string(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: "github.com/x/y.Gateway".to_string(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".to_string(),
                to: "requirements.requirement.r1".to_string(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-00".to_string(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "b".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // Every non-Plan record, serialized — must be byte-identical before
        // and after each update (phase/task/planned records + all plan edges).
        let others = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .filter(|r| !matches!(r, Record::Plan { .. }))
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };
        let others_before = others(&records);

        // `--title` alone: strategy unchanged.
        plan_update_at(&apg_root, "foo", Some("New"), None).unwrap();
        assert_eq!(
            plan_fields(&apg_root),
            ("New".to_string(), "S0".to_string())
        );
        assert_eq!(
            others(&specs::read_jsonl(&path).unwrap()),
            others_before,
            "a title-only update must preserve every record and edge"
        );

        // `--strategy` alone: title unchanged.
        plan_update_at(&apg_root, "foo", None, Some("S2")).unwrap();
        assert_eq!(
            plan_fields(&apg_root),
            ("New".to_string(), "S2".to_string())
        );
        assert_eq!(
            others(&specs::read_jsonl(&path).unwrap()),
            others_before,
            "a strategy-only update must preserve every record and edge"
        );

        // Both flags merge together.
        plan_update_at(&apg_root, "foo", Some("T3"), Some("S3")).unwrap();
        assert_eq!(plan_fields(&apg_root), ("T3".to_string(), "S3".to_string()));

        // An absent plan is refused, naming `apg plan add`.
        let err = plan_update_at(&apg_root, "ghost", Some("X"), None).unwrap_err();
        assert!(err.to_string().contains("apg plan add ghost"), "{err}");

        testutil::remove(&repo);
    }

    /// Int (fixture repo/branch DB): the plan-record CLI surface — `apg plan
    /// add <project>` creates and refuses an existing plan; `apg plan update`
    /// MERGEs title/strategy independently and refuses an absent plan; the
    /// retired `apg plan init` is an unknown subcommand.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_add_update_cli_and_init_retirement() {
        let (apg_root, repo, wt) = fixture("plan-add-update-cli");

        // `apg plan init` is retired at dispatch: an unknown subcommand. The
        // match fails before any root resolution, so no cwd is needed.
        let err = cmd_plan(&["init".to_string(), "foo".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown apg plan subcommand: init"),
            "{err}"
        );

        // `apg plan add <project>` (no second positional) creates the plan.
        with_cwd(&wt, || {
            cmd_plan(&[
                "add".to_string(),
                "foo".to_string(),
                "--title".to_string(),
                "T1".to_string(),
                "--strategy".to_string(),
                "S1".to_string(),
            ])
        })
        .unwrap();
        assert_eq!(plan_fields(&apg_root), ("T1".to_string(), "S1".to_string()));

        // `apg plan add <project>` refuses when the plan already exists.
        let err = with_cwd(&wt, || {
            cmd_plan(&["add".to_string(), "foo".to_string()]).unwrap_err()
        });
        assert!(err.to_string().contains("already exists"), "{err}");

        // `apg plan update`: `--title` alone preserves strategy.
        with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "foo".to_string(),
                "--title".to_string(),
                "T2".to_string(),
            ])
        })
        .unwrap();
        assert_eq!(plan_fields(&apg_root), ("T2".to_string(), "S1".to_string()));

        // `--strategy` alone preserves title.
        with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "foo".to_string(),
                "--strategy".to_string(),
                "S2".to_string(),
            ])
        })
        .unwrap();
        assert_eq!(plan_fields(&apg_root), ("T2".to_string(), "S2".to_string()));

        // Updating an absent plan is refused, naming `apg plan add`.
        let err = with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "ghost".to_string(),
                "--title".to_string(),
                "X".to_string(),
            ])
            .unwrap_err()
        });
        assert!(err.to_string().contains("apg plan add ghost"), "{err}");

        testutil::remove(&repo);
    }

    /// Unit: every plan/phase/task/planned rm refuses while the entity still
    /// has a dependent (plan-with-phases, phase-with-tasks, `done` or
    /// feedback-bearing task, creates-targeted planned node), naming the
    /// dependent plus the `--force` escape; a non-existent entity is a hard
    /// error; every refusal leaves the on-disk JSONL byte-identical.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_rm_refusals_name_dependents_and_leave_the_store_intact() {
        let (apg_root, repo, _wt) = fixture("rm-refusal");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: "github.com/x/y".to_string(),
            },
            Record::Contains {
                from: "github.com/x/y".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        // A plan with a phase refuses, naming the phase + --force.
        let err = plan_rm_at(&apg_root, "foo", false).unwrap_err().to_string();
        assert!(err.contains("foo/plan.phase-01"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A phase with a task refuses, naming the task + --force.
        let err = plan_rm_phase_at(&apg_root, "foo", 1, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo/plan.phase-01.task-1"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A done task refuses, naming the status + --force.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let before_done = std::fs::read_to_string(&path).unwrap();
        let err = plan_rm_task_at(&apg_root, "foo", 1, 1, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("done"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_done);

        // A pending task with incident feedback refuses, naming the feedback.
        plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let mut recs = specs::read_jsonl(&path).unwrap();
        recs.push(Record::Feedback {
            fqn: "foo/feedback-1".to_string(),
            body: "b".to_string(),
            status: "open".to_string(),
            disposition: String::new(),
        });
        recs.push(Record::Reviews {
            from: "foo/feedback-1".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&path, &recs).unwrap();
        let before_fb = std::fs::read_to_string(&path).unwrap();
        let err = plan_rm_task_at(&apg_root, "foo", 1, 1, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo/feedback-1"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_fb);

        // A creates-targeted planned node refuses, naming the task + --force.
        let err = plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Store", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo/plan.phase-01.task-1"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_fb);

        // Non-existent entities are hard errors, never a silent no-op.
        assert!(plan_rm_at(&apg_root, "ghost", true).is_err());
        assert!(plan_rm_phase_at(&apg_root, "foo", 9, false).is_err());
        assert!(plan_rm_task_at(&apg_root, "foo", 1, 9, false).is_err());
        assert!(plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Nope", false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_fb);

        testutil::remove(&repo);
    }

    /// Unit: `--force` cascades leave no orphan records — no task without its
    /// phase, no edge to a removed plan/phase/task/planned record or Note; a
    /// phase whose only dependents are Feedback/Note is removable WITHOUT
    /// `--force` (its details edge and an orphaned Note go with it, while a
    /// `Feedback` record and its `Reviews` reference survive the removal of the
    /// node it reviews — a review item is closed by a reviewer, never dropped
    /// by target removal, so `feedback-persists-across-target-loss`). A plan
    /// `--force` cascade removes every plan-family record but leaves the store
    /// file in place while surviving Feedback exists.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_rm_cascades_leave_no_orphan_records() {
        let (apg_root, repo, _wt) = fixture("rm-cascade");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            // Plan-level (structural) feedback: its Reviews target is removed
            // only by the plan rm.
            Record::Feedback {
                fqn: "foo/feedback-plan".to_string(),
                body: "structural".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-plan".to_string(),
                to: "foo/plan".to_string(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
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
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-02".to_string(),
            },
            Record::Gates {
                from: "foo/plan.phase-02".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".to_string(),
                to: "requirements.requirement.r1".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: "github.com/x/y".to_string(),
            },
            Record::Contains {
                from: "github.com/x/y".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
            // Task-level feedback: its Reviews target is removed by the task
            // cascade; the record survives either way.
            Record::Feedback {
                fqn: "foo/feedback-task".to_string(),
                body: "task issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-task".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            // Phase-level feedback + a task note: the phase rm GC's the
            // orphaned Note while the Feedback record survives.
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "phase issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Note {
                fqn: "foo/plan.note-1".to_string(),
                body: "note".to_string(),
                kind: "note".to_string(),
            },
            Record::Details {
                from: "foo/plan.note-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // The creates-targeted planned node refuses without --force; --force
        // removes it plus its parent Contains edge, leaving no dangling edge.
        assert!(plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Store", false).is_err());
        plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Store", true).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(!recs.iter().any(
            |r| matches!(r, Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Store")
        ));
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Contains { to, .. } if to == "github.com/x/y.Store"))
        );
        assert_no_orphans(&recs);

        // The feedback-bearing task refuses without --force; --force removes
        // the task and its phase Contains edge, while the task's Feedback
        // record and its Reviews reference survive.
        assert!(plan_rm_task_at(&apg_root, "foo", 1, 1, false).is_err());
        plan_rm_task_at(&apg_root, "foo", 1, 1, true).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(
            !recs.iter().any(|r| matches!(r, Record::Task { .. })),
            "no task may survive its phase's task cascade"
        );
        assert!(
            recs.iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-task")),
            "the task's feedback survives its target's removal"
        );
        assert!(
            recs.iter().any(|r| matches!(r, Record::Reviews { from, to }
                if from == "foo/feedback-task" && to == "foo/plan.phase-01.task-1")),
            "the retained feedback keeps its Reviews reference"
        );
        assert_no_orphans(&recs);

        // A phase whose only dependents are Feedback/Note is removable WITHOUT
        // --force: its incident edges go with it, the dependent Feedback
        // survives (with its Reviews reference), and the orphaned Note is GC'd.
        plan_rm_phase_at(&apg_root, "foo", 1, false).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::PlanPhase { fqn, .. } if fqn == "foo/plan.phase-01"))
        );
        assert!(
            recs.iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-1")),
            "the phase's feedback survives its target's removal"
        );
        assert!(
            recs.iter().any(|r| matches!(r, Record::Reviews { from, to }
                if from == "foo/feedback-1" && to == "foo/plan.phase-01")),
            "the retained feedback keeps its Reviews reference"
        );
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Note { fqn, .. } if fqn == "foo/plan.note-1")),
            "an orphaned Note is garbage-collected"
        );
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Gates { from, .. } if from == "foo/plan.phase-02")),
            "the phase-02 gate on the removed phase must go too"
        );
        assert_no_orphans(&recs);

        // The plan still carries phase-02, so a plain plan rm refuses...
        assert!(plan_rm_at(&apg_root, "foo", false).is_err());

        // ...and a plan --force cascade removes every plan-family record while
        // the plan-level Feedback (and each already-orphaned Feedback) survives
        // — so the store is not emptied and the file is left in place.
        plan_rm_at(&apg_root, "foo", true).unwrap();
        assert!(
            path.exists(),
            "a store with surviving feedback must not be deleted"
        );
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(
            !recs.iter().any(|r| matches!(
                r,
                Record::Plan { .. }
                    | Record::PlanPhase { .. }
                    | Record::Task { .. }
                    | Record::PlannedNode { .. }
            )),
            "the whole-plan cascade removes every plan-family record"
        );
        for fqn in ["foo/feedback-plan", "foo/feedback-task", "foo/feedback-1"] {
            assert!(
                recs.iter()
                    .any(|r| matches!(r, Record::Feedback { fqn: f, .. } if f == fqn)),
                "feedback `{fqn}` survives the plan cascade"
            );
            assert!(
                recs.iter()
                    .any(|r| matches!(r, Record::Reviews { from, .. } if from == fqn)),
                "feedback `{fqn}` keeps its Reviews reference"
            );
        }
        assert_no_orphans(&recs);

        testutil::remove(&repo);
    }

    /// Unit: an error mid-cascade leaves the original plan file exactly as it
    /// was — the whole-record rewrite is atomic, never a half-deleted plan.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_rm_error_mid_cascade_leaves_the_store_untouched() {
        let (apg_root, repo, _wt) = fixture("rm-atomic");
        let path = write_plan(&apg_root);
        let before = std::fs::read(&path).unwrap();

        // Force the single write-through to fail: record a scan_meta that does
        // not match the live git state, so the stale gate refuses the re-ingest
        // after the whole cascade has been computed in memory.
        testutil::write_scan_meta(
            &apg_root,
            Some("0000000000000000000000000000000000000000"),
            true,
            "2026-09-07T00:00:00Z",
        );

        let err = plan_rm_at(&apg_root, "foo", true).unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a mid-cascade failure must leave the plan store byte-identical"
        );

        testutil::remove(&repo);
    }

    /// Int (fixture repo/branch DB): the `apg plan rm` CLI surface — a
    /// dependent-bearing plan/phase refuses (non-zero, naming the dependent
    /// plus the `--force` escape), the same rm with `--force` cascades, an
    /// absent entity is a hard error, and `rm plan --force` deletes the JSONL
    /// so a following `apg plan add` recreates it (rm→add round-trip).
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn plan_rm_cli_refusal_force_and_rm_add_roundtrip() {
        let (apg_root, repo, wt) = plan_store_fixture("rm-cli");
        let path = write_plan(&apg_root);

        // One cwd hold for the whole surface: `cmd_plan` resolves `apg/` by
        // walking up from cwd, so the DB-free path fits every command under
        // a single `CWD_LOCK` acquisition instead of one per command.
        with_cwd(&wt, || {
            // A plan with a phase refuses, naming the phase + --force.
            let err = cmd_plan(&["rm".to_string(), "foo".to_string()]).unwrap_err();
            assert!(err.to_string().contains("foo/plan.phase-01"), "{err}");
            assert!(err.to_string().contains("--force"), "{err}");
            assert!(path.exists());

            // A phase with a task refuses, naming the task + --force.
            let err = cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "1".to_string(),
            ])
            .unwrap_err();
            assert!(
                err.to_string().contains("foo/plan.phase-01.task-1"),
                "{err}"
            );
            assert!(err.to_string().contains("--force"), "{err}");

            // A done task refuses without --force, cascades with it.
            plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
            let err = cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "task".to_string(),
                "1".to_string(),
                "1".to_string(),
            ])
            .unwrap_err();
            assert!(err.to_string().contains("done"), "{err}");
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "task".to_string(),
                "1".to_string(),
                "1".to_string(),
                "--force".to_string(),
            ])
            .unwrap();
            let recs = specs::read_jsonl(&path).unwrap();
            assert!(!recs.iter().any(|r| matches!(r, Record::Task { .. })));

            // A phase with no tasks removes without --force.
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "1".to_string(),
            ])
            .unwrap();
            assert!(
                !specs::read_jsonl(&path)
                    .unwrap()
                    .iter()
                    .any(|r| matches!(r, Record::PlanPhase { .. }))
            );

            // `rm foo --force` deletes the whole plan JSONL...
            cmd_plan(&["rm".to_string(), "foo".to_string(), "--force".to_string()]).unwrap();
            assert!(
                !path.exists(),
                "a plan --force rm must delete the plan JSONL"
            );

            // ...so the following `apg plan add foo` recreates it (rm→add).
            cmd_plan(&["add".to_string(), "foo".to_string()]).unwrap();
            assert!(path.exists(), "apg plan add must recreate the removed plan");

            // A non-existent entity is a hard error.
            let err = cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "9".to_string(),
            ])
            .unwrap_err();
            assert!(err.to_string().contains("no phase 9"), "{err}");
            let err = cmd_plan(&["rm".to_string(), "ghost".to_string()]).unwrap_err();
            assert!(
                err.to_string().contains("no plan for project `ghost`"),
                "{err}"
            );
        });

        testutil::remove(&repo);
    }

    /// Phase-7 task-3 (E2E, top-level dispatch): `apg plan update` preservation
    /// — a phase title/deliverable update keeps its task `Contains` edges and
    /// replaces `Satisfies`/`Gates` with set-semantics (cycle-refusing); a task
    /// update keeps `status` + incident `Reviews` and re-validates
    /// `--verb`/`--fqn`; a planned update repoints the parent `Contains` edge.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn strict_plan_update_preservation_through_dispatch() {
        let (apg_root, repo, wt) = fixture("dispatch-update");
        write_requirement(&apg_root, "R1");
        write_requirement(&apg_root, "R2");
        // The untracked requirement node files dirty the tree; re-anchor the
        // recorded scan_meta so the update write-through's stale gate passes.
        testutil::write_scan_meta(
            &apg_root,
            Some(&repo.head_sha()),
            false,
            "2026-09-07T00:00:00Z",
        );
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D1".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D2".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-03".into(),
                number: 3,
                title: "P3".into(),
                deliverable: "D3".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
                verb: "modifies".into(),
                target: "github.com/x/y.Store".into(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "requirements.requirement.R1".into(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-02".into(),
                to: "requirements.requirement.R2".into(),
            },
            // phase-01's outgoing gate; phase-03 gates into phase-01.
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-03".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "task issue".into(),
                status: "open".into(),
                disposition: String::new(),
            },
            Record::PlannedNode {
                fqn: "/todo/app.ts".into(),
                kind: "file".into(),
                name: "app.ts".into(),
                parent: "github.com/x/y".into(),
            },
            Record::Contains {
                from: "github.com/x/y".into(),
                to: "/todo/app.ts".into(),
            },
            Record::Reviews {
                from: "foo/feedback-2".into(),
                to: "/todo/app.ts".into(),
            },
        ];
        specs::write_jsonl(&plan_path, &records).unwrap();

        let task_contains = |recs: &[Record]| {
            recs.iter().any(|r| {
                matches!(
                    r,
                    Record::Contains { from, to }
                        if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
                )
            })
        };

        // One cwd hold for the whole surface: `cmd_plan` resolves `apg/` by
        // walking up from cwd, so every command fits under a single
        // `CWD_LOCK` acquisition instead of one per call. The test's own work
        // is sub-second; it was the seven separate re-queues on the
        // process-wide lock that pushed it past libtest's 60s warning.
        with_cwd(&wt, || {
            // A title/deliverable-only phase update: the task Contains edge and both
            // outgoing bridge edges survive.
            cmd_plan(&av(&[
                "update",
                "foo",
                "phase",
                "1",
                "--title",
                "P1b",
                "--deliverable",
                "D1b",
            ]))
            .unwrap();
            let recs = specs::read_jsonl(&plan_path).unwrap();
            assert!(
                recs.iter().any(|r| matches!(
                    r,
                    Record::PlanPhase { fqn, title, deliverable, .. }
                        if fqn == "foo/plan.phase-01" && title == "P1b" && deliverable == "D1b"
                )),
                "phase title/deliverable updated in place"
            );
            assert!(
                task_contains(&recs),
                "task Contains survives a phase update"
            );
            assert!(recs.iter().any(|r| matches!(
                r,
                Record::Satisfies { from, to }
                    if from == "foo/plan.phase-01" && to == "requirements.requirement.R1"
            )));
            assert!(recs.iter().any(|r| matches!(
                r,
                Record::Gates { from, to }
                    if from == "foo/plan.phase-01" && to == "foo/plan.phase-02"
            )));

            // `--satisfies`/`--prereq` replace only phase-01's OWN outgoing sets; the
            // task Contains, the incoming gate, and phase-02's Satisfies survive.
            cmd_plan(&av(&[
                "update",
                "foo",
                "phase",
                "1",
                "--satisfies",
                "R2",
                "--prereq",
                "2",
            ]))
            .unwrap();
            let recs = specs::read_jsonl(&plan_path).unwrap();
            let sat: Vec<&str> = recs
                .iter()
                .filter_map(|r| match r {
                    Record::Satisfies { from, to } if from == "foo/plan.phase-01" => {
                        Some(to.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                sat,
                vec!["requirements.requirement.R2"],
                "the passed --satisfies replaces phase-01's outgoing set"
            );
            let gates: Vec<&str> = recs
                .iter()
                .filter_map(|r| match r {
                    Record::Gates { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                gates,
                vec!["foo/plan.phase-02"],
                "the passed --prereq replaces phase-01's outgoing gate set"
            );
            assert!(recs.iter().any(|r| matches!(
                r,
                Record::Gates { from, to }
                    if from == "foo/plan.phase-03" && to == "foo/plan.phase-01"
            )));
            assert!(recs.iter().any(|r| matches!(
                r,
                Record::Satisfies { from, to }
                    if from == "foo/plan.phase-02" && to == "requirements.requirement.R2"
            )));
            assert!(task_contains(&recs));

            // A cycle-forming gate is refused before any write (phase-02 gating
            // phase-01 closes the phase-01 → phase-02 → phase-01 loop).
            let before = std::fs::read_to_string(&plan_path).unwrap();
            let err = cmd_plan(&av(&["update", "foo", "phase", "2", "--prereq", "1"])).unwrap_err();
            assert!(
                err.to_string().to_lowercase().contains("cycle")
                    || err.to_string().to_lowercase().contains("gate"),
                "{err}"
            );
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), before);

            // A task update (title only) keeps status: done and the Reviews edge.
            cmd_plan(&av(&["update", "foo", "task", "1", "1", "--title", "T2"])).unwrap();
            let recs = specs::read_jsonl(&plan_path).unwrap();
            assert!(
                recs.iter().any(|r| matches!(
                    r,
                    Record::Task { fqn, title, status, .. }
                        if fqn == "foo/plan.phase-01.task-1" && title == "T2" && status == "done"
                )),
                "the done status survives a task update"
            );
            assert!(recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
            )));

            // Re-validation: an invalid verb and a creates-over-real-code are both
            // refused before any write.
            let before = std::fs::read_to_string(&plan_path).unwrap();
            let err = cmd_plan(&av(&[
                "update", "foo", "task", "1", "1", "--verb", "explodes",
            ]))
            .unwrap_err();
            assert!(!err.to_string().is_empty(), "{err}");
            let err = cmd_plan(&av(&[
                "update",
                "foo",
                "task",
                "1",
                "1",
                "--verb",
                "creates",
                "--fqn",
                "github.com/x/y.Store",
            ]))
            .unwrap_err();
            assert!(!err.to_string().is_empty(), "{err}");
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), before);

            // A planned update repoints the parent Contains edge and preserves the
            // unrelated Reviews edge.
            cmd_plan(&av(&[
                "update",
                "foo",
                "planned",
                "/todo/app.ts",
                "--kind",
                "function",
                "--name",
                "app2.ts",
                "--parent",
                "github.com/x/y.Store",
            ]))
            .unwrap();
            let recs = specs::read_jsonl(&plan_path).unwrap();
            assert!(
            recs.iter().any(|r| matches!(
                r,
                Record::PlannedNode { fqn, kind, name, parent }
                    if fqn == "/todo/app.ts" && kind == "function" && name == "app2.ts" && parent == "github.com/x/y.Store"
            )),
            "planned node updated in place"
        );
            let contains: Vec<&str> = recs
                .iter()
                .filter_map(|r| match r {
                    Record::Contains { from, to } if to == "/todo/app.ts" => Some(from.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                contains,
                vec!["github.com/x/y.Store"],
                "exactly the repointed parent Contains edge"
            );
            assert!(recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-2" && to == "/todo/app.ts"
            )));
        });

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn strict_plan_rm_refusal_and_force_cascade_through_dispatch() {
        let (apg_root, repo, wt) = plan_store_fixture("dispatch-rm");
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
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
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "creates".into(),
                target: "github.com/x/y.Gateway".into(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".into(),
                kind: "struct".into(),
                name: "Gateway".into(),
                parent: "github.com/x/y".into(),
            },
            Record::Contains {
                from: "github.com/x/y".into(),
                to: "github.com/x/y.Gateway".into(),
            },
        ];
        specs::write_jsonl(&plan_path, &records).unwrap();
        let before = std::fs::read_to_string(&plan_path).unwrap();

        // One cwd hold for the whole surface: `cmd_plan` resolves `apg/` by
        // walking up from cwd, so the DB-free path fits every command under
        // a single `CWD_LOCK` acquisition instead of one per command.
        with_cwd(&wt, || {
            // A plan with a phase refuses, naming the phase + --force.
            let err = cmd_plan(&av(&["rm", "foo"])).unwrap_err();
            assert!(err.to_string().contains("foo/plan.phase-01"), "{err}");
            assert!(err.to_string().contains("--force"), "{err}");
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), before);

            // A phase with a task refuses, naming the task + --force.
            let err = cmd_plan(&av(&["rm", "foo", "phase", "1"])).unwrap_err();
            assert!(
                err.to_string().contains("foo/plan.phase-01.task-1"),
                "{err}"
            );
            assert!(err.to_string().contains("--force"), "{err}");
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), before);

            // A `done` task refuses, naming the status + --force.
            plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
            let done_before = std::fs::read_to_string(&plan_path).unwrap();
            let err = cmd_plan(&av(&["rm", "foo", "task", "1", "1"])).unwrap_err();
            assert!(err.to_string().contains("done"), "{err}");
            assert!(err.to_string().contains("--force"), "{err}");
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), done_before);

            // A pending task with incident Feedback refuses, naming the feedback.
            plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
            let mut recs = specs::read_jsonl(&plan_path).unwrap();
            recs.push(Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "b".into(),
                status: "open".into(),
                disposition: String::new(),
            });
            recs.push(Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "foo/plan.phase-01.task-1".into(),
            });
            specs::write_jsonl(&plan_path, &recs).unwrap();
            let fb_before = std::fs::read_to_string(&plan_path).unwrap();
            let err = cmd_plan(&av(&["rm", "foo", "task", "1", "1"])).unwrap_err();
            assert!(err.to_string().contains("foo/feedback-1"), "{err}");
            assert!(err.to_string().contains("--force"), "{err}");
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), fb_before);

            // A creates-targeted planned node refuses, naming the task + --force.
            let err =
                cmd_plan(&av(&["rm", "foo", "planned", "github.com/x/y.Gateway"])).unwrap_err();
            assert!(
                err.to_string().contains("foo/plan.phase-01.task-1"),
                "{err}"
            );
            assert!(err.to_string().contains("--force"), "{err}");
            assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), fb_before);

            // The forced planned cascade removes the node + its parent Contains.
            cmd_plan(&av(&[
                "rm",
                "foo",
                "planned",
                "github.com/x/y.Gateway",
                "--force",
            ]))
            .unwrap();
            let recs = specs::read_jsonl(&plan_path).unwrap();
            assert!(
                !recs.iter().any(|r| matches!(r, Record::PlannedNode { .. })),
                "the planned node is gone"
            );
            assert_no_orphans(&recs);

            // The forced task cascade removes the task; its Feedback record
            // and its Reviews reference survive deliberately (a review item is
            // closed by a reviewer, never dropped by target removal).
            cmd_plan(&av(&["rm", "foo", "task", "1", "1", "--force"])).unwrap();
            let recs = specs::read_jsonl(&plan_path).unwrap();
            assert!(!recs.iter().any(|r| matches!(r, Record::Task { .. })));
            assert!(
                recs.iter()
                    .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-1")),
                "the task's feedback survives its target's removal"
            );
            assert!(
                recs.iter().any(|r| matches!(r, Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1")),
                "the retained feedback keeps its Reviews reference"
            );
            assert_no_orphans(&recs);

            // The phase now has no tasks: a plain (no --force) rm removes it.
            cmd_plan(&av(&["rm", "foo", "phase", "1"])).unwrap();
            assert!(
                !specs::read_jsonl(&plan_path)
                    .unwrap()
                    .iter()
                    .any(|r| matches!(r, Record::PlanPhase { .. }))
            );

            // `rm foo --force` cascades every plan-family record but leaves the
            // store in place while the retained Feedback survives — a store
            // with surviving feedback must not be deleted.
            cmd_plan(&av(&["rm", "foo", "--force"])).unwrap();
            assert!(
                plan_path.exists(),
                "a store with surviving feedback must not be deleted"
            );
            let recs = specs::read_jsonl(&plan_path).unwrap();
            assert!(
                recs.iter()
                    .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-1")),
                "the task's feedback survives the plan cascade"
            );
            assert!(
                recs.iter().any(|r| matches!(r, Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1")),
                "the retained feedback keeps its Reviews reference"
            );
            assert_no_orphans(&recs);
        });

        testutil::remove(&repo);
    }

    /// Phase-7 task-4 (E2E, top-level dispatch): a mid-cascade failure during
    /// `apg plan rm --force` leaves the on-disk plan file byte-identical — the
    /// whole-record rewrite is atomic, never a half-deleted plan.
    #[test]
    #[ignore = "e2e tier: real I/O (plan store/node files/db.lbug/git/process); run via cargo test-e2e"]
    fn strict_plan_rm_mid_cascade_error_leaves_store_intact_through_dispatch() {
        let (apg_root, repo, wt) = fixture("dispatch-rm-atomic");
        let plan_path = write_plan(&apg_root);
        let before = std::fs::read(&plan_path).unwrap();

        // Force the single write-through to fail: record a scan_meta that does
        // not match the live git state, so the stale gate refuses the re-ingest
        // after the whole cascade has been computed in memory.
        testutil::write_scan_meta(
            &apg_root,
            Some("0000000000000000000000000000000000000000"),
            true,
            "2026-09-07T00:00:00Z",
        );

        let err = with_cwd(&wt, || {
            cmd_plan(&av(&["rm", "foo", "--force"])).unwrap_err()
        });
        assert!(err.to_string().contains("stale"), "{err}");
        assert_eq!(
            std::fs::read(&plan_path).unwrap(),
            before,
            "a mid-cascade failure must leave the plan store byte-identical"
        );

        testutil::remove(&repo);
    }
}
