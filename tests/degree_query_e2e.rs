//! e2e tier — real I/O: builds a scratch `/tmp` git repo, scans a real
//! `db.lbug` into a project worktree, and spawns the candidate `apg` binary as
//! a subprocess for each query.
//!
//! Regression pin for the LadybugDB degree-optimizer defect fixed in v0.21.0:
//! a top-N grouped `Contains` query whose destination node is unlabelled and
//! whose grouping key is a STRING primary key (`File.fqn`) was rewritten into a
//! degree-table plan that wrote raw storage offsets as the group key, and the
//! 0.19.1-linked engine segfaulted (exit 139, empty stdout). The labelled
//! destination is the control proving the unlabelled row set is not an
//! artefact of the fixture. Both run only under `cargo test-e2e`; a plain
//! `cargo test` never reaches them.

mod common;

use common::*;
use std::os::unix::process::ExitStatusExt;

mod e2e {
    use super::*;

    /// The candidate binary runs both query shapes against its own scanned
    /// database: exit must be successful and each must return at least one
    /// row. A child killed by a signal is reported by its signal — never a
    /// hang and never silently conflated with a clean non-zero exit.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/scanned db.lbug/spawned apg); run via cargo test-e2e"]
    fn topn_grouped_contains_query_survives_string_primary_key() {
        let (repo, wt, _wt_apg) = project_with_db("degree-query");

        // Give the fixture a second File of real cardinality (the stock seed
        // is one File with one Struct), so the grouped count has a non-trivial
        // ORDER BY to drive.
        let extra = [
            module_line("fixture.extra"),
            struct_line("n11", "fixture.extra", "Alpha", "/abs/extra.go"),
            struct_line("n12", "fixture.extra", "Beta", "/abs/extra.go"),
            struct_line("n13", "fixture.extra", "Gamma", "/abs/extra.go"),
            function_line("n14", "fixture.extra", "run", "/abs/extra.go"),
            file_line("/abs/extra.go", "fixture.extra", 80),
        ]
        .join("\n");
        std::fs::write(wt.join("code/extra.scan.jsonl"), format!("{extra}\n")).unwrap();
        wt_commit(&wt, &["code/extra.scan.jsonl"], "extra code");
        scan_checkout(&wt).unwrap();

        let assert_rows = |label: &str, query: &str| {
            let out = ApgCommand::new(&["query", "--json", query])
                .cwd(&wt)
                .output();
            assert!(
                out.status.success(),
                "{label}: candidate `apg query` failed (exit code {:?}, signal {:?}) — \
                 the 0.19.1 defect signature is exit 139 with empty stdout; stderr: {}",
                out.status.code(),
                out.status.signal(),
                String::from_utf8_lossy(&out.stderr),
            );
            let parsed: serde_json::Value =
                serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
                    panic!(
                        "{label}: `--json` output is not JSON ({e}): {}",
                        String::from_utf8_lossy(&out.stdout)
                    )
                });
            let rows = parsed
                .as_array()
                .unwrap_or_else(|| panic!("{label}: `--json` did not emit an array: {parsed}"));
            assert!(
                !rows.is_empty(),
                "{label}: expected at least one row, got none; stdout: {}",
                String::from_utf8_lossy(&out.stdout),
            );
        };

        // The regression shape: unlabelled destination grouped by the File's
        // STRING primary key, ordered by the aggregate with a top-N LIMIT.
        assert_rows(
            "unlabelled top-N grouped Contains",
            "MATCH (f:File)-[:Contains]->(c) RETURN f.fqn AS parent, count(c) AS children ORDER BY children DESC LIMIT 12",
        );

        // The control: the same shape with a labelled destination.
        assert_rows(
            "labelled-destination top-N grouped Contains",
            "MATCH (f:File)-[:Contains]->(s:Struct) RETURN f.fqn AS parent, count(s) AS children ORDER BY children DESC LIMIT 12",
        );

        remove(&repo);
    }
}
