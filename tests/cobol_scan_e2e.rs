//! Phase-02 task-27 (e2e): `apg scan` over a COBOL scratch repo, driven
//! through the CANDIDATE binary (`testutil::apg_bin`) — never the apg repo.
//! Every test is `#[ignore]`d and runs through `cargo test-e2e`.

mod common;

use common::{ApgCommand, Repo};
use std::process::Output;

const HELLO: &str = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. HELLO.\n       PROCEDURE DIVISION.\n           DISPLAY 'HI'.\n           STOP RUN.\n";

/// A scratch repo with COBOL programs at the root (`HELLO.cob`), in a nested
/// directory (`src/cobol/PROG.CBL`), a program that COPYs a copybook from
/// `lib/` (`app/USES.cob`, `lib/COPYDEF.cpy`), and a Markdown file for the
/// structural stream. The layout is the versioned `apg/` scaffold.
fn cobol_repo(tag: &str) -> Repo {
    let repo = Repo::new(tag);
    repo.write("HELLO.cob", HELLO);
    repo.write(
        "src/cobol/PROG.CBL",
        "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. PROG.\n       PROCEDURE DIVISION.\n           STOP RUN.\n",
    );
    repo.write(
        "app/USES.cob",
        "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. USES.\n       PROCEDURE DIVISION.\n       MAIN-PARA.\n       COPY COPYDEF.\n           STOP RUN.\n",
    );
    repo.write(
        "lib/COPYDEF.cpy",
        "       CPY-PARA.\n           DISPLAY 'C'.\n",
    );
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
/// returns the rows as a JSON array of objects keyed by column name.
fn query_rows(repo: &Repo, cypher: &str) -> Vec<serde_json::Value> {
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(&repo.root)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json rows")
}

/// The string values of column `col` across `rows`.
fn column(rows: &[serde_json::Value], col: &str) -> Vec<String> {
    rows.iter()
        .map(|r| r[col].as_str().unwrap_or_default().to_string())
        .collect()
}

mod e2e {
    use super::*;

    /// `--language cobol` selects the COBOL frontend; the program identities
    /// follow the language-identity rule: a root-level program roots at
    /// `cobol..PROG`, and each COBOL directory is the module `cobol.<dir>`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn language_flag_selects_cobol_and_renders_dir_modules() {
        let repo = cobol_repo("cobol-lang");
        let (out, log) = scan(&repo, &["--language", "cobol"]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&"cobol".to_string()),
            "Languages line must list cobol: {log}"
        );

        let structs = column(&query_rows(&repo, "MATCH (n:Struct) RETURN n.fqn"), "n.fqn");
        for fqn in ["cobol..HELLO", "cobol.src/cobol.PROG", "cobol.app.USES"] {
            assert!(
                structs.contains(&fqn.to_string()),
                "missing {fqn}: {structs:?}"
            );
        }
        let modules = column(&query_rows(&repo, "MATCH (n:Module) RETURN n.fqn"), "n.fqn");
        for m in ["cobol.", "cobol.src/cobol", "cobol.app", "cobol.lib"] {
            assert!(
                modules.contains(&m.to_string()),
                "missing module {m}: {modules:?}"
            );
        }
    }

    /// Auto-detection (no `--language`) selects COBOL for a tree carrying
    /// `.cob`, `.CBL` and `.cpy` sources.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn auto_detect_selects_cobol_for_cob_cbl_and_cpy() {
        let repo = cobol_repo("cobol-auto");
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&"cobol".to_string()),
            "auto-detection must select cobol: {log}"
        );
        let files = column(&query_rows(&repo, "MATCH (f:File) RETURN f.fqn"), "f.fqn");
        for f in ["HELLO.cob", "src/cobol/PROG.CBL", "lib/COPYDEF.cpy"] {
            assert!(
                files.contains(&f.to_string()),
                "missing File {f}: {files:?}"
            );
        }
    }

    /// `--copy-path lib` resolves `COPY COPYDEF.` to the copybook: the
    /// paragraph it declares is located in `lib/COPYDEF.cpy`. Without the
    /// flag the copybook is not searched, so the declaration is absent.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn copy_path_resolves_the_copybook() {
        let with = cobol_repo("cobol-copy-on");
        let (out, log) = scan(&with, &["--language", "cobol", "--copy-path", "lib"]);
        assert!(out.status.success(), "scan with --copy-path: {log}");
        let rows = query_rows(&with, "MATCH (n:Function) RETURN n.fqn, n.path");
        let found: Vec<(String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r["n.fqn"].as_str().unwrap_or_default().to_string(),
                    r["n.path"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        assert!(
            found.contains(&(
                "cobol.app.USES.CPY-PARA".to_string(),
                "lib/COPYDEF.cpy".to_string()
            )),
            "copybook paragraph located in lib/COPYDEF.cpy: {found:?}"
        );

        let without = cobol_repo("cobol-copy-off");
        let (out, log) = scan(&without, &["--language", "cobol"]);
        assert!(out.status.success(), "scan without --copy-path: {log}");
        let paths = column(
            &query_rows(&without, "MATCH (n:Function) RETURN n.path"),
            "n.path",
        );
        assert!(
            !paths.contains(&"lib/COPYDEF.cpy".to_string()),
            "without --copy-path the copybook is not searched: {paths:?}"
        );
    }

    /// `--std cobol2023` is a refused placeholder: the scan exits non-zero
    /// with the dialect's message, before any frontend runs.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn std_cobol2023_is_refused_with_its_message() {
        let repo = cobol_repo("cobol-2023");
        let (out, log) = scan(&repo, &["--language", "cobol", "--std", "cobol2023"]);
        assert!(!out.status.success(), "cobol2023 must refuse: {log}");
        assert!(
            log.contains("dialect cobol2023 is a placeholder with no features yet"),
            "refusal message: {log}"
        );
    }

    /// The dialect folds into the scan cache key: a tree change followed by
    /// `--std cobol2002` (after a `cobol85` scan) logs `cache key drifted` and
    /// rebuilds the COBOL facts, while the same dialect on the next change
    /// reuses them with no drift.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn std_cobol2002_changes_the_cache_key() {
        let repo = cobol_repo("cobol-cache");
        let (out, log) = scan(&repo, &["--language", "cobol"]);
        assert!(out.status.success(), "first scan: {log}");

        repo.write("HELLO.cob", &HELLO.replace("'HI'", "'HI1'"));
        let (out, log) = scan(&repo, &["--language", "cobol", "--std", "cobol85"]);
        assert!(out.status.success(), "cobol85 scan: {log}");
        assert!(
            !log.contains("cache key drifted"),
            "same key must not drift: {log}"
        );

        repo.write("HELLO.cob", &HELLO.replace("'HI'", "'HI2'"));
        let (out, log) = scan(&repo, &["--language", "cobol", "--std", "cobol2002"]);
        assert!(out.status.success(), "cobol2002 scan: {log}");
        assert!(
            log.contains("cache key drifted"),
            "--std cobol2002 must change the cache key: {log}"
        );

        repo.write("HELLO.cob", &HELLO.replace("'HI'", "'HI3'"));
        let (out, log) = scan(&repo, &["--language", "cobol", "--std", "cobol2002"]);
        assert!(out.status.success(), "repeat cobol2002 scan: {log}");
        assert!(
            !log.contains("cache key drifted"),
            "the new key is stable across scans: {log}"
        );
    }

    /// A `.cob` source is claimed by the COBOL frontend alone: no structural
    /// stream (`misc`, `md`, …) carries it, so every module that contains a
    /// COBOL file is a `cobol.` module.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn cob_files_are_absent_from_the_structural_stream() {
        let repo = cobol_repo("cobol-claim");
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");
        let rows = query_rows(
            &repo,
            "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn, f.fqn",
        );
        let cobol_files: Vec<(String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r["m.fqn"].as_str().unwrap_or_default().to_string(),
                    r["f.fqn"].as_str().unwrap_or_default().to_string(),
                )
            })
            .filter(|(_, f)| f.ends_with(".cob") || f.ends_with(".CBL") || f.ends_with(".cpy"))
            .collect();
        assert_eq!(
            cobol_files.len(),
            4,
            "every COBOL file is a File node: {cobol_files:?}"
        );
        for (module, file) in &cobol_files {
            assert!(
                module.starts_with("cobol."),
                "{file} must sit under a cobol module, not {module}"
            );
        }
    }
}
