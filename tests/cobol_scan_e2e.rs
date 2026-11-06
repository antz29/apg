//! Phase-02 task-27 (e2e): `apg scan` over a COBOL scratch repo, driven
//! through the CANDIDATE binary (`testutil::apg_bin`) — never the apg repo.
//! Every test is `#[ignore]`d and runs through `cargo test-e2e`.

mod common;

use common::{ApgCommand, Repo};
use std::process::Output;

const HELLO: &str = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. HELLO.\n       PROCEDURE DIVISION.\n           DISPLAY 'HI'.\n           STOP RUN.\n";

/// Root-level class: a FACTORY `make` and an OBJECT `make` (same name), plus
/// an OBJECT `deposit` that INVOKEs `balance-of` on SELF.
const OO_CLASS: &str = r"       IDENTIFICATION DIVISION.
       CLASS-ID. ACCT.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
       FACTORY.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 made PIC 9(5) VALUE 0.
       PROCEDURE DIVISION.
       METHOD-ID. make.
       DATA DIVISION.
       LINKAGE SECTION.
       01 new-acct OBJECT REFERENCE ACCT.
       PROCEDURE DIVISION RETURNING new-acct.
           INVOKE SELF 'new' RETURNING new-acct.
       END METHOD make.
       END FACTORY.
       OBJECT.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 balance PIC S9(7)V99 VALUE 0.
       01 cur PIC S9(7)V99.
       PROCEDURE DIVISION.
       METHOD-ID. make.
       DATA DIVISION.
       LINKAGE SECTION.
       01 amount PIC S9(7)V99.
       PROCEDURE DIVISION USING amount.
           ADD amount TO balance.
       END METHOD make.
       METHOD-ID. deposit.
       DATA DIVISION.
       LINKAGE SECTION.
       01 amount PIC S9(7)V99.
       PROCEDURE DIVISION USING amount.
           INVOKE SELF 'balance-of' RETURNING cur.
           ADD amount TO balance.
       END METHOD deposit.
       METHOD-ID. balance-of.
       DATA DIVISION.
       LINKAGE SECTION.
       01 balance-out PIC S9(7)V99.
       PROCEDURE DIVISION RETURNING balance-out.
           MOVE balance TO balance-out.
       END METHOD balance-of.
       END OBJECT.
       END CLASS ACCT.
";

/// Root-level interface with one prototype method.
const OO_INTERFACE: &str = r"       IDENTIFICATION DIVISION.
       INTERFACE-ID. SHAPE.
       METHOD-ID. area.
       DATA DIVISION.
       LINKAGE SECTION.
       01 out-area PIC 9(5).
       PROCEDURE DIVISION RETURNING out-area.
       END METHOD area.
       END INTERFACE SHAPE.
";

/// Class in the `geo/` subdirectory: its module is `cobol.geo`.
const OO_SUBDIR_CLASS: &str = r"       IDENTIFICATION DIVISION.
       CLASS-ID. POINT.
       OBJECT.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 px PIC S9(5) VALUE 0.
       PROCEDURE DIVISION.
       METHOD-ID. shift.
       DATA DIVISION.
       LINKAGE SECTION.
       01 dx PIC S9(5).
       PROCEDURE DIVISION USING dx.
           ADD dx TO px.
       END METHOD shift.
       END OBJECT.
       END CLASS POINT.
";

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

    /// Programs in a subdirectory (`app/`) that COPY a statement-only copybook
    /// (`lib/STMTS.cpy`) each get a `copy` Uses edge to the copybook File; a
    /// copybook-only edit followed by a plain re-scan keeps both rows identical.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn copybook_impact_edges_survive_a_copybook_only_rescan() {
        let repo = Repo::new("cobol-impact");
        let program = |id: &str| {
            format!(
                "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. {id}.\n       PROCEDURE DIVISION.\n       MAIN-PARA.\n       COPY STMTS.\n           STOP RUN.\n"
            )
        };
        repo.write("app/A.cob", &program("A"));
        repo.write("app/B.cob", &program("B"));
        repo.write("lib/STMTS.cpy", "           DISPLAY 'S'.\n");
        repo.commit_all("fixture");

        let cypher =
            "MATCH (s:Struct)-[r:Uses]->(f:File) RETURN s.fqn, f.fqn, r.properties ORDER BY s.fqn";
        let edges = |rows: &[serde_json::Value]| -> Vec<(String, String, String)> {
            rows.iter()
                .map(|r| {
                    (
                        r["s.fqn"].as_str().unwrap_or_default().to_string(),
                        r["f.fqn"].as_str().unwrap_or_default().to_string(),
                        r["r.properties"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        };

        let (out, log) = scan(&repo, &["--language", "cobol", "--copy-path", "lib"]);
        assert!(out.status.success(), "scan: {log}");
        let first = edges(&query_rows(&repo, cypher));
        let expected = vec![
            (
                "cobol.app.A".to_string(),
                "lib/STMTS.cpy".to_string(),
                "{\"kind\":\"copy\"}".to_string(),
            ),
            (
                "cobol.app.B".to_string(),
                "lib/STMTS.cpy".to_string(),
                "{\"kind\":\"copy\"}".to_string(),
            ),
        ];
        assert_eq!(first, expected, "copy edges after first scan");

        repo.write("lib/STMTS.cpy", "           DISPLAY 'T'.\n");
        let (out, log) = scan(&repo, &["--language", "cobol", "--copy-path", "lib"]);
        assert!(out.status.success(), "re-scan: {log}");
        let second = edges(&query_rows(&repo, cypher));
        assert_eq!(second, expected, "copybook-only re-scan keeps both rows");
    }

    /// Phase-03 task-23 (e2e): a root-level CLASS-ID renders `cobol..ACCT` with
    /// its METHOD-IDs as `cobol..ACCT.<method>`; a factory and an instance
    /// method sharing a name render `(factory)`/`(instance)`; an INTERFACE-ID
    /// renders its prototype; a subdirectory class renders `cobol.geo.POINT`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn oo_class_and_method_fqns_render() {
        let repo = Repo::new("cobol-oo");
        repo.write("ACCT.cob", OO_CLASS);
        repo.write("SHAPE.cob", OO_INTERFACE);
        repo.write("geo/POINT.cob", OO_SUBDIR_CLASS);
        repo.write(
            "ROOTP.cob",
            "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. ROOTP.\n       PROCEDURE DIVISION.\n       MAIN-PARA.\n           STOP RUN.\n",
        );
        repo.commit_all("fixture");
        let (out, log) = scan(&repo, &["--language", "cobol", "--std", "cobol2002"]);
        assert!(out.status.success(), "scan: {log}");

        let structs = column(&query_rows(&repo, "MATCH (n:Struct) RETURN n.fqn"), "n.fqn");
        for s in [
            "cobol..ACCT",
            "cobol..SHAPE",
            "cobol.geo.POINT",
            "cobol..ROOTP",
        ] {
            assert!(
                structs.contains(&s.to_string()),
                "missing Struct {s}: {structs:?}"
            );
        }
        let fns = column(
            &query_rows(&repo, "MATCH (n:Function) RETURN n.fqn"),
            "n.fqn",
        );
        for f in [
            "cobol..ACCT.deposit",
            "cobol..ACCT.balance-of",
            "cobol..ACCT.make(factory)",
            "cobol..ACCT.make(instance)",
            "cobol..SHAPE.area",
            "cobol.geo.POINT.shift",
            "cobol..ROOTP.MAIN-PARA",
        ] {
            assert!(
                fns.contains(&f.to_string()),
                "missing Function {f}: {fns:?}"
            );
        }

        let pairs = |cypher: &str, a: &str, b: &str| -> Vec<(String, String)> {
            query_rows(&repo, cypher)
                .iter()
                .map(|r| {
                    (
                        r[a].as_str().unwrap_or_default().to_string(),
                        r[b].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        };
        let contains = pairs(
            "MATCH (s:Struct)-[:Contains]->(f:Function) RETURN s.fqn, f.fqn",
            "s.fqn",
            "f.fqn",
        );
        for (s, f) in [
            ("cobol..ACCT", "cobol..ACCT.deposit"),
            ("cobol..ACCT", "cobol..ACCT.make(factory)"),
            ("cobol..SHAPE", "cobol..SHAPE.area"),
            ("cobol.geo.POINT", "cobol.geo.POINT.shift"),
        ] {
            let edge = (s.to_string(), f.to_string());
            assert!(
                contains.contains(&edge),
                "missing Contains {edge:?}: {contains:?}"
            );
        }
        let files = pairs(
            "MATCH (f:File)-[:Contains]->(s:Struct) RETURN f.fqn, s.fqn",
            "f.fqn",
            "s.fqn",
        );
        let edge = ("ACCT.cob".to_string(), "cobol..ACCT".to_string());
        assert!(
            files.contains(&edge),
            "missing File→Struct {edge:?}: {files:?}"
        );

        let calls = pairs(
            "MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.fqn, b.fqn",
            "a.fqn",
            "b.fqn",
        );
        let edge = (
            "cobol..ACCT.deposit".to_string(),
            "cobol..ACCT.balance-of".to_string(),
        );
        assert!(
            calls.contains(&edge),
            "missing INVOKE Calls {edge:?}: {calls:?}"
        );
    }
}
