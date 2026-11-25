//! End-to-end tests for the `agentfrontend` binary: every case drives the built
//! binary over its own scratch directory under the platform temp dir, so each
//! test performs real filesystem I/O and carries `#[ignore]`, run via
//! `(cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch checkout under the platform temp dir; `outside/` is a sibling of
/// `checkout/`, so it is outside the frontend's claimed root.
struct Scratch {
    base: PathBuf,
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "agentlib-e2e-{}-{tag}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("checkout");
        std::fs::create_dir_all(&root).expect("create scratch checkout");
        Scratch { base, root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        write_file(&self.path(rel), contents)
    }

    fn write_outside(&self, rel: &str, contents: &str) -> PathBuf {
        write_file(&self.base.join("outside").join(rel), contents)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn write_file(path: &Path, contents: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create scratch parent");
    }
    std::fs::write(path, contents).expect("write scratch file");
    path.to_path_buf()
}

fn run(root: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentfrontend"));
    cmd.arg(root);
    cmd.args(args);
    cmd.output().expect("spawn agentfrontend")
}

fn records(output: &Output) -> Vec<Value> {
    assert!(
        output.status.success(),
        "agentfrontend failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .expect("utf-8 stream")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSONL record"))
        .collect()
}

fn typed<'a>(records: &'a [Value], ty: &str) -> Vec<&'a Value> {
    records.iter().filter(|r| r["type"] == ty).collect()
}

fn assert_declined(output: &Output) {
    assert!(
        records(output).is_empty(),
        "a declined run must emit no records"
    );
}

/// Two `*.md` definitions plus a non-`*.md` direct child (the claim is
/// location-based, not extension-based) in `.opencode/agents/`, with three
/// non-definitions around them: a nested file, a non-`*.md` file outside the
/// claimed directory and a `docs/` sibling subtree. `sub/` is a real directory
/// so a `sub/..` module value can traverse back to the checkout root.
fn module_fixture(tag: &str) -> Scratch {
    let scratch = Scratch::new(tag);
    scratch.write(".opencode/agents/first.md", "# first\n");
    scratch.write(".opencode/agents/second.md", "# second\n# body\n");
    scratch.write(".opencode/agents/notes.txt", "not markdown\n");
    scratch.write(".opencode/agents/nested/deep.md", "# nested\n");
    scratch.write(".opencode/README.txt", "not a definition\n");
    scratch.write("docs/agent.md", "# elsewhere\n");
    scratch.write("sub/records.txt", "real child\n");
    scratch
}

/// The identity set of a record stream — `module` by FQN, located records by
/// `path` (an `agent` by its stem too), sorted — so two runs selecting the same
/// definitions compare equal whatever `--module` form selected them. The agent
/// stream carries repo-relative paths only.
fn identities(recs: &[Value]) -> Vec<String> {
    let mut ids: Vec<String> = recs
        .iter()
        .map(|rec| {
            let ty = rec["type"].as_str().expect("record type");
            match ty {
                "module" => format!("module:{}", rec["fqn"].as_str().expect("module fqn")),
                "agent" => format!(
                    "agent:{}:{}",
                    rec["name"].as_str().expect("agent name"),
                    rec["path"].as_str().expect("agent path")
                ),
                _ => format!(
                    "{ty}:{}",
                    rec["path"].as_str().expect("located record path")
                ),
            }
        })
        .collect();
    ids.sort();
    ids
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn definitions_emit_module_file_and_agent_records() {
        let scratch = Scratch::new("happy");
        scratch.write(
            ".opencode/agents/go-frontend-implementer.md",
            "# go\n# role\n",
        );
        scratch.write(
            ".opencode/agents/plan-writer.md",
            "# plan\n# writer\n# body\n",
        );
        // Paths outside the claimed directory: never a definition.
        scratch.write(".opencode/README.md", "# agents docs\n");
        scratch.write(".opencode/agents.md", "# not a definition\n");
        scratch.write(".opencode/agents/nested/deep.md", "# nested\n");
        scratch.write("docs/agent.md", "# elsewhere\n");

        let recs = records(&run(&scratch.root, &[]));

        let modules = typed(&recs, "module");
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["fqn"], ".opencode/agents");

        let files = typed(&recs, "file");
        let agents = typed(&recs, "agent");
        assert_eq!(files.len(), 2, "one file record per definition");
        assert_eq!(agents.len(), 2, "one agent record per definition");
        assert_eq!(recs.len(), 1 + 2 * 2);

        let expected = [
            (
                "go-frontend-implementer",
                ".opencode/agents/go-frontend-implementer.md",
                2,
            ),
            ("plan-writer", ".opencode/agents/plan-writer.md", 3),
        ];
        for (stem, path, end_line) in expected {
            let file = files
                .iter()
                .find(|r| r["path"] == path)
                .expect("file record per definition");
            assert_eq!(file["parent"], ".opencode/agents");
            assert_eq!(file["start_line"].as_u64(), Some(1));
            assert_eq!(file["end_line"].as_u64(), Some(end_line));

            let agent = agents
                .iter()
                .find(|r| r["path"] == path)
                .expect("agent record per definition");
            assert_eq!(agent["name"], stem);
            assert_eq!(agent["start_line"].as_u64(), Some(1));
            assert_eq!(agent["end_line"].as_u64(), Some(end_line));
        }

        // Every located record is a direct child of the claimed directory.
        for rec in recs.iter().filter(|r| r["type"] != "module") {
            let path = rec["path"].as_str().expect("located record carries a path");
            let name = path
                .strip_prefix(".opencode/agents/")
                .unwrap_or_else(|| panic!("record outside the claim scope: {rec}"));
            assert!(!name.contains('/'), "nested record: {rec}");
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn missing_agents_directory_emits_nothing_and_succeeds() {
        let scratch = Scratch::new("missing");
        scratch.write("main.go", "package main\n");
        assert_declined(&run(&scratch.root, &[]));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn agents_directory_symlink_to_outside_is_declined() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("dir-symlink");
        scratch.write_outside("definitions/outside.md", "# outside\n");
        std::fs::create_dir_all(scratch.path(".opencode")).expect("create .opencode");
        symlink(
            scratch.base.join("outside/definitions"),
            scratch.path(".opencode/agents"),
        )
        .expect("symlink agents directory");

        assert_declined(&run(&scratch.root, &[]));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn opencode_ancestor_symlink_to_outside_is_declined() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("ancestor-symlink");
        scratch.write_outside("opencode/agents/outside.md", "# outside\n");
        symlink(
            scratch.base.join("outside/opencode"),
            scratch.path(".opencode"),
        )
        .expect("symlink .opencode");

        assert_declined(&run(&scratch.root, &[]));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn agents_path_that_is_a_regular_file_is_declined() {
        let scratch = Scratch::new("regular-file");
        scratch.write(".opencode/agents", "not a directory\n");
        assert_declined(&run(&scratch.root, &[]));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn child_symlink_is_skipped() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("child-symlink");
        scratch.write(".opencode/agents/inside.md", "# inside\n");
        let outside = scratch.write_outside("outside.md", "# outside\n");
        symlink(&outside, scratch.path(".opencode/agents/outside.md"))
            .expect("symlink child definition");

        let recs = records(&run(&scratch.root, &[]));
        assert_eq!(typed(&recs, "module").len(), 1);
        assert_eq!(typed(&recs, "file").len(), 1);
        let agents = typed(&recs, "agent");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "inside");
        assert!(
            recs.iter()
                .all(|r| r["path"] != ".opencode/agents/outside.md"),
            "the symlink child was claimed: {recs:?}"
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn extensionless_definition_keeps_its_whole_name() {
        let scratch = Scratch::new("extensionless");
        scratch.write(".opencode/agents/extension-less", "line one\nline two\n");

        let recs = records(&run(&scratch.root, &[]));
        assert_eq!(typed(&recs, "module").len(), 1);
        assert_eq!(typed(&recs, "file").len(), 1);
        let agents = typed(&recs, "agent");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "extension-less");
        assert_eq!(agents[0]["path"], ".opencode/agents/extension-less");
        assert_eq!(agents[0]["start_line"].as_u64(), Some(1));
        assert_eq!(agents[0]["end_line"].as_u64(), Some(2));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn exclude_path_glob_drops_one_definition() {
        let scratch = Scratch::new("exclude");
        scratch.write(".opencode/agents/keep.md", "# keep\n");
        scratch.write(".opencode/agents/skip.draft", "# skip\n");

        let recs = records(&run(&scratch.root, &["--exclude-path", "*.draft"]));
        assert_eq!(typed(&recs, "module").len(), 1);
        assert_eq!(typed(&recs, "file").len(), 1);
        let agents = typed(&recs, "agent");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["path"], ".opencode/agents/keep.md");
        assert!(
            recs.iter()
                .all(|r| r["path"] != ".opencode/agents/skip.draft"),
            "the excluded definition was claimed: {recs:?}"
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn targets_list_keeps_only_the_listed_definition() {
        let scratch = Scratch::new("targets");
        let keep = scratch.write(".opencode/agents/keep.md", "# keep\n");
        scratch.write(".opencode/agents/other.md", "# other\n");
        let targets = scratch.write("targets.txt", &format!("{}\n", keep.display()));

        let recs = records(&run(
            &scratch.root,
            &["--targets", targets.to_str().expect("utf-8 targets path")],
        ));
        assert_eq!(typed(&recs, "module").len(), 1);
        assert_eq!(typed(&recs, "file").len(), 1);
        let agents = typed(&recs, "agent");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "keep");
        assert_eq!(agents[0]["path"], ".opencode/agents/keep.md");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn module_relative_and_absolute_forms_select_the_same_definitions() {
        let scratch = module_fixture("module-forms");
        let relative = records(&run(&scratch.root, &["--module", ".opencode/agents"]));
        let absolute_path = scratch.path(".opencode/agents");
        let absolute = records(&run(
            &scratch.root,
            &[
                "--module",
                absolute_path.to_str().expect("utf-8 module path"),
            ],
        ));

        assert_eq!(
            typed(&relative, "agent").len(),
            3,
            "the selection must admit every definition: {relative:?}"
        );
        assert_eq!(identities(&relative), identities(&absolute));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn module_dotdot_value_selects_the_same_subtree() {
        let scratch = module_fixture("module-dotdot");
        let plain = records(&run(&scratch.root, &["--module", "."]));
        let dotdot = records(&run(&scratch.root, &["--module", "sub/.."]));

        assert_eq!(
            typed(&plain, "agent").len(),
            3,
            "the root selection must admit every definition: {plain:?}"
        );
        assert_eq!(identities(&plain), identities(&dotdot));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn module_symlinked_value_selects_the_same_subtree() {
        use std::os::unix::fs::symlink;

        let scratch = module_fixture("module-symlink");
        let target = scratch.path(".opencode/agents");
        let link = scratch.path("linked-agents");
        if symlink(&target, &link).is_err() {
            // The platform denied symlink creation: nothing to compare.
            return;
        }

        let plain = records(&run(&scratch.root, &["--module", ".opencode/agents"]));
        let linked = records(&run(&scratch.root, &["--module", "linked-agents"]));

        assert_eq!(
            typed(&plain, "agent").len(),
            3,
            "the selection must admit every definition: {plain:?}"
        );
        assert_eq!(identities(&plain), identities(&linked));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn module_outside_the_agents_subtree_emits_nothing() {
        let scratch = module_fixture("module-outside");
        let recs = records(&run(&scratch.root, &["--module", "docs"]));

        assert!(
            recs.is_empty(),
            "a subtree outside .opencode/agents admitted records: {recs:?}"
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem I/O; run via (cd src/agentlib && cargo test e2e:: -- --ignored --test-threads=4)"]
    fn flagless_run_emits_every_definition() {
        let scratch = module_fixture("module-flagless");
        let recs = records(&run(&scratch.root, &[]));

        assert_eq!(typed(&recs, "module").len(), 1);
        let mut files: Vec<&str> = typed(&recs, "file")
            .iter()
            .map(|r| r["path"].as_str().expect("file path"))
            .collect();
        files.sort();
        assert_eq!(
            files,
            [
                ".opencode/agents/first.md",
                ".opencode/agents/notes.txt",
                ".opencode/agents/second.md",
            ],
            "only the direct definition children are emitted"
        );
        let mut names: Vec<&str> = typed(&recs, "agent")
            .iter()
            .map(|r| r["name"].as_str().expect("agent name"))
            .collect();
        names.sort();
        assert_eq!(names, ["first", "notes", "second"]);
    }
}
