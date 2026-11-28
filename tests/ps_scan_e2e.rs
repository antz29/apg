//! `apg scan` runs over a PowerShell scratch repo, driven
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
        Parity::with_languages(tag, files, "ps")
    }

    /// [`Parity::new`] whose main full scan passes `--language <languages>`.
    fn with_languages(tag: &str, files: &[(&str, &str)], languages: &str) -> Parity {
        let p = Parity::scratch(tag, files);
        p.apg(&p.repo.root, &["init", "."]);
        p.repo.commit_all("apg init");
        p.apg(&p.repo.root, &["scan", ".", "--language", languages]);
        p.apg(&p.repo.root, &["project", "start", "demo"]);
        p
    }

    /// The bare fixture: the scratch repo with `files` committed and the
    /// isolated HOME — no `apg init`, scan or project yet.
    fn scratch(tag: &str, files: &[(&str, &str)]) -> Parity {
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
        Parity { repo, home, wt }
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

/// Every Struct-to-File Uses edge in `dir` as sorted `(struct, file,
/// properties)`; a non-string cell (NULL included) is kept as its JSON text.
fn struct_file_uses(dir: &Path) -> Vec<(String, String, String)> {
    let cypher = "MATCH (s:Struct)-[r:Uses]->(f:File) RETURN s.fqn, f.fqn, r.properties";
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(dir)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).expect("json rows");
    let cell = |r: &serde_json::Value, c: &str| match &r[c] {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    let mut edges: Vec<_> = rows
        .iter()
        .map(|r| (cell(r, "s.fqn"), cell(r, "f.fqn"), cell(r, "r.properties")))
        .collect();
    edges.sort();
    edges
}

/// Asserts `dir`'s Struct-to-File Uses are exactly main.ps1's script
/// Struct (ps-rooted, located in main.ps1) to the three unrooted
/// repo-relative dependency Files with their kinds; returns the edges.
fn assert_ps_file_edges(dir: &Path, when: &str) -> Vec<(String, String, String)> {
    let edges = struct_file_uses(dir);
    let main = fqns_in(dir, "main.ps1");
    let expected = [
        ("lib/util.ps1", "dot-source"),
        ("mods/Foo/Foo.psd1", "import-module"),
        ("mods/Foo/Foo.psm1", "import-module"),
    ];
    assert_eq!(
        edges.len(),
        3,
        "{when}: exactly three Struct-to-File uses: {edges:#?}"
    );
    for ((s, f, props), (file, kind)) in edges.iter().zip(expected) {
        assert!(
            s.starts_with("ps.") && main.contains(&format!("\"{s}\"")),
            "{when}: source is main.ps1's ps-rooted script Struct ({main:?}): {edges:#?}"
        );
        assert_eq!(
            f, file,
            "{when}: File target is unrooted repo-relative: {edges:#?}"
        );
        let props: serde_json::Value = serde_json::from_str(props)
            .unwrap_or_else(|e| panic!("{when}: properties {props:?} not JSON: {e}"));
        assert_eq!(
            props,
            serde_json::json!({ "kind": kind }),
            "{when}: {file} kind: {edges:#?}"
        );
    }
    edges
}

/// The single Struct FQN located in `path`.
fn only_struct_in(dir: &Path, path: &str) -> String {
    let rows = json_lines(
        dir,
        &format!("MATCH (n:Struct) WHERE n.path = '{path}' RETURN n.fqn AS a"),
    );
    assert_eq!(rows.len(), 1, "exactly one Struct in {path}: {rows:?}");
    serde_json::from_str(&rows[0]).expect("struct fqn")
}

/// [`struct_file_uses`] with each properties cell re-serialized as compact
/// JSON (a NULL stays `null`, non-JSON text stays raw), so it compares
/// against `json!` text.
fn file_edge_rows(dir: &Path) -> Vec<(String, String, String)> {
    struct_file_uses(dir)
        .into_iter()
        .map(|(s, f, p)| {
            let p = serde_json::from_str::<serde_json::Value>(&p)
                .map(|v| v.to_string())
                .unwrap_or(p);
            (s, f, p)
        })
        .collect()
}

mod e2e {
    use super::*;

    /// ps-file-dependency-cached-assembly: main.ps1's dot-source and
    /// import-module Struct-to-File Uses (beside PROGA's COBOL copy edge) hold
    /// with their kinds through every assembly of an auto-detected scan: the
    /// cold full scan, the warm-cache path (db.lbug deleted, every file from
    /// cached units, zero frontends), the stale-socket full-load fallback
    /// (`ingest_with_reuse`, ps files reused) and a `--force` rebuild. The
    /// whole `apg/` layout is ignored and untracked so every tracked path is
    /// a located File with a fact unit (the warm path needs one per entry).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/psfrontend/cobolfrontend/structfrontend, db.lbug); run via cargo test-e2e"]
    fn file_dependency_edges_survive_the_cached_fact_assembly() {
        const PROGA: &str = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. PROGA.\n       PROCEDURE DIVISION.\n       MAIN-PARA.\n       COPY STMTS.\n           STOP RUN.\n";
        let p = Parity::scratch(
            "ps-cached-assembly",
            &[
                (
                    "main.ps1",
                    ". $PSScriptRoot\\lib\\util.ps1\nImport-Module ./mods/Foo/Foo.psm1\n",
                ),
                ("lib/util.ps1", "function Get-Util {\n    'util'\n}\n"),
                ("mods/Foo/Foo.psm1", "function Get-Foo {\n    'foo'\n}\n"),
                ("PROGA.cbl", PROGA),
                ("lib/STMTS.cpy", "           DISPLAY 'S'.\n"),
            ],
        );
        let root = &p.repo.root;
        p.apg(root, &["init", "."]);
        let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        std::fs::write(root.join(".gitignore"), format!("{gitignore}/apg/\n")).unwrap();
        let git = common::git2_repo(&p.repo);
        let mut index = git.index().unwrap();
        index
            .remove_path(Path::new("apg/config.json"))
            .expect("untrack apg/config.json");
        index.write().unwrap();
        p.repo.commit_all("ignore apg/");
        assert!(p.repo.is_clean(), "the fixture tree must be clean");
        let scan = |extra: &[&str]| {
            let mut args = vec!["scan", ".", "--copy-path", "lib"];
            args.extend_from_slice(extra);
            p.apg(root, &args)
        };

        // (1) Cold full scan.
        let log = scan(&[]);
        assert!(
            log.contains("[scan] running ps frontend")
                && log.contains("[scan] running cobol frontend"),
            "(1) auto-detect must run ps and cobol: {log}"
        );
        let main = only_struct_in(root, "main.ps1");
        let prog = only_struct_in(root, "PROGA.cbl");
        assert!(
            main.starts_with("ps.") && prog.starts_with("cobol."),
            "{main} {prog}"
        );
        let kind = |k: &str| serde_json::json!({ "kind": k }).to_string();
        let mut expected = vec![
            (main.clone(), "lib/util.ps1".to_string(), kind("dot-source")),
            (main, "mods/Foo/Foo.psm1".to_string(), kind("import-module")),
            (prog, "lib/STMTS.cpy".to_string(), kind("copy")),
        ];
        expected.sort();
        assert_eq!(file_edge_rows(root), expected, "(1) cold full scan");

        // (2) Warm cache: the derived index is gone, the recorded scan at
        // HEAD assembles every file from cached units with zero frontends.
        let trans = p.repo.apg_root().join(".trans");
        for name in ["db.lbug", "db.lbug.wal", "db.lbug.shm"] {
            let _ = std::fs::remove_file(trans.join(name));
        }
        let log = scan(&[]);
        assert!(
            log.contains("[scan] warm cache: recorded scan at HEAD"),
            "(2) must take the warm-cache path: {log}"
        );
        assert!(
            !log.contains("[scan] running "),
            "(2) the warm path runs zero frontends: {log}"
        );
        assert_eq!(file_edge_rows(root), expected, "(2) warm cache");

        // (3) Full-load fallback: a COBOL-only edit (ps files reused) under a
        // planted stale session socket re-assembles through ingest_with_reuse.
        std::fs::write(
            root.join("PROGA.cbl"),
            PROGA.replacen("\n", "\n      * edited\n", 1),
        )
        .unwrap();
        p.repo.commit_all("edit PROGA");
        let socket = apg::session::socket_path(&p.repo.apg_root());
        std::fs::write(&socket, b"stale-session").unwrap();
        let log = scan(&[]);
        let _ = std::fs::remove_file(&socket);
        for line in [
            "[scan] incremental: ",
            "[load] full-load fallback: re-assembling the full graph",
        ] {
            assert!(log.contains(line), "(3) log must contain `{line}`: {log}");
        }
        assert!(
            !log.contains("[scan] running ps frontend"),
            "(3) no ps file is targeted, so ps is reused: {log}"
        );
        assert_eq!(file_edge_rows(root), expected, "(3) full-load fallback");

        // (4) A --force rebuild.
        scan(&["--force"]);
        assert_eq!(file_edge_rows(root), expected, "(4) --force rebuild");
    }

    /// ps-file-dependency-ingest: main.ps1 dot-sources lib/util.ps1 and
    /// imports mods/Foo/Foo.psm1 by path and `Foo` by name (manifest
    /// Foo.psd1). Its script Struct's three Struct-to-File Uses (unrooted
    /// File targets, kinds carried) hold after the main full scan, in the
    /// branch seed, after a spliced ps change, after a spliced md-only change
    /// (unchanged), and the spliced ps graph equals a `--force` rebuild.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/psfrontend, db.lbug); run via cargo test-e2e"]
    fn file_dependency_edges_survive_full_and_spliced_builds() {
        let p = Parity::with_languages(
            "ps-file-deps",
            &[
                (
                    "main.ps1",
                    ". $PSScriptRoot\\lib\\util.ps1\nImport-Module ./mods/Foo/Foo.psm1\nImport-Module Foo\n",
                ),
                ("lib/util.ps1", "function Get-Util {\n    'util'\n}\n"),
                ("mods/Foo/Foo.psm1", "function Get-Foo {\n    'foo'\n}\n"),
                ("mods/Foo/Foo.psd1", "@{\n    RootModule = 'Foo.psm1'\n}\n"),
                ("README.md", "# scratch\n"),
            ],
            "ps,md",
        );
        assert_ps_file_edges(&p.repo.root, "main full scan");
        assert_ps_file_edges(&p.wt, "branch seed");
        let scan = |args: &[&str]| {
            let mut full = vec!["scan", ".", "--language", "ps,md"];
            full.extend_from_slice(args);
            p.apg(&p.wt, &full)
        };
        let edit = |rel: &str, text: &str| {
            std::fs::write(p.wt.join(rel), text).unwrap();
            wt_commit(&p.wt, &[rel], "file-deps edit");
        };

        // (1) A ps change: every ps file re-emitted, spliced.
        edit("lib/util.ps1", "function Get-Util {\n    'util2'\n}\n");
        let log = scan(&[]);
        assert!(spliced(&log), "ps change must splice: {log}");
        let after_ps = assert_ps_file_edges(&p.wt, "after the ps change");

        // (2) An md-only change: every ps file reused, spliced, unchanged.
        edit("README.md", "# scratch\n\nmore\n");
        let log = scan(&[]);
        assert!(spliced(&log), "md change must splice: {log}");
        let after_md = assert_ps_file_edges(&p.wt, "after the md change");
        assert_eq!(
            after_ps, after_md,
            "an md-only change leaves the edges unchanged"
        );
        let spliced_graph = ps_graph(&p.wt);

        // (3) A full rebuild reproduces the spliced ps graph.
        let log = scan(&["--force"]);
        assert!(!spliced(&log), "--force must full-rebuild: {log}");
        assert_ps_file_edges(&p.wt, "after --force");
        assert_eq!(
            spliced_graph,
            ps_graph(&p.wt),
            "spliced (left) ps graph must equal the --force rebuild (right)"
        );
    }

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

    /// (c) The unchanged `app/a.ps1` calls `Get-Thing`, which no file
    /// declares (an unresolved call, no edge to any other file). Adding the
    /// manifest module `one/` exporting `Get-Thing` resolves the call; adding
    /// a second exporter `two/` makes it ambiguous (an `unknown` unresolved
    /// call). Each spliced ps graph equals `--force`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/psfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_new_exporter_resolves_unchanged_caller() {
        let p = Parity::new("ps-parity-export", &[("app/a.ps1", "Get-Thing\n")]);
        let srcs = fqns_in(&p.wt, "app/a.ps1");
        let thing = |g: &[String]| -> Vec<String> {
            calls_from(g, &srcs)
                .into_iter()
                .filter(|l| l.contains("Get-Thing"))
                .collect()
        };
        let psd1 = |m: &str| {
            format!("@{{\n    RootModule = '{m}.psm1'\n    FunctionsToExport = 'Get-Thing'\n}}\n")
        };
        let psm1 = "function Get-Thing {\n    'thing'\n}\n";

        // (1) A new exporter: the unresolved call becomes a Calls edge into one/.
        let one = psd1("one");
        p.step(
            &[("one/one.psd1", &one), ("one/one.psm1", psm1)],
            |before, after| {
                let (b, a) = (thing(before), thing(after));
                assert_ne!(b, a, "the unchanged caller's Get-Thing edge must change");
                assert!(
                    b.len() == 1 && b[0].starts_with("UnresolvedCall "),
                    "Get-Thing starts unresolved: {before:#?}"
                );
                assert!(
                    before
                        .iter()
                        .any(|l| l.starts_with("UnresolvedTarget \"Get-Thing\" \"external\"")),
                    "the undeclared Get-Thing is an external target: {before:#?}"
                );
                let target = fqns_in(&p.wt, "one/one.psm1");
                assert!(
                    a.len() == 1
                        && a[0].starts_with("Calls ")
                        && target.iter().any(|t| a[0].contains(&format!(" {t} "))),
                    "Get-Thing resolves to one/'s function {target:?}: {after:#?}"
                );
            },
        );

        // (2) A second exporter: the call becomes ambiguous (unknown).
        let two = psd1("two");
        p.step(
            &[("two/two.psd1", &two), ("two/two.psm1", psm1)],
            |before, after| {
                let (b, a) = (thing(before), thing(after));
                assert_ne!(b, a, "the unchanged caller's Get-Thing edge must change");
                assert!(
                    a.len() == 1 && a[0].starts_with("UnresolvedCall "),
                    "Get-Thing turns unresolved: {after:#?}"
                );
                assert!(
                    after
                        .iter()
                        .any(|l| l.starts_with("UnresolvedTarget \"Get-Thing\" \"unknown\"")),
                    "the ambiguous Get-Thing is an unknown target: {after:#?}"
                );
            },
        );
    }

    /// (d) `f/b.ps1.ps1` declares `class C`, colliding with function `ps1.C`
    /// of the unchanged `f/b.ps1` (called there by `Use`), which carries its
    /// `//f/b.ps1:<n>` suffix. Removing the class drops the survivor's
    /// suffix and `Use`'s call follows. The spliced ps graph equals `--force`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/psfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_removed_collision_drops_suffix() {
        let second = "f/b.ps1";
        let p = Parity::new(
            "ps-parity-unsuffix",
            &[
                (
                    second,
                    "function ps1.C {\n    'c'\n}\nfunction Use {\n    ps1.C\n}\n",
                ),
                ("f/b.ps1.ps1", "function Keep {\n    'k'\n}\nclass C {\n}\n"),
            ],
        );
        let suffix = format!("//{second}");
        let old = fqns_in(&p.wt, second);
        assert!(
            old.iter().any(|f| f.contains(&suffix)),
            "{second}'s ps1.C starts suffixed: {old:?}"
        );
        p.step(
            &[("f/b.ps1.ps1", "function Keep {\n    'k'\n}\n")],
            |before, after| {
                let new = fqns_in(&p.wt, second);
                assert!(
                    !new.iter().any(|f| f.contains("//")),
                    "{second}'s survivor drops its suffix: {new:?}"
                );
                let survivor = new
                    .iter()
                    .find(|f| f.ends_with("ps1.C\""))
                    .unwrap_or_else(|| panic!("survivor ps1.C in {second}: {new:?}"));
                let into = |g: &[String], to: &str| {
                    g.iter()
                        .filter(|l| l.starts_with("Calls ") && l.contains(&format!(" {to} ")))
                        .count()
                };
                let old_c = old.iter().find(|f| f.contains(&suffix)).unwrap();
                assert_eq!(
                    into(before, old_c),
                    1,
                    "Use called the suffixed FQN: {before:#?}"
                );
                assert_eq!(into(after, survivor), 1, "Use's call follows: {after:#?}");
            },
        );
    }
}
