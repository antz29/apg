//! e2e proof of the `apg scan --module` contract (module-flag-resolution
//! phase-01 task-3), driven only through the CANDIDATE binary
//! (`testutil::apg_bin`) against scratch `/tmp` repos — never the apg repo.
//!
//! Two things are proven: a `--module` value naming a missing directory or a
//! non-directory fails the scan loudly and writes no graph; and on one unchanged
//! tree the scan's recorded module selection drives both the freshness verdict
//! and the admitted subtree in both directions (whole-tree-recorded →
//! subtree-requested, subtree-recorded → whole-tree-requested) plus subtree →
//! different subtree, while re-requesting the same recorded selection still
//! reuses the recorded graph.
//!
//! The fixture's sources are Markdown so the hermetic bundled structural
//! frontend serves the scan — `--language md` selects it, and the structural
//! streams need no external toolchain.

mod common;

use common::{ApgCommand, Repo, remove};
use std::path::PathBuf;
use std::process::Output;

/// A committed scratch tree with two sibling Markdown subtrees plus a
/// root-level file. `Repo::new` already commits `.gitignore` and
/// `apg/config.json` at the binary's version, so no `apg init` is needed.
fn module_repo(tag: &str) -> Repo {
    let repo = Repo::new(tag);
    repo.write("A/one.md", "# one\n");
    repo.write("B/two.md", "# two\n");
    repo.write("README.md", "# root\n");
    repo.commit_all("fixture");
    repo
}

/// Runs `apg scan . --language md [args]` in `repo` and returns the output plus
/// its stderr text (the scan log: the staleness verdict, `[scan]` decisions and
/// the per-frontend lines).
fn scan(repo: &Repo, args: &[&str]) -> (Output, String) {
    let mut full = vec!["scan", ".", "--language", "md"];
    full.extend_from_slice(args);
    let out = ApgCommand::new(&full).cwd(&repo.root).output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    (out, log)
}

/// The scratch repo's derived database path.
fn db_path(repo: &Repo) -> PathBuf {
    repo.root.join("apg/.trans/db.lbug")
}

/// The admitted Markdown File FQNs, sorted. The structural streams also claim
/// the tracked `.gitignore` and `apg/config.json`, so the admission assertion
/// is scoped to the fixture's own `.md` files.
fn md_files(repo: &Repo) -> Vec<String> {
    let cypher = "MATCH (f:File) WHERE f.fqn ENDS WITH '.md' RETURN f.fqn";
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(&repo.root)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).expect("json rows");
    let mut files: Vec<String> = rows
        .iter()
        .map(|r| r["f.fqn"].as_str().unwrap_or_default().to_string())
        .collect();
    files.sort();
    files
}

/// Asserts the graph admits exactly `expected` Markdown File FQNs on `leg`.
fn assert_admits(repo: &Repo, leg: &str, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        md_files(repo),
        expected,
        "{leg}: the admitted Markdown subtree"
    );
}

mod e2e {
    use super::*;

    /// The whole tree's Markdown, in the sorted order the query returns.
    const WHOLE: [&str; 3] = ["A/one.md", "B/two.md", "README.md"];

    /// A `--module` value that names a missing directory, and one that names a
    /// file rather than a directory, each fail the scan with the named error
    /// naming the value — before any frontend spawns, so no graph is written.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo under /tmp, spawned apg); run via cargo test-e2e"]
    fn missing_or_non_directory_module_value_fails_loud_without_a_graph() {
        let repo = module_repo("module-flag-loud");

        for value in ["missing", "README.md"] {
            let (out, log) = scan(&repo, &["--module", value]);
            assert!(
                !out.status.success(),
                "`--module {value}` must fail the scan: {log}"
            );
            assert!(
                log.contains(&format!("`--module {value}` does not name a directory")),
                "the error must name the offending value `{value}`: {log}"
            );
            assert!(
                log.contains("is missing or not a directory"),
                "the error must say what is wrong: {log}"
            );
            assert!(
                !db_path(&repo).exists(),
                "a refused `--module {value}` must leave no graph written"
            );
        }

        remove(&repo);
    }

    /// One unchanged scratch tree: a flagless whole-tree scan, a `--module A`
    /// re-scan, a `--module B` re-scan and a final flagless re-scan prove both
    /// freshness directions (whole→subtree and subtree→whole) plus subtree→
    /// subtree, each rebuilding only because the requested module set differs
    /// from the recorded one; a re-request of the recorded set still reuses.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo under /tmp, spawned apg); run via cargo test-e2e"]
    fn recorded_module_selection_drives_freshness_and_admission() {
        let repo = module_repo("module-flag-selection");

        // (1) Cold whole-tree scan: records the empty (whole-tree) selection
        // and admits every Markdown subtree.
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "cold whole-tree scan: {log}");
        assert!(
            log.contains("Git state: no scan yet"),
            "the cold scan must report no prior scan: {log}"
        );
        assert_admits(&repo, "cold whole tree", &WHOLE);

        // (2) WHOLE-TREE-RECORDED → SUBTREE-REQUESTED: the requested `A`
        // differs from the recorded whole-tree selection, so the verdict is
        // STALE on `modules`, the graph is rebuilt (never the fast path), and
        // only A's subtree is admitted.
        let (out, log) = scan(&repo, &["--module", "A"]);
        assert!(out.status.success(), "`--module A` scan: {log}");
        assert!(
            log.contains("→ STALE (differing: modules)"),
            "a subtree requested against a whole-tree record must be STALE on `modules`: {log}"
        );
        assert!(
            !log.contains("fast-path"),
            "a differing module set must rebuild, never reuse: {log}"
        );
        assert_admits(&repo, "module A", &["A/one.md"]);

        // (3) SAME SET RE-REQUESTED: A is now the recorded selection, so the
        // unchanged re-scan reads FRESH and reuses the recorded A-only graph.
        let (out, log) = scan(&repo, &["--module", "A"]);
        assert!(out.status.success(), "`--module A` re-scan: {log}");
        assert!(
            log.contains("→ FRESH"),
            "the recorded module set re-requested must read FRESH: {log}"
        );
        assert!(
            log.contains("fast-path"),
            "the same module set must reuse the recorded graph: {log}"
        );
        assert_admits(&repo, "module A reuse", &["A/one.md"]);

        // (4) SUBTREE-RECORDED → DIFFERENT SUBTREE: B differs from the recorded
        // A, so the same rebuild rule holds and only B's subtree is admitted.
        let (out, log) = scan(&repo, &["--module", "B"]);
        assert!(out.status.success(), "`--module B` scan: {log}");
        assert!(
            log.contains("→ STALE (differing: modules)"),
            "a different subtree must be STALE on `modules`: {log}"
        );
        assert!(
            !log.contains("fast-path"),
            "a different module set must rebuild, never reuse: {log}"
        );
        assert_admits(&repo, "module B", &["B/two.md"]);

        // (5) SUBTREE-RECORDED → WHOLE-TREE-REQUESTED: the flagless empty
        // request differs from B's record, so it is STALE on `modules` again and
        // the rebuild restores every subtree.
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "whole-tree re-scan: {log}");
        assert!(
            log.contains("→ STALE (differing: modules)"),
            "a whole-tree request against a subtree record must be STALE on `modules`: {log}"
        );
        assert!(
            !log.contains("fast-path"),
            "a differing module set must rebuild, never reuse: {log}"
        );
        assert_admits(&repo, "whole tree again", &WHOLE);

        // (6) The whole-tree selection is recorded again: an unchanged re-scan
        // is FRESH and reuses it.
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "whole-tree reuse scan: {log}");
        assert!(
            log.contains("→ FRESH") && log.contains("fast-path"),
            "the same whole-tree selection must reuse the recorded graph: {log}"
        );
        assert_admits(&repo, "whole tree reuse", &WHOLE);

        remove(&repo);
    }
}
