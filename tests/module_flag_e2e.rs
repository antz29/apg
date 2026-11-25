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
//! reuses the recorded graph; and, per honouring code language, a relative
//! `--module` and its absolute form each admit exactly the selected subtree's
//! files and nothing from the sibling subtree.
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

/// A committed scratch VIRTUAL Cargo workspace whose members are `crates/a`
/// and its string-prefix sibling `crates/ab`. The Rust frontend needs a Cargo
/// manifest, and the two members make the `--module crates/a` sibling-prefix
/// admission case real (a string prefix would wrongly admit `crates/ab`).
fn cargo_workspace_repo(tag: &str) -> Repo {
    let repo = Repo::new(tag);
    repo.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/a\", \"crates/ab\"]\nresolver = \"2\"\n",
    );
    repo.write(
        "crates/a/Cargo.toml",
        "[package]\nname = \"crate-a\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    repo.write("crates/a/src/lib.rs", "pub fn a() {}\n");
    repo.write(
        "crates/ab/Cargo.toml",
        "[package]\nname = \"crate-ab\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    repo.write("crates/ab/src/lib.rs", "pub fn ab() {}\n");
    repo.commit_all("fixture");
    repo
}

/// Runs `apg scan . --language rust --no-build-scripts [args]` in `repo` and
/// returns the output plus its stderr text.
fn scan_rust(repo: &Repo, args: &[&str]) -> (Output, String) {
    let mut full = vec!["scan", ".", "--language", "rust", "--no-build-scripts"];
    full.extend_from_slice(args);
    let out = ApgCommand::new(&full).cwd(&repo.root).output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    (out, log)
}

/// The admitted Rust source-file File FQNs under the fixture workspace's
/// `crates/` tree, sorted.
fn crate_files(repo: &Repo) -> Vec<String> {
    let cypher =
        "MATCH (f:File) WHERE f.fqn STARTS WITH 'crates/' AND f.fqn ENDS WITH '.rs' RETURN f.fqn";
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

/// A committed scratch tree carrying `files` (repo-relative path, source) as
/// its whole fixture.
fn lang_repo(tag: &str, files: &[(&str, &str)]) -> Repo {
    let repo = Repo::new(tag);
    for (path, source) in files {
        repo.write(path, source);
    }
    repo.commit_all("fixture");
    repo
}

/// Runs `apg scan . --language <lang> [args]` in `repo` and returns the output
/// plus its stderr text. The C# frontend resolves framework reference
/// assemblies from the dotnet root and NuGet cache, so it is pointed at empty
/// scratch dirs (mirroring `tests/csharp_scan_e2e.rs`) to read nothing
/// developer-local and fall back to its embedded net9 references.
fn scan_lang(repo: &Repo, lang: &str, args: &[&str]) -> (Output, String) {
    let mut full = vec!["scan", ".", "--language", lang];
    full.extend_from_slice(args);
    let mut cmd = ApgCommand::new(&full).cwd(&repo.root);
    if lang == "csharp" {
        let iso = std::env::temp_dir().join(format!(
            "{}-cs-iso",
            repo.root.file_name().unwrap().to_string_lossy()
        ));
        let dir = |n: &str| {
            let d = iso.join(n);
            std::fs::create_dir_all(&d).unwrap();
            d.to_str().unwrap().to_string()
        };
        cmd = cmd
            .env("HOME", &dir("home"))
            .env("NUGET_PACKAGES", &dir("nuget"))
            .env("APG_CSHARP_DOTNET_ROOT", &dir("dotnet"))
            .env("APG_CSHARP_REFASM_ROOT", &dir("refasm"));
    }
    let out = cmd.output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    (out, log)
}

/// The admitted File FQNs under one of `prefixes` and ending in `suffix`,
/// sorted. The suffix scopes the query to the language's own files: the
/// bundled structural streams also claim the fixture's `.json`/`.csproj` text,
/// and those must not count as the selected code subtree.
fn files_under(repo: &Repo, prefixes: &[&str], suffix: &str) -> Vec<String> {
    let under = prefixes
        .iter()
        .map(|p| format!("f.fqn STARTS WITH '{p}/'"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let cypher =
        format!("MATCH (f:File) WHERE ({under}) AND f.fqn ENDS WITH '{suffix}' RETURN f.fqn");
    let out = ApgCommand::new(&["query", "--json", cypher.as_str()])
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

/// Asserts both `--module` spellings on one fixture: the relative
/// `selected` and its absolute form each exit 0 and admit exactly `expected`
/// — never the `other` sibling subtree, and never an empty set. The query
/// spans both sibling prefixes, so an ignored flag (which admits `other`) and
/// an over-restricted selection (which admits nothing) both fail.
fn check_relative_and_absolute(
    tag: &str,
    lang: &str,
    files: &[(&str, &str)],
    selected: &str,
    other: &str,
    suffix: &str,
    expected: &[&str],
) {
    let repo = lang_repo(tag, files);
    let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();

    let (out, log) = scan_lang(&repo, lang, &["--module", selected]);
    assert!(
        out.status.success(),
        "[{lang}] relative `--module {selected}` scan failed: {log}"
    );
    assert_eq!(
        files_under(&repo, &[selected, other], suffix),
        expected,
        "[{lang}] relative `--module {selected}` must admit only its subtree: {log}"
    );

    let abs = repo.root.join(selected);
    let abs_s = abs.to_str().expect("utf-8 absolute module path");
    let (out, log) = scan_lang(&repo, lang, &["--module", abs_s]);
    assert!(
        out.status.success(),
        "[{lang}] absolute `--module {abs_s}` scan failed: {log}"
    );
    assert_eq!(
        files_under(&repo, &[selected, other], suffix),
        expected,
        "[{lang}] absolute `--module {abs_s}` must admit the identical set: {log}"
    );

    remove(&repo);
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

    /// A `--module` value whose `..` crosses a symlink names the PHYSICAL
    /// subtree: with `inner/link -> ../A`, `inner/link/../A` is physically
    /// `A`. A lexical normalisation yields the non-existent `inner/A` and
    /// admits nothing, so this pins the one spelling every frontend receives.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo under /tmp, spawned apg); run via cargo test-e2e"]
    fn dotdot_crossing_a_symlink_selects_the_physical_subtree() {
        let repo = module_repo("module-flag-symlink-dotdot");
        std::fs::create_dir_all(repo.root.join("inner")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../A", repo.root.join("inner/link")).unwrap();
        #[cfg(not(unix))]
        {
            println!(
                "skipping dotdot_crossing_a_symlink_selects_the_physical_subtree: \
                 symlink creation is unavailable on this platform"
            );
            return;
        }
        repo.commit_all("fixture symlink");

        let (out, log) = scan(&repo, &["--module", "inner/link/../A"]);
        assert!(
            out.status.success(),
            "`--module inner/link/../A` scan: {log}"
        );
        assert_admits(
            &repo,
            "symlink-crossing .. resolves physically",
            &["A/one.md"],
        );

        // The driver resolved that value to the physical `A`, so re-requesting
        // the subtree as the plain `A` names the same recorded selection and
        // reuses it. A driver that carried the `..`-bearing spelling would
        // record a different selection and report STALE here.
        let (out, log) = scan(&repo, &["--module", "A"]);
        assert!(out.status.success(), "`--module A` re-scan: {log}");
        assert!(
            log.contains("→ FRESH") && log.contains("fast-path"),
            "`A` and `inner/link/../A` must resolve to one recorded selection: {log}"
        );
        assert_admits(
            &repo,
            "the same physical subtree re-requested",
            &["A/one.md"],
        );

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

    /// RUST sibling-prefix containment: on one unchanged virtual Cargo
    /// workspace, `--module crates/a` admits exactly crate a's sources and
    /// never its string-prefix sibling `crates/ab`, while the same tree
    /// scanned flagless admits both members. Admission only — the freshness
    /// legs are the other tests' concern.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch cargo workspace under /tmp, spawned apg + rustfrontend); run via cargo test-e2e"]
    fn rust_module_value_excludes_the_prefix_sibling_crate() {
        let repo = cargo_workspace_repo("module-flag-rust-prefix");

        // (1) `--module crates/a`: the component-wise comparison stops at the
        // directory boundary, so `crates/ab` — which a string prefix would have
        // admitted — is excluded.
        let (out, log) = scan_rust(&repo, &["--module", "crates/a"]);
        assert!(out.status.success(), "`--module crates/a` rust scan: {log}");
        assert_eq!(
            crate_files(&repo),
            vec!["crates/a/src/lib.rs".to_string()],
            "`--module crates/a` must admit only crate a's sources, never crates/ab"
        );

        // (2) The same tree, flagless: every workspace member is admitted.
        let (out, log) = scan_rust(&repo, &[]);
        assert!(out.status.success(), "flagless rust scan: {log}");
        assert_eq!(
            crate_files(&repo),
            vec![
                "crates/a/src/lib.rs".to_string(),
                "crates/ab/src/lib.rs".to_string(),
            ],
            "a flagless rust scan must admit both workspace members"
        );

        remove(&repo);
    }

    /// Per-honouring-language CLI-through admission: on a scratch tree with two
    /// sibling source subtrees, a relative `--module <selected>` and its
    /// absolute form each exit 0 and admit exactly the selected subtree's
    /// files — never the sibling's. Rust reuses phase-4 task-17's cargo fixture
    /// and pins only the absolute form here (its relative pin is that task's).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repos under /tmp, spawned apg + per-language frontends); run via cargo test-e2e"]
    fn per_language_relative_and_absolute_admission() {
        // go (selection is module-granular: each sibling is its own module)
        check_relative_and_absolute(
            "lang-go",
            "go",
            &[
                ("A/go.mod", "module example.com/a\n\ngo 1.25\n"),
                ("A/a.go", "package a\n\nfunc A() {}\n"),
                ("B/go.mod", "module example.com/b\n\ngo 1.25\n"),
                ("B/b.go", "package b\n\nfunc B() {}\n"),
            ],
            "A",
            "B",
            ".go",
            &["A/a.go"],
        );

        // java
        check_relative_and_absolute(
            "lang-java",
            "java",
            &[
                ("A/A.java", "package a;\n\npublic class A {}\n"),
                ("B/B.java", "package b;\n\npublic class B {}\n"),
            ],
            "A",
            "B",
            ".java",
            &["A/A.java"],
        );

        // cpp
        check_relative_and_absolute(
            "lang-cpp",
            "cpp",
            &[
                ("A/a.cpp", "int a() { return 1; }\n"),
                ("B/b.cpp", "int b() { return 2; }\n"),
            ],
            "A",
            "B",
            ".cpp",
            &["A/a.cpp"],
        );

        // ts (package-granular; js shares this arm)
        check_relative_and_absolute(
            "lang-ts",
            "ts",
            &[
                (
                    "package.json",
                    "{\"name\":\"ws\",\"private\":true,\"workspaces\":[\"packages/*\"]}\n",
                ),
                ("packages/a/package.json", "{\"name\":\"a\"}\n"),
                ("packages/a/a.ts", "export class A {}\n"),
                ("packages/b/package.json", "{\"name\":\"b\"}\n"),
                ("packages/b/b.ts", "export class B {}\n"),
            ],
            "packages/a",
            "packages/b",
            ".ts",
            &["packages/a/a.ts"],
        );

        // csharp
        const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net9.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n";
        check_relative_and_absolute(
            "lang-csharp",
            "csharp",
            &[
                ("A/A.csproj", CSPROJ),
                ("A/A.cs", "namespace A;\n\npublic class A {}\n"),
                ("B/B.csproj", CSPROJ),
                ("B/B.cs", "namespace B;\n\npublic class B {}\n"),
            ],
            "A",
            "B",
            ".cs",
            &["A/A.cs"],
        );

        // py
        check_relative_and_absolute(
            "lang-py",
            "py",
            &[
                ("A/a.py", "def a():\n    pass\n"),
                ("B/b.py", "def b():\n    pass\n"),
            ],
            "A",
            "B",
            ".py",
            &["A/a.py"],
        );

        // cobol (fixed format, default dialect)
        const COBOL_A: &str = "000100 IDENTIFICATION DIVISION.\n000200 PROGRAM-ID. A.\n000300 PROCEDURE DIVISION.\n000400     STOP RUN.\n";
        const COBOL_B: &str = "000100 IDENTIFICATION DIVISION.\n000200 PROGRAM-ID. B.\n000300 PROCEDURE DIVISION.\n000400     STOP RUN.\n";
        check_relative_and_absolute(
            "lang-cobol",
            "cobol",
            &[("A/a.cob", COBOL_A), ("B/b.cob", COBOL_B)],
            "A",
            "B",
            ".cob",
            &["A/a.cob"],
        );

        // sql
        check_relative_and_absolute(
            "lang-sql",
            "sql",
            &[
                ("A/a.sql", "CREATE TABLE a_t (id INT);\n"),
                ("B/b.sql", "CREATE TABLE b_t (id INT);\n"),
            ],
            "A",
            "B",
            ".sql",
            &["A/a.sql"],
        );

        // ps
        check_relative_and_absolute(
            "lang-ps",
            "ps",
            &[
                ("A/a.ps1", "function Get-A { 1 }\n"),
                ("B/b.ps1", "function Get-B { 2 }\n"),
            ],
            "A",
            "B",
            ".ps1",
            &["A/a.ps1"],
        );

        // rust: reuse phase-4 task-17's scratch cargo workspace; this leg pins
        // only the absolute form (its relative form is that task's).
        let repo = cargo_workspace_repo("module-flag-rust-abs");
        let abs = repo.root.join("crates/a");
        let abs_s = abs.to_str().expect("utf-8 absolute module path");
        let (out, log) = scan_rust(&repo, &["--module", abs_s]);
        assert!(
            out.status.success(),
            "[rust] absolute `--module {abs_s}` scan failed: {log}"
        );
        assert_eq!(
            crate_files(&repo),
            vec!["crates/a/src/lib.rs".to_string()],
            "[rust] absolute `--module {abs_s}` must admit only crate a, never crates/ab"
        );
        remove(&repo);
    }
}
