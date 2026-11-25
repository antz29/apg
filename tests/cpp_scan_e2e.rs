//! cpp-module-identity (e2e): the C++ frontend's directory-path module
//! identities, driven through the CANDIDATE binary (`testutil::apg_bin`) and
//! the staged `cppfrontend` against self-built scratch `/tmp` git repos only —
//! never the apg repo. Every test is `#[ignore]`d and runs through
//! `cargo test-e2e`.

mod common;

use common::{ApgCommand, Repo};
use std::path::PathBuf;

/// The staged single-file `cppfrontend` the candidate runs
/// (`<profile>/frontends/cppfrontend`), resolved from `testutil::apg_bin()`.
/// Fails loudly naming the build when it is absent.
fn cpp_frontend_bin() -> PathBuf {
    let dir = common::apg_bin()
        .parent()
        .expect("apg binary has a parent")
        .join("frontends");
    let bin = ["cppfrontend", "cppfrontend.exe"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file());
    bin.unwrap_or_else(|| {
        panic!(
            "cppfrontend not staged in {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"cpp\"'",
            dir.display()
        )
    })
}

/// A scratch `/tmp` git repo (`Repo` scaffolds the versioned `apg/config.json`
/// and its initial commit) carrying `files`, committed as one `fixture` commit.
fn cpp_repo(tag: &str, files: &[(&str, &str)]) -> Repo {
    let repo = Repo::new(tag);
    for (path, source) in files {
        repo.write(path, source);
    }
    repo.commit_all("fixture");
    repo
}

/// Runs the candidate `apg scan . --language cpp <args>` in `repo`, asserting
/// exit 0 and that the cpp frontend did not fail. Returns the scan log.
fn scan_cpp(repo: &Repo, args: &[&str]) -> String {
    let _ = cpp_frontend_bin();
    let mut full = vec!["scan", ".", "--language", "cpp"];
    full.extend_from_slice(args);
    let out = ApgCommand::new(&full).cwd(&repo.root).output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "scan {args:?} failed: {log}");
    assert!(
        !log.contains("cpp frontend failed"),
        "cpp frontend failed: {log}"
    );
    log
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

/// The `(a, b)` string pairs of columns `a` and `b` across `rows`.
fn pairs(rows: &[serde_json::Value], a: &str, b: &str) -> Vec<(String, String)> {
    rows.iter()
        .map(|r| {
            (
                r[a].as_str().unwrap_or_default().to_string(),
                r[b].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

mod e2e {
    use super::*;

    /// Two source directories sharing the basename `util` keep distinct
    /// repo-relative path identities — `cpp.src/a/util` and `cpp.src/b/util` —
    /// each owning its own file and the function declared in it. Neither
    /// collapses into the other, and no identity carries the checkout
    /// basename.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/spawned apg/cppfrontend/db.lbug/git); run via cargo test-e2e"]
    fn same_basename_directories_keep_distinct_module_identities() {
        let repo = cpp_repo(
            "cpp-same-basename",
            &[
                ("src/a/util/one.cpp", "int one() { return 1; }\n"),
                ("src/b/util/two.cpp", "int two() { return 2; }\n"),
            ],
        );
        scan_cpp(&repo, &[]);

        let modules = column(
            &query_rows(
                &repo,
                "MATCH (n:Module) WHERE n.fqn STARTS WITH 'cpp.' RETURN n.fqn ORDER BY n.fqn",
            ),
            "n.fqn",
        );
        assert_eq!(
            modules,
            vec!["cpp.src/a/util".to_string(), "cpp.src/b/util".to_string()],
            "two same-basename directories keep distinct path identities"
        );

        let checkout = repo
            .root
            .file_name()
            .expect("checkout basename")
            .to_string_lossy();
        for fqn in &modules {
            assert!(
                !fqn.contains(checkout.as_ref()),
                "module {fqn} must not carry the checkout basename {checkout}"
            );
        }

        let module_files = pairs(
            &query_rows(
                &repo,
                "MATCH (m:Module)-[:Contains]->(f:File) WHERE m.fqn STARTS WITH 'cpp.' \
                 RETURN m.fqn, f.fqn ORDER BY f.fqn",
            ),
            "m.fqn",
            "f.fqn",
        );
        assert_eq!(
            module_files,
            vec![
                (
                    "cpp.src/a/util".to_string(),
                    "src/a/util/one.cpp".to_string()
                ),
                (
                    "cpp.src/b/util".to_string(),
                    "src/b/util/two.cpp".to_string()
                ),
            ],
            "each module owns only its own file"
        );

        let functions = column(
            &query_rows(
                &repo,
                "MATCH (n:Function) WHERE n.fqn STARTS WITH 'cpp.' RETURN n.fqn ORDER BY n.fqn",
            ),
            "n.fqn",
        );
        assert_eq!(
            functions,
            vec![
                "cpp.src/a/util.one".to_string(),
                "cpp.src/b/util.two".to_string()
            ],
            "each function nests under its own directory module"
        );

        let file_functions = pairs(
            &query_rows(
                &repo,
                "MATCH (f:File)-[:Contains]->(fn:Function) WHERE fn.fqn STARTS WITH 'cpp.' \
                 RETURN f.fqn, fn.fqn ORDER BY fn.fqn",
            ),
            "f.fqn",
            "fn.fqn",
        );
        assert_eq!(
            file_functions,
            vec![
                (
                    "src/a/util/one.cpp".to_string(),
                    "cpp.src/a/util.one".to_string()
                ),
                (
                    "src/b/util/two.cpp".to_string(),
                    "cpp.src/b/util.two".to_string()
                ),
            ],
            "each file contains its own module-nested function"
        );
    }

    /// A source directory whose name contains a dot (`src/foo.bar`) is ONE
    /// module identity — `cpp.src/foo.bar` — with no phantom ancestor module
    /// (`cpp.src/foo`) and no checkout-named identity: the module record, its
    /// file's parent and the declaration's scope all agree on that path.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/spawned apg/cppfrontend/db.lbug/git); run via cargo test-e2e"]
    fn dotted_directory_name_is_one_module_identity() {
        let repo = cpp_repo(
            "cpp-dotted-dir",
            &[("src/foo.bar/one.cpp", "int dotted() { return 7; }\n")],
        );
        scan_cpp(&repo, &[]);

        let modules = column(
            &query_rows(
                &repo,
                "MATCH (n:Module) WHERE n.fqn STARTS WITH 'cpp.' RETURN n.fqn ORDER BY n.fqn",
            ),
            "n.fqn",
        );
        assert_eq!(
            modules,
            vec!["cpp.src/foo.bar".to_string()],
            "a dotted directory name is exactly one module identity"
        );
        assert!(
            !modules.iter().any(|m| m == "cpp.src/foo"),
            "the dot in `foo.bar` must not fabricate the ancestor module `cpp.src/foo`: {modules:?}"
        );

        let checkout = repo
            .root
            .file_name()
            .expect("checkout basename")
            .to_string_lossy();
        for fqn in &modules {
            assert!(
                !fqn.contains(checkout.as_ref()),
                "module {fqn} must not carry the checkout basename {checkout}"
            );
        }

        let module_files = pairs(
            &query_rows(
                &repo,
                "MATCH (m:Module)-[:Contains]->(f:File) WHERE m.fqn STARTS WITH 'cpp.' \
                 RETURN m.fqn, f.fqn ORDER BY f.fqn",
            ),
            "m.fqn",
            "f.fqn",
        );
        assert_eq!(
            module_files,
            vec![(
                "cpp.src/foo.bar".to_string(),
                "src/foo.bar/one.cpp".to_string()
            )],
            "the file's parent is the one dotted-path identity"
        );

        let functions = column(
            &query_rows(
                &repo,
                "MATCH (n:Function) WHERE n.fqn STARTS WITH 'cpp.' RETURN n.fqn ORDER BY n.fqn",
            ),
            "n.fqn",
        );
        assert_eq!(
            functions,
            vec!["cpp.src/foo.bar.dotted".to_string()],
            "the declaration's scope agrees on the dotted-path module"
        );

        let file_functions = pairs(
            &query_rows(
                &repo,
                "MATCH (f:File)-[:Contains]->(fn:Function) WHERE fn.fqn STARTS WITH 'cpp.' \
                 RETURN f.fqn, fn.fqn ORDER BY fn.fqn",
            ),
            "f.fqn",
            "fn.fqn",
        );
        assert_eq!(
            file_functions,
            vec![(
                "src/foo.bar/one.cpp".to_string(),
                "cpp.src/foo.bar.dotted".to_string()
            )],
            "the file contains its dotted-module-nested function"
        );
    }
}
