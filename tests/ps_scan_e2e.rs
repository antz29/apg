//! Phase-07 task-20 (e2e): `apg scan` over a PowerShell scratch repo, driven
//! through the CANDIDATE binary (`testutil::apg_bin`) — never the apg repo.
//! Only the File/Module facts are asserted, so the tests stay valid as the
//! declaration/edge phases land. Every test is `#[ignore]`d and runs through
//! `cargo test-e2e`.

mod common;

use common::{ApgCommand, Repo};
use std::process::Output;

/// The four PowerShell Files the fixture carries.
const PS_FILES: [&str; 4] = [
    "build.ps1",
    "LEGACY.PS1",
    "sub/Util.psm1",
    "data/Settings.psd1",
];

/// A scratch repo with root scripts (`build.ps1`, upper-case `LEGACY.PS1`), a
/// script module in a subdirectory (`sub/Util.psm1`), a plain-hashtable data
/// file that is never a manifest (`data/Settings.psd1`), and a Markdown file
/// for the structural stream. No dot-source or `Import-Module`.
fn ps_repo(tag: &str) -> Repo {
    let repo = Repo::new(tag);
    repo.write("build.ps1", "function Invoke-Build {\n    'build'\n}\n");
    repo.write("LEGACY.PS1", "function Get-Legacy {\n    'legacy'\n}\n");
    repo.write("sub/Util.psm1", "function Get-Util {\n    'util'\n}\n");
    repo.write("data/Settings.psd1", "@{ Name = 'x' }\n");
    repo.write("README.md", "# scratch\n");
    repo.commit_all("fixture");
    repo
}

/// Runs `apg scan .` with `args` in `repo` and returns the output plus its
/// stderr text (the scan log: `Languages:` line, `[scan]` decisions).
fn scan(repo: &Repo, args: &[&str]) -> (Output, String) {
    let mut full = vec!["scan", "."];
    full.extend_from_slice(args);
    let out = ApgCommand::new(&full).cwd(&repo.root).output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    (out, log)
}

/// The `Languages:` list the scan logged.
fn languages(log: &str) -> Vec<String> {
    log.lines()
        .find_map(|l| l.strip_prefix("Languages: "))
        .map(|l| l.split(", ").map(str::to_string).collect())
        .unwrap_or_default()
}

/// Runs a read-only Cypher through the candidate `apg query --json` and
/// returns the rows as `(col_a, col_b)` string pairs.
fn query_rows(repo: &Repo, cypher: &str, a: &str, b: &str) -> Vec<(String, String)> {
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(&repo.root)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).expect("json rows");
    let cell = |r: &serde_json::Value, c: &str| r[c].as_str().unwrap_or_default().to_string();
    rows.iter().map(|r| (cell(r, a), cell(r, b))).collect()
}

/// The `(module, file)` Contains pairs of the scanned graph.
fn module_files(repo: &Repo) -> Vec<(String, String)> {
    query_rows(
        repo,
        "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn, f.fqn",
        "m.fqn",
        "f.fqn",
    )
}

mod e2e {
    use super::*;

    /// `--language ps` selects the PowerShell frontend: every PowerShell File
    /// exists, root scripts sit in `ps.`, the subdirectory module is a
    /// non-root `ps.` module, and the `ps` Language contains `ps.`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn language_flag_selects_ps_and_renders_file_modules() {
        let repo = ps_repo("ps-lang");
        let (out, log) = scan(&repo, &["--language", "ps"]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&"ps".to_string()),
            "Languages line must list ps: {log}"
        );

        let pairs = module_files(&repo);
        let files: Vec<&String> = pairs.iter().map(|(_, f)| f).collect();
        for f in PS_FILES {
            assert!(files.iter().any(|x| *x == f), "missing File {f}: {pairs:?}");
        }
        for f in ["build.ps1", "LEGACY.PS1"] {
            assert!(
                pairs.contains(&("ps.".to_string(), f.to_string())),
                "ps. must contain {f}: {pairs:?}"
            );
        }
        let util: Vec<&String> = pairs
            .iter()
            .filter(|(_, f)| f == "sub/Util.psm1")
            .map(|(m, _)| m)
            .collect();
        assert!(
            !util.is_empty() && util.iter().all(|m| m.starts_with("ps.") && *m != "ps."),
            "sub/Util.psm1 sits in a non-root ps. module: {util:?}"
        );

        let roots = query_rows(
            &repo,
            "MATCH (l:Language)-[:Contains]->(m:Module) WHERE m.fqn = 'ps.' RETURN l.fqn, m.fqn",
            "l.fqn",
            "m.fqn",
        );
        assert_eq!(
            roots,
            vec![("ps".to_string(), "ps.".to_string())],
            "the ps Language contains ps."
        );
    }

    /// Auto-detection (no `--language`) selects ps, and each PowerShell File is
    /// claimed by the ps frontend alone — exactly one, ps-rooted, containing
    /// Module — while README.md stays in the md stream.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn auto_detect_selects_ps_and_claims_every_ps_file() {
        let repo = ps_repo("ps-auto");
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&"ps".to_string()),
            "auto-detection must select ps: {log}"
        );

        let pairs = module_files(&repo);
        for f in PS_FILES {
            let mods: Vec<&String> = pairs
                .iter()
                .filter(|(_, x)| x == f)
                .map(|(m, _)| m)
                .collect();
            assert_eq!(mods.len(), 1, "{f} has exactly one Module: {mods:?}");
            assert!(
                mods[0].starts_with("ps."),
                "{f} must sit under a ps. module, not {}",
                mods[0]
            );
        }
        assert!(
            pairs
                .iter()
                .any(|(m, f)| f == "README.md" && m.starts_with("md.")),
            "README.md stays under md.: {pairs:?}"
        );
    }
}
