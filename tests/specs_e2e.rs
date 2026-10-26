mod common;

use apg::layers::Layer;
use apg::schema::Record;
use apg::specs::*;
use std::path::{Path, PathBuf};

fn plans_jsonl(dir: &Path, project: &str) -> PathBuf {
    dir.join(TRANS)
        .join("plans")
        .join(format!("{project}.jsonl"))
}

/// e2e tier -- real I/O: both tests stage temp dirs and discover/read/write
/// the `.trans` mirror paths on disk. Each is `#[ignore]`d, so a plain
/// `cargo test` never runs one; the only entry point is the named guard
/// `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/.trans mirrors); run via cargo test-e2e"]
    fn jsonl_roundtrip_and_plan_files() {
        let dir = std::env::temp_dir().join(format!("apg-specs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join(TRANS).join("plans");
        std::fs::create_dir_all(&plans).unwrap();

        // The task's own record is title+status alone; the Agent it is
        // assigned to, the Component it implements and the code it touches
        // are the SCHEMA.md task edges written beside it — one full per-kind
        // operation family, each renames/moves destination on `target`.
        let task = "foo/plan.phase-01.task-1".to_string();
        let recs = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Plan".to_string(),
                strategy: "Layer-first".to_string(),
            },
            Record::Task {
                fqn: task.clone(),
                title: "T".to_string(),
                status: "pending".to_string(),
            },
            Record::AssignedTo {
                from: task.clone(),
                to: "agent.go-frontend-implementer".to_string(),
            },
            Record::Implements {
                from: task.clone(),
                to: "solution.component.plan-reader".to_string(),
            },
            // Module operations.
            Record::ModuleCreate {
                from: task.clone(),
                to: "foo.new".to_string(),
            },
            Record::ModuleModify {
                from: task.clone(),
                to: "foo.live".to_string(),
            },
            Record::ModuleDelete {
                from: task.clone(),
                to: "foo.gone".to_string(),
            },
            Record::ModuleMove {
                from: task.clone(),
                to: "foo.old".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "foo.moved".to_string(),
            },
            Record::ModuleRename {
                from: task.clone(),
                to: "foo.renamable".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "foo.renamed".to_string(),
            },
            // File operations.
            Record::FileCreate {
                from: task.clone(),
                to: "src/new.rs".to_string(),
            },
            Record::FileModify {
                from: task.clone(),
                to: "src/live.rs".to_string(),
            },
            Record::FileDelete {
                from: task.clone(),
                to: "src/gone.rs".to_string(),
            },
            Record::FileMove {
                from: task.clone(),
                to: "src/old.rs".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "src/moved.rs".to_string(),
            },
            Record::FileRename {
                from: task.clone(),
                to: "src/renamable.rs".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "src/renamed.rs".to_string(),
            },
            // Struct operations.
            Record::StructCreate {
                from: task.clone(),
                to: "foo.new.Widget".to_string(),
            },
            Record::StructModify {
                from: task.clone(),
                to: "foo.live.Store".to_string(),
            },
            Record::StructDelete {
                from: task.clone(),
                to: "foo.gone.Legacy".to_string(),
            },
            Record::StructMove {
                from: task.clone(),
                to: "foo.old.Widget".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "foo.moved.Widget".to_string(),
            },
            Record::StructRename {
                from: task.clone(),
                to: "foo.renamable.Widget".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "foo.renamed.Widget".to_string(),
            },
            // Function operations.
            Record::FunctionCreate {
                from: task.clone(),
                to: "foo.new.Widget.build".to_string(),
            },
            Record::FunctionModify {
                from: task.clone(),
                to: "foo.live.Store.get".to_string(),
            },
            Record::FunctionDelete {
                from: task.clone(),
                to: "foo.gone.Legacy.drop".to_string(),
            },
            Record::FunctionMove {
                from: task.clone(),
                to: "foo.old.Widget.build".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "foo.moved.Widget.build".to_string(),
            },
            Record::FunctionRename {
                from: task.clone(),
                to: "foo.renamable.Widget.build".to_string(),
            },
            Record::Target {
                from: task.clone(),
                to: "foo.renamed.Widget.build".to_string(),
            },
        ];
        write_jsonl(&plans.join("foo.jsonl"), &recs).unwrap();

        // plan_files discovers only the transient plan leg.
        let p = plan_files(&dir);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0], plans.join("foo.jsonl"));
        // Round-trip: re-serialize the parsed records identically.
        let parsed = read_jsonl(&plans.join("foo.jsonl")).unwrap();
        assert_eq!(parsed, recs);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/.trans mirrors); run via cargo test-e2e"]
    fn feedback_mirrors_discovered_and_paths_are_per_tier_per_project() {
        let dir = std::env::temp_dir().join(format!("apg-specs-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(TRANS).join("plans")).unwrap();
        std::fs::create_dir_all(dir.join(TRANS).join("requirements")).unwrap();
        std::fs::create_dir_all(dir.join(TRANS).join("implementation")).unwrap();
        std::fs::create_dir_all(dir.join(TRANS).join("global")).unwrap();

        // Feedback mirrors are per-project JSONLs in the tier dir of the
        // attached node (SPEC §5); a second project shares the dirs.
        write_jsonl(
            &dir.join(TRANS).join("requirements").join("foo.jsonl"),
            &[Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            }],
        )
        .unwrap();
        write_jsonl(
            &dir.join(TRANS).join("implementation").join("foo.jsonl"),
            &[Record::Reviews {
                from: "foo/feedback-2".to_string(),
                to: "fixture.mod.Store".to_string(),
            }],
        )
        .unwrap();
        write_jsonl(
            &dir.join(TRANS).join("global").join("bar.jsonl"),
            &[Record::Feedback {
                fqn: "bar/feedback-1".to_string(),
                body: "y".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            }],
        )
        .unwrap();

        // trans_mirror_files finds every tier mirror, never the plans store.
        let mirrors = trans_mirror_files(&dir);
        let got: Vec<String> = mirrors
            .iter()
            .map(|p| {
                p.strip_prefix(dir.join(TRANS))
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect();
        assert_eq!(
            got,
            vec![
                "global/bar.jsonl",
                "implementation/foo.jsonl",
                "requirements/foo.jsonl"
            ]
        );

        // transient_feedback_path: plans tier == the plan store, the other
        // tiers mirror under their layer dir.
        assert_eq!(
            transient_feedback_path(&dir, "foo", Layer::Plans),
            plans_jsonl(&dir, "foo")
        );
        assert_eq!(
            transient_feedback_path(&dir, "foo", Layer::Requirements),
            dir.join(TRANS).join("requirements").join("foo.jsonl")
        );

        // project_transient_files: the plan store + the five tier mirrors for
        // one project, in layer order.
        let files = project_transient_files(&dir, "foo");
        assert_eq!(files.len(), 6);
        assert_eq!(files[0], plans_jsonl(&dir, "foo"));
        assert_eq!(
            files[1],
            dir.join(TRANS).join("requirements").join("foo.jsonl")
        );
        assert_eq!(files[2], dir.join(TRANS).join("domain").join("foo.jsonl"));
        assert_eq!(files[3], dir.join(TRANS).join("solution").join("foo.jsonl"));
        assert_eq!(
            files[4],
            dir.join(TRANS).join("implementation").join("foo.jsonl")
        );
        assert_eq!(files[5], dir.join(TRANS).join("global").join("foo.jsonl"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
