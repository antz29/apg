//! Phase-07 task-20 (e2e): `apg scan` over a PowerShell scratch repo, driven
//! through the CANDIDATE binary (`testutil::apg_bin`) — never the apg repo.
//! Only the File/Module facts are asserted, so the tests stay valid as the
//! declaration/edge phases land. Every test is `#[ignore]`d and runs through
//! `cargo test-e2e`.

mod common;

use common::{ApgCommand, Repo, wt_commit};
use std::path::{Path, PathBuf};
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

/// The full-vs-incremental parity fixture: a scratch repo (whose `Repo::new`
/// `.gitignore` covers `apg/.trans/`, `apg/.worktrees/`, `apg/.scratch/`) with
/// `apg init .` committed, a full main `apg scan . --language ps`, and project
/// `demo` started; every later scan runs in the `demo` worktree. The isolated
/// HOME sits outside the repo.
struct Parity {
    repo: Repo,
    home: PathBuf,
    wt: PathBuf,
}

impl Drop for Parity {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.repo.root);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Parity {
    fn new(tag: &str, files: &[(&str, &str)]) -> Parity {
        let repo = Repo::new(tag);
        let home = repo
            .root
            .parent()
            .unwrap()
            .join(format!("apg-{tag}-home-{}", std::process::id()));
        // A present plugin dir keeps `apg init` from running `bun install`.
        std::fs::create_dir_all(home.join(".config/opencode/node_modules/@opencode/plugin"))
            .unwrap();
        for (rel, text) in files {
            repo.write(rel, text);
        }
        repo.commit_all("ps fixture");
        let wt = repo.root.join("apg").join(".worktrees").join("demo");
        let p = Parity { repo, home, wt };
        p.apg(&p.repo.root, &["init", "."]);
        p.repo.commit_all("apg init");
        p.apg(&p.repo.root, &["scan", ".", "--language", "ps"]);
        p.apg(&p.repo.root, &["project", "start", "demo"]);
        p
    }

    /// One `apg` run in `dir` with the isolated HOME; fails on a non-zero
    /// exit and returns the stderr log.
    fn apg(&self, dir: &Path, args: &[&str]) -> String {
        let out = ApgCommand::new(args)
            .cwd(dir)
            .env("HOME", self.home.to_str().unwrap())
            .output();
        let log = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "{args:?}: {}{log}",
            String::from_utf8_lossy(&out.stdout)
        );
        log
    }

    /// One parity step in the worktree: write and commit `files`, run the
    /// incremental `--language ps` scan (it must splice), check
    /// `precondition(before, after)` on the ps graphs, then a `--force` full
    /// scan (it must not splice) must reproduce the incremental ps graph.
    fn step(&self, files: &[(&str, &str)], precondition: impl FnOnce(&[String], &[String])) {
        let before = ps_graph(&self.wt);
        for (rel, text) in files {
            let p = self.wt.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let rels: Vec<&str> = files.iter().map(|(r, _)| *r).collect();
        wt_commit(&self.wt, &rels, "parity edit");
        let log = self.apg(&self.wt, &["scan", ".", "--language", "ps"]);
        assert!(spliced(&log), "incremental scan must splice: {log}");
        let incremental = ps_graph(&self.wt);
        precondition(&before, &incremental);
        let log = self.apg(&self.wt, &["scan", ".", "--language", "ps", "--force"]);
        assert!(!spliced(&log), "--force must full-rebuild: {log}");
        let force = ps_graph(&self.wt);
        assert_eq!(
            incremental, force,
            "incremental (left) ps graph must equal the --force rebuild (right) ({rels:?})"
        );
    }
}

/// True only when the scan's `[load]` line ends in `full load skipped`.
fn spliced(log: &str) -> bool {
    log.lines()
        .any(|l| l.contains("[load]") && l.trim_end().ends_with("full load skipped"))
}

/// `apg query --json` in `dir`: each row as its columns (sorted by alias)
/// joined with spaces, JSON-encoded.
fn json_lines(dir: &Path, cypher: &str) -> Vec<String> {
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(dir)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).expect("json rows");
    rows.iter()
        .map(|r| {
            let cols = r.as_object().expect("row object").values();
            cols.map(|v| v.to_string()).collect::<Vec<_>>().join(" ")
        })
        .collect()
}

/// A `.ps1`/`.psm1`/`.psd1` File FQN test on `v`, in any case.
fn ps_file(v: &str) -> String {
    let f = format!("lower({v}.fqn)");
    format!("({f} ENDS WITH '.ps1' OR {f} ENDS WITH '.psm1' OR {f} ENDS WITH '.psd1')")
}

/// An UnresolvedTarget `v` reached from a ps node.
fn ps_unresolved(v: &str) -> String {
    let from =
        |rel: &str| format!("EXISTS {{ MATCH (p)-[:{rel}]->({v}) WHERE p.fqn STARTS WITH 'ps.' }}");
    format!("({} OR {})", from("UnresolvedCall"), from("UnresolvedUse"))
}

/// The whole ps graph as sorted lines: ps Module/Struct/Function nodes, ps
/// Files, the `ps` Language, the UnresolvedTargets ps nodes reach, and every
/// Contains/Calls/Uses/UnresolvedCall/UnresolvedUse edge touching that set.
fn ps_graph(dir: &Path) -> Vec<String> {
    let in_set = |v: &str| {
        format!(
            "((label({v}) IN ['Module','Struct','Function'] AND {v}.fqn STARTS WITH 'ps.')
              OR (label({v}) = 'File' AND {})
              OR (label({v}) = 'Language' AND {v}.fqn = 'ps')
              OR (label({v}) = 'UnresolvedTarget' AND {}))",
            ps_file(v),
            ps_unresolved(v)
        )
    };
    let mut queries = vec![
        (
            "Module",
            "MATCH (n:Module) WHERE n.fqn STARTS WITH 'ps.' RETURN n.fqn AS a".to_string(),
        ),
        ("Struct", "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'ps.' RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c".to_string()),
        ("Function", "MATCH (n:Function) WHERE n.fqn STARTS WITH 'ps.' RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c".to_string()),
        (
            "File",
            format!(
                "MATCH (n:File) WHERE {} RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c",
                ps_file("n")
            ),
        ),
        (
            "Language",
            "MATCH (n:Language) WHERE n.fqn = 'ps' RETURN n.fqn AS a".to_string(),
        ),
        (
            "UnresolvedTarget",
            format!(
                "MATCH (n:UnresolvedTarget) WHERE {} RETURN n.fqn AS a, n.category AS b",
                ps_unresolved("n")
            ),
        ),
    ];
    for (rel, prop) in [
        ("Contains", "''"),
        ("Calls", "''"),
        ("Uses", "r.properties"),
        ("UnresolvedCall", "r.target_type"),
        ("UnresolvedUse", "''"),
    ] {
        queries.push((
            rel,
            format!(
                "MATCH (x)-[r:{rel}]->(y) WHERE {} OR {} RETURN x.fqn AS a, y.fqn AS b, {prop} AS c",
                in_set("x"),
                in_set("y")
            ),
        ));
    }
    let mut lines: Vec<String> = queries
        .iter()
        .flat_map(|(tag, q)| {
            json_lines(dir, q)
                .into_iter()
                .map(move |l| format!("{tag} {l}"))
        })
        .collect();
    lines.sort();
    lines
}

/// The Struct/Function FQNs located in the file `path`, sorted.
fn fqns_in(dir: &Path, path: &str) -> Vec<String> {
    let mut out: Vec<String> = ["Struct", "Function"]
        .iter()
        .flat_map(|l| {
            json_lines(
                dir,
                &format!("MATCH (n:{l}) WHERE n.path = '{path}' RETURN n.fqn AS a"),
            )
        })
        .collect();
    out.sort();
    out
}

/// The Calls/UnresolvedCall lines of `graph` sourced at one of `srcs` (JSON
/// FQNs, as [`fqns_in`] returns them).
fn calls_from(graph: &[String], srcs: &[String]) -> Vec<String> {
    graph
        .iter()
        .filter(|l| {
            ["Calls", "UnresolvedCall"]
                .iter()
                .any(|rel| srcs.iter().any(|s| l.starts_with(&format!("{rel} {s} "))))
        })
        .cloned()
        .collect()
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

    /// (a) Per unit kind, a change in one file collides with a unit of an
    /// unchanged file, re-rendering that unit's FQN; the spliced ps graph
    /// equals a `--force` rebuild. Kinds (of the re-rendered unit): class
    /// `ps1` and enum `ps1` yield to the new script `<dir>/a.ps1.ps1`;
    /// function `ps1.C` yields to class `C` gained by `f/b.ps1.ps1`; script
    /// `s/b.ps1` yields to the new directory Module `s.b.ps1`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/psfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_cross_file_collision_rerenders() {
        let p = Parity::new(
            "ps-parity-collide",
            &[
                ("m/a.ps1", "class ps1 {\n}\n"),
                ("e/a.ps1", "enum ps1 {\n    One\n}\n"),
                ("f/b.ps1", "function ps1.C {\n    'c'\n}\n"),
                ("f/b.ps1.ps1", "function Keep {\n    'k'\n}\n"),
                ("s/b.ps1", "function Get-S {\n    's'\n}\n"),
            ],
        );
        for (second, first, text) in [
            ("m/a.ps1", "m/a.ps1.ps1", "Get-A\n"),
            ("e/a.ps1", "e/a.ps1.ps1", "Get-A\n"),
            (
                "f/b.ps1",
                "f/b.ps1.ps1",
                "function Keep {\n    'k'\n}\nclass C {\n}\n",
            ),
            ("s/b.ps1", "s.b.ps1/x.ps1", "Get-A\n"),
        ] {
            let old = fqns_in(&p.wt, second);
            assert!(!old.is_empty(), "{second} has units: {old:?}");
            p.step(&[(first, text)], |_, _| {
                let new = fqns_in(&p.wt, second);
                assert_ne!(old, new, "{first} must re-render a unit of {second}");
                assert!(
                    new.iter().any(|f| f.contains(&format!("//{second}"))),
                    "{second}'s unit carries its //path suffix: {new:?}"
                );
            });
        }
    }

    /// (b) `Thing` in `lib/Lib.psm1` (manifest `lib/Lib.psd1`, exports `*`)
    /// is called by `app/a.ps1` by plain name and by `app/b.ps1` as
    /// `Lib\Thing`. Renaming it makes both calls unresolved; restoring the
    /// name beside a colliding `class Thing` re-suffixes it, and both calls
    /// resolve to the suffixed FQN. Each spliced ps graph equals `--force`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/psfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_renamed_unit_callers_reresolve() {
        let lib = "lib/Lib.psm1";
        let p = Parity::new(
            "ps-parity-rename",
            &[
                (
                    "lib/Lib.psd1",
                    "@{\n    RootModule = 'Lib.psm1'\n    FunctionsToExport = '*'\n}\n",
                ),
                (lib, "function Thing {\n    'thing'\n}\n"),
                ("app/a.ps1", "Thing\n"),
                ("app/b.ps1", "Lib\\Thing\n"),
            ],
        );
        let mut srcs = fqns_in(&p.wt, "app/a.ps1");
        srcs.extend(fqns_in(&p.wt, "app/b.ps1"));
        let calls = |g: &[String], rel: &str| {
            calls_from(g, &srcs)
                .into_iter()
                .filter(|l| l.starts_with(&format!("{rel} ")))
                .collect::<Vec<_>>()
        };

        // (1) Rename: both callers' Calls become UnresolvedCalls.
        p.step(
            &[(lib, "function Other {\n    'thing'\n}\n")],
            |before, after| {
                assert_ne!(calls_from(before, &srcs), calls_from(after, &srcs));
                assert_eq!(calls(before, "Calls").len(), 2, "{before:#?}");
                assert!(calls(after, "Calls").is_empty(), "{after:#?}");
                assert_eq!(calls(after, "UnresolvedCall").len(), 2, "{after:#?}");
            },
        );

        // (2) Re-suffix: both callers resolve to Thing//lib/Lib.psm1:<n>.
        let suffixed = "function Thing {\n    'thing'\n}\nclass Thing {\n}\n";
        p.step(&[(lib, suffixed)], |before, after| {
            assert_ne!(calls_from(before, &srcs), calls_from(after, &srcs));
            let resolved = calls(after, "Calls");
            assert_eq!(resolved.len(), 2, "{after:#?}");
            assert!(
                resolved.iter().all(|l| l.contains("Thing//lib/Lib.psm1:")),
                "callers re-resolve to the suffixed FQN: {resolved:#?}"
            );
        });
    }
}
