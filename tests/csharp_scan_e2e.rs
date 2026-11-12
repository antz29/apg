//! csharp-projects (e2e): the C# frontend's project-aware and Razor scan
//! tier, driving the staged `csharpfrontend` / the CANDIDATE binary
//! (`testutil::apg_bin`) against scratch `/tmp` trees only — never the apg
//! repo. Every test is `#[ignore]`d and runs through `cargo test-e2e`.

mod common;

use common::{ApgCommand, Repo};
use std::path::{Path, PathBuf};

/// A minimal SDK-style project file (its compile set is the `**/*.cs` glob).
const SDK_CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <TargetFramework>net9.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n";

/// A `.cshtml` view with an `@functions` method: `Greet` is named on line 3.
/// No trailing newline, so its line count is `split('\n').count()` (7).
const VIEW_CSHTML: &str = "<h1>Home</h1>\n@functions {\n    public string Greet()\n    {\n        return \"hi\";\n    }\n}";

/// A `.razor` component with an `@code` method: `Increment` is named on
/// line 6. No trailing newline (10 lines).
const COUNTER_RAZOR: &str = "<p>@Count</p>\n<button>Go</button>\n@code {\n    private int Count;\n\n    private void Increment()\n    {\n        Count++;\n    }\n}";

/// The staged single-file `csharpfrontend` the candidate runs
/// (`<profile>/frontends/csharpfrontend`), resolved from
/// `testutil::apg_bin()`. Fails loudly naming the build when it is absent.
fn csharp_frontend_bin() -> PathBuf {
    let dir = common::apg_bin()
        .parent()
        .expect("apg binary has a parent")
        .join("frontends");
    let bin = ["csharpfrontend", "csharpfrontend.exe"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file());
    bin.unwrap_or_else(|| {
        panic!(
            "csharpfrontend not staged in {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"csharp\"'",
            dir.display()
        )
    })
}

/// A fresh scratch directory under the system temp dir (never the apg repo).
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("apg-csharp-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Writes `text` at `dir/name` and returns its absolute path as a string.
fn write(dir: &Path, name: &str, text: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, text).expect("write fixture");
    p.to_str().expect("utf-8 path").to_string()
}

/// Runs the candidate `apg scan . --language csharp <args>` in `repo` with
/// NUGET_PACKAGES, APG_CSHARP_DOTNET_ROOT and APG_CSHARP_REFASM_ROOT on
/// empty scratch dirs (outside the repo), asserting exit 0 and no csharp
/// frontend failure. Returns the scan log (stderr).
fn scan_csharp(repo: &Repo, tag: &str, args: &[&str]) -> String {
    let iso = scratch(&format!("{tag}-iso"));
    let dir = |n: &str| {
        let d = iso.join(n);
        std::fs::create_dir_all(&d).expect("create isolation dir");
        d.to_str().expect("utf-8 path").to_string()
    };
    let mut full = vec!["scan", ".", "--language", "csharp"];
    full.extend_from_slice(args);
    let out = ApgCommand::new(&full)
        .cwd(&repo.root)
        .env("NUGET_PACKAGES", &dir("nuget"))
        .env("APG_CSHARP_DOTNET_ROOT", &dir("dotnet"))
        .env("APG_CSHARP_REFASM_ROOT", &dir("refasm"))
        .output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "scan {args:?} failed: {log}");
    assert!(
        !log.contains("csharp frontend failed"),
        "csharp frontend failed: {log}"
    );
    log
}

/// Runs a read-only Cypher through the candidate `apg query --json` and
/// returns column `col` of every row, in row order.
fn query_col(repo: &Repo, cypher: &str, col: &str) -> Vec<String> {
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(&repo.root)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).expect("json rows");
    rows.iter()
        .map(|r| r[col].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every `csharp.`-rooted FQN of node label `label` (sorted).
fn csharp_fqns(repo: &Repo, label: &str) -> Vec<String> {
    query_col(
        repo,
        &format!("MATCH (n:{label}) WHERE n.fqn STARTS WITH 'csharp.' RETURN n.fqn ORDER BY n.fqn"),
        "n.fqn",
    )
}

mod e2e {
    use super::*;

    /// The normal project-dir invocation (`csharpfrontend <dir>`) on a
    /// projectless tree: each Razor file yields one `file` record spanning
    /// `1..line count` and a `function` record per `@code`/`@functions`
    /// method at its `#line`-mapped source line.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch tree, spawned csharpfrontend); run via cargo test-e2e"]
    fn razor_engine_single_file_smoke() {
        let dir = scratch("razor");
        let view = write(&dir, "Index.cshtml", VIEW_CSHTML);
        let counter = write(&dir, "Counter.razor", COUNTER_RAZOR);

        let bin = csharp_frontend_bin();
        let out = std::process::Command::new(&bin)
            .arg(&dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(0),
            "csharpfrontend failed: {stderr}"
        );
        let records: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad record `{l}`: {e}")))
            .collect();

        // Line count convention: the fixtures carry no trailing newline, so
        // Roslyn's SourceText line count (line breaks + 1) equals
        // `split('\n').count()`.
        for (path, text, method, line) in [
            (&view, VIEW_CSHTML, "Greet", 3),
            (&counter, COUNTER_RAZOR, "Increment", 6),
        ] {
            let files: Vec<_> = records
                .iter()
                .filter(|r| r["type"] == "file" && r["path"] == path.as_str())
                .collect();
            assert_eq!(files.len(), 1, "one file record for {path}: {records:#?}");
            assert_eq!(files[0]["start_line"], 1, "{path}");
            assert_eq!(
                files[0]["end_line"],
                text.split('\n').count(),
                "end_line is the line count of {path}"
            );

            let func = records
                .iter()
                .find(|r| {
                    r["type"] == "function" && r["name"] == method && r["path"] == path.as_str()
                })
                .unwrap_or_else(|| panic!("no function {method} in {path}: {records:#?}"));
            assert_eq!(
                func["start_line"], line,
                "{method} start_line is its #line-mapped line in {path}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 6 task 22: global-namespace types render without a leading
    /// `.`; a same-named global type in two projects (or in a project and the
    /// projectless set) is qualified by its project's repo-relative `.csproj`
    /// path; top-level-statements entry points stay per-project unique; and a
    /// target-filtered re-scan keeps Calls/Uses on the global type equal to a
    /// `--force` rebuild. Fixtures use no BCL member beyond `object`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repos, spawned apg + csharpfrontend); run via cargo test-e2e"]
    fn global_namespace_types_scan_end_to_end() {
        csharp_frontend_bin(); // fail loudly when the frontend is not staged

        // (1) projectless global types.
        let repo = Repo::new("cs-global-1");
        repo.write(
            "Alpha.cs",
            "class Alpha { void M() {} class Inner {} }\nclass Alpha<T> {}\n",
        );
        repo.commit_all("fixture");
        scan_csharp(&repo, "g1", &[]);
        let structs = csharp_fqns(&repo, "Struct");
        let funcs = csharp_fqns(&repo, "Function");
        for s in ["csharp.Alpha", "csharp.Alpha.Inner", "csharp.Alpha`1"] {
            assert!(structs.contains(&s.to_string()), "missing {s}: {structs:?}");
        }
        assert!(
            funcs.contains(&"csharp.Alpha.M".to_string()),
            "missing csharp.Alpha.M: {funcs:?}"
        );
        for label in ["Module", "Struct", "Function"] {
            let all = query_col(
                &repo,
                &format!("MATCH (n:{label}) RETURN n.fqn ORDER BY n.fqn"),
                "n.fqn",
            );
            let bad: Vec<_> = all
                .iter()
                .filter(|f| f.starts_with('.') || f.starts_with("csharp.."))
                .collect();
            assert!(bad.is_empty(), "{label} FQN starts with `.`: {bad:?}");
        }

        // (5) target-filtered re-scan after editing only the caller file: the
        // caller's type holds a field of type Alpha and calls Alpha.M (public,
        // so the call binds; the nested Inner is the regression's trigger).
        let repo = Repo::new("cs-global-5");
        repo.write(
            "Alpha.cs",
            "class Alpha { public void M() {} class Inner {} }\nclass Alpha<T> {}\n",
        );
        let caller = "class Caller\n{\n    Alpha a;\n    void Go() { a.M(); }\n}\n";
        repo.write("Caller.cs", caller);
        repo.commit_all("fixture");
        scan_csharp(&repo, "g5", &[]);
        let calls = "MATCH (a:Function)-[:Calls]->(b:Function) WHERE b.fqn = 'csharp.Alpha.M' RETURN a.fqn ORDER BY a.fqn";
        let uses = "MATCH (a)-[:Uses]->(b:Struct) WHERE b.fqn = 'csharp.Alpha' RETURN a.fqn ORDER BY a.fqn";
        repo.write("Caller.cs", &caller.replace("}\n}", "}\n    // edited\n}"));
        let log = scan_csharp(&repo, "g5", &[]);
        assert!(
            log.contains("[scan] incremental: 1 target file(s)"),
            "re-scan not target-filtered to Caller.cs: {log}"
        );
        let inc_calls = query_col(&repo, calls, "a.fqn");
        let inc_uses = query_col(&repo, uses, "a.fqn");
        assert_eq!(
            inc_calls,
            vec!["csharp.Caller.Go".to_string()],
            "Calls on csharp.Alpha.M"
        );
        assert!(
            inc_uses.contains(&"csharp.Caller".to_string()),
            "type-reference Uses on csharp.Alpha: {inc_uses:?}"
        );
        scan_csharp(&repo, "g5", &["--force"]);
        assert_eq!(
            query_col(&repo, calls, "a.fqn"),
            inc_calls,
            "Calls vs --force"
        );
        assert_eq!(query_col(&repo, uses, "a.fqn"), inc_uses, "Uses vs --force");

        // (2) two projects each declaring a global Alpha.
        let repo = Repo::new("cs-global-2");
        for p in ["P1", "P2"] {
            repo.write(&format!("{p}/{p}.csproj"), SDK_CSPROJ);
            repo.write(&format!("{p}/Alpha.cs"), "class Alpha { void M() {} }\n");
        }
        repo.commit_all("fixture");
        scan_csharp(&repo, "g2", &[]);
        let structs = csharp_fqns(&repo, "Struct");
        let funcs = csharp_fqns(&repo, "Function");
        let modules = csharp_fqns(&repo, "Module");
        for p in ["P1", "P2"] {
            let m = format!("csharp.{p}/{p}.csproj");
            assert!(
                structs.contains(&format!("{m}.Alpha")),
                "{m}.Alpha: {structs:?}"
            );
            assert!(
                funcs.contains(&format!("{m}.Alpha.M")),
                "{m}.Alpha.M: {funcs:?}"
            );
            assert!(modules.contains(&m), "module {m}: {modules:?}");
            assert!(!structs.contains(&m), "module {m} doubles as a Struct");
        }
        assert!(
            modules.iter().all(|m| !m.ends_with('.')),
            "trailing-dot module: {modules:?}"
        );
        assert!(
            !structs.contains(&"csharp.Alpha".to_string()),
            "plain csharp.Alpha: {structs:?}"
        );

        // (3) a projectless global Program beside a project's global Program.
        let repo = Repo::new("cs-global-3");
        repo.write("Program.cs", "class Program {}\n");
        repo.write("App/App.csproj", SDK_CSPROJ);
        repo.write("App/Program.cs", "class Program {}\n");
        repo.commit_all("fixture");
        scan_csharp(&repo, "g3", &[]);
        let structs = csharp_fqns(&repo, "Struct");
        for s in ["csharp.Program", "csharp.App/App.csproj.Program"] {
            assert!(structs.contains(&s.to_string()), "missing {s}: {structs:?}");
        }

        // (4) two projects each holding a top-level-statements Program.cs.
        let repo = Repo::new("cs-global-4");
        for p in ["P1", "P2"] {
            repo.write(&format!("{p}/{p}.csproj"), SDK_CSPROJ);
            repo.write(&format!("{p}/Program.cs"), "object o = new object();\n");
        }
        repo.commit_all("fixture");
        scan_csharp(&repo, "g4", &[]);
        // Non-vacuous: each Program.cs was scanned, under its own project.
        for p in ["P1", "P2"] {
            let q = format!(
                "MATCH (m:Module)-[:Contains]->(f:File) WHERE f.fqn = '{p}/Program.cs' RETURN m.fqn"
            );
            assert_eq!(
                query_col(&repo, &q, "m.fqn"),
                vec![format!("csharp.{p}/{p}.csproj")],
                "{p}/Program.cs module"
            );
        }
        for label in ["Struct", "Function"] {
            let q = format!(
                "MATCH (n:{label}) WHERE n.fqn STARTS WITH 'csharp.' RETURN n.fqn, n.path ORDER BY n.fqn"
            );
            let fqns = query_col(&repo, &q, "n.fqn");
            let paths = query_col(&repo, &q, "n.path");
            let mut uniq = fqns.clone();
            uniq.dedup();
            assert_eq!(uniq, fqns, "{label} FQNs not unique");
            for p in ["P1", "P2"] {
                let parent = format!("csharp.{p}/{p}.csproj.");
                // Whatever the frontend emits for the synthesized entry point
                // (possibly nothing) sits under its own project.
                for (f, _) in fqns
                    .iter()
                    .zip(&paths)
                    .filter(|(_, path)| path.starts_with(&format!("{p}/")))
                {
                    assert!(f.starts_with(&parent), "{f} not under {parent}");
                }
            }
        }
    }
}
