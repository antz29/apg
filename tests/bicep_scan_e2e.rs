//! Phase-03 task-13 (e2e): `apg scan` over Bicep scratch repos, driven through
//! the CANDIDATE binary — never the apg repo. Every fixture uses Azure
//! (`Microsoft.Storage`) naming. Every test is `#[ignore]`d and runs through
//! `cargo test-e2e`; the staged `bicepfrontend` is required (a missing
//! frontend fails the scan, as the terraform e2e does).

mod common;

use common::{ApgCommand, Repo, wt_commit};
use std::path::{Path, PathBuf};
use std::process::Output;

/// The language id every Bicep scan drives (`--language bicep`).
const LANG: &str = "bicep";

/// The layout config the fixture commits: the test/generated/external globs
/// the Bicep paths need. A present `apg/config.json` replaces the built-in
/// code-type defaults, so the globs carry the types.
const CONFIG_JSON: &str = concat!(
    "{\"default\":\"src\",\"types\":[",
    "{\"name\":\"test\",\"globs\":[\"**/tests/**\"]},",
    "{\"name\":\"generated\",\"globs\":[\"**/gen/**\"]},",
    "{\"name\":\"external\",\"globs\":[\"**/vendor/**\"]}",
    "],\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\"}\n",
);

/// The claimed Bicep files of [`bicep_fixture`], repo-relative.
const BICEP_FILES: &[&str] = &[
    "main.bicep",
    "net.bicep",
    "shared/store.bicep",
    "params/dev.bicepparam",
    "legacy.bicep",
    "broken.bicep",
    "tests/smoke.bicep",
    "gen/gen.bicep",
    "vendor/v.bicep",
];

const MAIN_BICEP: &str = r#"import {skuName} from './shared/store.bicep'
param location string = 'westeurope'
param sku skuName = 'Standard_LRS'
func prefixed(name string) string => '${name}stg'
func storage(name string) string => prefixed(name)
resource stg 'Microsoft.Storage/storageAccounts@2023-01-01' = {
  name: storage('demo')
  location: location
  sku: {
    name: sku
  }
  kind: 'StorageV2'
}
module net './net.bicep' = {
  name: 'net'
  params: {
    location: location
  }
}
output id string = stg.id
"#;

const NET_BICEP: &str = r#"param location string
resource vnet 'Microsoft.Network/virtualNetworks@2023-04-01' = {
  name: 'vnet'
  location: location
}
"#;

const STORE_BICEP: &str = r#"type skuName = 'Standard_LRS' | 'Standard_GRS'
resource store 'Microsoft.Storage/storageAccounts@2023-01-01' = {
  name: 'store'
  location: 'westeurope'
  kind: 'StorageV2'
}
"#;

const PARAMS_DEV: &str = r#"using '../main.bicep'
param location = 'westeurope'
param sku = 'Standard_LRS'
"#;

const LEGACY_BICEP: &str = r#"module arm './arm/template.json' = {
  name: 'arm'
}
"#;

const ARM_JSON: &str = "{\"resources\": []}\n";

const SMOKE_BICEP: &str = r#"resource smoke 'Microsoft.Storage/storageAccounts@2023-01-01' = {
  name: 'smoke'
}
"#;

const GEN_BICEP: &str = r#"resource g 'Microsoft.Network/virtualNetworks@2023-04-01' = {
  name: 'g'
}
"#;

const VENDOR_BICEP: &str = r#"resource v 'Microsoft.Network/virtualNetworks@2023-04-01' = {
  name: 'v'
}
"#;

/// A syntax error: the resource body is never closed.
const BROKEN_BICEP: &str =
    "resource broken 'Microsoft.Storage/storageAccounts@2023-01-01' = {\n  name: 'x'\n";

/// The root-and-subdirectory Azure Bicep fixture: an import, a user-defined
/// type, funcs with a call, a local and an ARM `.json` module target, a
/// `.bicepparam` `using`, a syntax error, and `tests`/`gen`/`vendor` directories.
fn bicep_fixture() -> Vec<(&'static str, &'static str)> {
    vec![
        ("main.bicep", MAIN_BICEP),
        ("net.bicep", NET_BICEP),
        ("shared/store.bicep", STORE_BICEP),
        ("params/dev.bicepparam", PARAMS_DEV),
        ("legacy.bicep", LEGACY_BICEP),
        ("arm/template.json", ARM_JSON),
        ("broken.bicep", BROKEN_BICEP),
        ("tests/smoke.bicep", SMOKE_BICEP),
        ("gen/gen.bicep", GEN_BICEP),
        ("vendor/v.bicep", VENDOR_BICEP),
        ("apg/config.json", CONFIG_JSON),
        ("README.md", "# scratch\n"),
    ]
}

/// Every expected Bicep Struct as `(fqn, repo-relative path)`.
const EXPECTED_STRUCTS: &[(&str, &str)] = &[
    ("bicep.main.bicep.import.skuName", "main.bicep"),
    ("bicep.main.bicep.param.location", "main.bicep"),
    ("bicep.main.bicep.param.sku", "main.bicep"),
    ("bicep.main.bicep.resource.stg", "main.bicep"),
    ("bicep.main.bicep.module.net", "main.bicep"),
    ("bicep.main.bicep.output.id", "main.bicep"),
    ("bicep.net.bicep.param.location", "net.bicep"),
    ("bicep.net.bicep.resource.vnet", "net.bicep"),
    (
        "bicep.shared/store.bicep.type.skuName",
        "shared/store.bicep",
    ),
    (
        "bicep.shared/store.bicep.resource.store",
        "shared/store.bicep",
    ),
    ("bicep.params/dev.bicepparam.using", "params/dev.bicepparam"),
    (
        "bicep.params/dev.bicepparam.param.location",
        "params/dev.bicepparam",
    ),
    (
        "bicep.params/dev.bicepparam.param.sku",
        "params/dev.bicepparam",
    ),
    ("bicep.legacy.bicep.module.arm", "legacy.bicep"),
    (
        "bicep.tests/smoke.bicep.resource.smoke",
        "tests/smoke.bicep",
    ),
    ("bicep.gen/gen.bicep.resource.g", "gen/gen.bicep"),
    ("bicep.vendor/v.bicep.resource.v", "vendor/v.bicep"),
];

/// Every expected Bicep Function as `(fqn, repo-relative path)`.
const EXPECTED_FUNCTIONS: &[(&str, &str)] = &[
    ("bicep.main.bicep.func.prefixed", "main.bicep"),
    ("bicep.main.bicep.func.storage", "main.bicep"),
];

/// Every `(module, file)` Contains pair the fixture must produce: a Bicep
/// file's module is named by its repo-relative path.
const MODULE_FILES: &[(&str, &str)] = &[
    ("bicep.main.bicep", "main.bicep"),
    ("bicep.net.bicep", "net.bicep"),
    ("bicep.shared/store.bicep", "shared/store.bicep"),
    ("bicep.params/dev.bicepparam", "params/dev.bicepparam"),
    ("bicep.legacy.bicep", "legacy.bicep"),
    ("bicep.broken.bicep", "broken.bicep"),
    ("bicep.tests/smoke.bicep", "tests/smoke.bicep"),
    ("bicep.gen/gen.bicep", "gen/gen.bicep"),
    ("bicep.vendor/v.bicep", "vendor/v.bicep"),
];

/// Every Struct-to-Struct `uses` the fixture must produce: the import binds
/// the exported type, a param's type names the import, resources and the
/// module read params, and the output reads the resource.
const USES: &[(&str, &str)] = &[
    (
        "bicep.main.bicep.import.skuName",
        "bicep.shared/store.bicep.type.skuName",
    ),
    (
        "bicep.main.bicep.param.sku",
        "bicep.main.bicep.import.skuName",
    ),
    (
        "bicep.main.bicep.resource.stg",
        "bicep.main.bicep.param.location",
    ),
    (
        "bicep.main.bicep.resource.stg",
        "bicep.main.bicep.param.sku",
    ),
    (
        "bicep.main.bicep.module.net",
        "bicep.main.bicep.param.location",
    ),
    (
        "bicep.main.bicep.output.id",
        "bicep.main.bicep.resource.stg",
    ),
    (
        "bicep.net.bicep.resource.vnet",
        "bicep.net.bicep.param.location",
    ),
];

/// Every Function-to-Function `calls` the fixture must produce.
const CALLS: &[(&str, &str)] = &[(
    "bicep.main.bicep.func.storage",
    "bicep.main.bicep.func.prefixed",
)];

/// The `code_type` of each Struct the fixture pins (a `tests/` Struct is
/// `test`, a `gen/` one `generated`, a `vendor/` one `external`).
const CODE_TYPES: &[(&str, &str)] = &[
    ("bicep.main.bicep.resource.stg", "src"),
    ("bicep.tests/smoke.bicep.resource.smoke", "test"),
    ("bicep.gen/gen.bicep.resource.g", "generated"),
    ("bicep.vendor/v.bicep.resource.v", "external"),
];

/// A scratch repo with `files` committed (no `apg init`, scan or project).
fn bicep_repo(tag: &str, files: &[(&str, &str)]) -> Repo {
    let repo = Repo::new(tag);
    for (rel, text) in files {
        repo.write(rel, text);
    }
    repo.commit_all("bicep fixture");
    repo
}

/// Runs `apg scan .` with `args` in `repo`; returns the output and its stderr
/// text (the `Languages:` line and the `[scan]`/`[load]` decisions).
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

/// Runs a read-only Cypher through `apg query --json` in `dir`.
fn rows(dir: &Path, cypher: &str) -> Vec<serde_json::Value> {
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(dir)
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json rows")
}

/// Each row as two string columns `(a, b)`.
fn pairs(dir: &Path, cypher: &str, a: &str, b: &str) -> Vec<(String, String)> {
    let cell = |r: &serde_json::Value, c: &str| r[c].as_str().unwrap_or_default().to_string();
    rows(dir, cypher)
        .iter()
        .map(|r| (cell(r, a), cell(r, b)))
        .collect()
}

/// Each row as its columns joined with spaces, JSON-encoded, for the parity
/// graph comparisons.
fn json_lines(dir: &Path, cypher: &str) -> Vec<String> {
    rows(dir, cypher)
        .iter()
        .map(|r| {
            r.as_object()
                .expect("row object")
                .values()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Every Struct-to-File Uses edge as sorted `(struct, file, compact-JSON
/// properties)`.
fn file_edges(dir: &Path) -> Vec<(String, String, String)> {
    let mut edges: Vec<_> = rows(
        dir,
        "MATCH (s:Struct)-[r:Uses]->(f:File) RETURN s.fqn, f.fqn, r.properties",
    )
    .iter()
    .map(|r| {
        let props = match &r["r.properties"] {
            serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
                .map(|v| v.to_string())
                .unwrap_or_else(|_| s.clone()),
            v => v.to_string(),
        };
        (
            r["s.fqn"].as_str().unwrap_or_default().to_string(),
            r["f.fqn"].as_str().unwrap_or_default().to_string(),
            props,
        )
    })
    .collect();
    edges.sort();
    edges
}

/// The Struct-to-File Uses the fixture must produce: the import's module file,
/// the root `module net` target, the `.bicepparam` `using` of `main.bicep`
/// (`../` normalised against `params/`), and the ARM `.json` module target —
/// its File comes from the always-on structural json stream, so it is present
/// whatever `--language` selects.
fn expected_file_edges() -> Vec<(String, String, String)> {
    let e =
        |s: &str, f: &str, k: &str| (s.to_string(), f.to_string(), format!(r#"{{"kind":"{k}"}}"#));
    let mut edges = vec![
        e(
            "bicep.main.bicep.import.skuName",
            "shared/store.bicep",
            "import",
        ),
        e("bicep.main.bicep.module.net", "net.bicep", "module"),
        e("bicep.params/dev.bicepparam.using", "main.bicep", "using"),
        e(
            "bicep.legacy.bicep.module.arm",
            "arm/template.json",
            "module",
        ),
    ];
    edges.sort();
    edges
}

/// True only when the scan's `[load]` line ends in `full load skipped`.
fn spliced(log: &str) -> bool {
    log.lines()
        .any(|l| l.contains("[load]") && l.trim_end().ends_with("full load skipped"))
}

/// A `.bicep`/`.bicepparam` File test on `v`, in any case.
fn bicep_file(v: &str) -> String {
    format!("(lower({v}.fqn) ENDS WITH '.bicep' OR lower({v}.fqn) ENDS WITH '.bicepparam')")
}

/// The whole Bicep graph as sorted lines: `bicep.` Module/Struct/Function
/// nodes, Bicep Files, the `bicep` Language, and every Contains/Calls/Uses/
/// UnresolvedUse edge touching that set.
fn bicep_graph(dir: &Path) -> Vec<String> {
    let from = |rel: &str, v: &str| {
        format!("EXISTS {{ MATCH (p)-[:{rel}]->({v}) WHERE p.fqn STARTS WITH 'bicep.' }}")
    };
    let in_set = |v: &str| {
        format!(
            "((label({v}) IN ['Module','Struct','Function'] AND {v}.fqn STARTS WITH 'bicep.')
              OR (label({v}) = 'File' AND {})
              OR (label({v}) = 'Language' AND {v}.fqn = 'bicep')
              OR (label({v}) = 'UnresolvedTarget' AND {}))",
            bicep_file(v),
            from("UnresolvedUse", v)
        )
    };
    let mut queries = vec![
        (
            "Module",
            "MATCH (n:Module) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn AS a".to_string(),
        ),
        (
            "Struct",
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c".to_string(),
        ),
        (
            "Function",
            "MATCH (n:Function) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c".to_string(),
        ),
        (
            "File",
            format!(
                "MATCH (n:File) WHERE {} RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c",
                bicep_file("n")
            ),
        ),
        (
            "Language",
            "MATCH (n:Language) WHERE n.fqn = 'bicep' RETURN n.fqn AS a".to_string(),
        ),
    ];
    for (rel, prop) in [
        ("Contains", "''"),
        ("Calls", "''"),
        ("Uses", "r.properties"),
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

/// The full-vs-incremental parity fixture: a scratch repo with `apg init .`
/// committed, a full main `apg scan . --language bicep`, and project `demo`
/// started; every later scan runs in the `demo` worktree.
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
        repo.commit_all("bicep fixture");
        let wt = repo.root.join("apg").join(".worktrees").join("demo");
        let p = Parity { repo, home, wt };
        p.apg(&p.repo.root, &["init", "."]);
        p.repo.commit_all("apg init");
        p.apg(&p.repo.root, &["scan", ".", "--language", LANG]);
        p.apg(&p.repo.root, &["project", "start", "demo"]);
        p
    }

    /// One `apg` run in `dir` with the isolated HOME; fails on a non-zero exit
    /// and returns the stderr log.
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
    /// incremental bicep scan (it must splice), check
    /// `precondition(before, after)` on the bicep graphs, then a `--force`
    /// rebuild must reproduce the incremental graph.
    fn step(&self, files: &[(&str, &str)], precondition: impl FnOnce(&[String], &[String])) {
        let before = bicep_graph(&self.wt);
        for (rel, text) in files {
            let p = self.wt.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let rels: Vec<&str> = files.iter().map(|(r, _)| *r).collect();
        wt_commit(&self.wt, &rels, "parity edit");
        let log = self.apg(&self.wt, &["scan", ".", "--language", LANG]);
        assert!(spliced(&log), "incremental scan must splice: {log}");
        let incremental = bicep_graph(&self.wt);
        precondition(&before, &incremental);
        let log = self.apg(&self.wt, &["scan", ".", "--language", LANG, "--force"]);
        assert!(!spliced(&log), "--force must full-rebuild: {log}");
        assert_eq!(
            incremental,
            bicep_graph(&self.wt),
            "incremental (left) bicep graph must equal the --force rebuild (right) ({rels:?})"
        );
    }
}

/// Whether `graph` holds a line starting with `prefix`.
fn has(graph: &[String], prefix: &str) -> bool {
    graph.iter().any(|l| l.starts_with(prefix))
}

mod e2e {
    use super::*;

    /// `--language bicep` selects the frontend, every Bicep file is a module
    /// named by its repo-relative path (contained by the `bicep` Language),
    /// the expected Structs and Functions exist at their files, the import,
    /// param, resource, module and output references draw their uses edges,
    /// the func call draws its calls edge, the module/using file edges hold,
    /// and `code_type` classifies test, generated and external files.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn language_flag_selects_bicep_and_renders_the_fixture_graph() {
        let repo = bicep_repo("bicep-lang", &bicep_fixture());
        let (out, log) = scan(&repo, &["--language", LANG]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&LANG.to_string()),
            "Languages line must list bicep: {log}"
        );
        let root = &repo.root;

        let roots = pairs(
            root,
            "MATCH (l:Language)-[:Contains]->(m:Module) WHERE m.fqn = 'bicep.main.bicep' RETURN l.fqn, m.fqn",
            "l.fqn",
            "m.fqn",
        );
        assert_eq!(
            roots,
            vec![("bicep".to_string(), "bicep.main.bicep".to_string())],
            "the bicep Language contains the main.bicep module"
        );

        let modules = pairs(
            root,
            "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn, f.fqn",
            "m.fqn",
            "f.fqn",
        );
        for (m, f) in MODULE_FILES {
            assert!(
                modules
                    .iter()
                    .any(|(a, b)| (a.as_str(), b.as_str()) == (*m, *f)),
                "missing module file {m} -> {f}: {modules:#?}"
            );
        }

        let structs = pairs(
            root,
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn, n.path",
            "n.fqn",
            "n.path",
        );
        for (fqn, path) in EXPECTED_STRUCTS {
            assert!(
                structs
                    .iter()
                    .any(|(f, p)| (f.as_str(), p.as_str()) == (*fqn, *path)),
                "missing Struct {fqn} in {path}: {structs:#?}"
            );
        }

        let functions = pairs(
            root,
            "MATCH (n:Function) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn, n.path",
            "n.fqn",
            "n.path",
        );
        for (fqn, path) in EXPECTED_FUNCTIONS {
            assert!(
                functions
                    .iter()
                    .any(|(f, p)| (f.as_str(), p.as_str()) == (*fqn, *path)),
                "missing Function {fqn} in {path}: {functions:#?}"
            );
        }

        let uses = pairs(
            root,
            "MATCH (s:Struct)-[:Uses]->(t:Struct) WHERE s.fqn STARTS WITH 'bicep.' RETURN s.fqn, t.fqn",
            "s.fqn",
            "t.fqn",
        );
        for (s, t) in USES {
            assert!(
                uses.iter()
                    .any(|(a, b)| (a.as_str(), b.as_str()) == (*s, *t)),
                "missing Uses {s} -> {t}: {uses:#?}"
            );
        }

        let calls = pairs(
            root,
            "MATCH (s:Function)-[:Calls]->(t:Function) WHERE s.fqn STARTS WITH 'bicep.' RETURN s.fqn, t.fqn",
            "s.fqn",
            "t.fqn",
        );
        for (s, t) in CALLS {
            assert!(
                calls
                    .iter()
                    .any(|(a, b)| (a.as_str(), b.as_str()) == (*s, *t)),
                "missing Calls {s} -> {t}: {calls:#?}"
            );
        }

        assert_eq!(
            file_edges(root),
            expected_file_edges(),
            "Struct-to-File uses in the full scan"
        );
        assert!(
            modules
                .iter()
                .any(|(m, x)| x == "arm/template.json" && m.starts_with("json.")),
            "the ARM template is a structural json File, not a bicep one: {modules:#?}"
        );

        let typed = pairs(
            root,
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn, n.code_type",
            "n.fqn",
            "n.code_type",
        );
        for (fqn, t) in CODE_TYPES {
            assert!(
                typed
                    .iter()
                    .any(|(a, b)| (a.as_str(), b.as_str()) == (*fqn, *t)),
                "{fqn} must be code_type {t}: {typed:#?}"
            );
        }
    }

    /// Auto-detection (no `--language`) selects bicep, every Bicep file is
    /// claimed by the bicep stream alone (one `bicep.` module, never misc or
    /// md), and README.md stays in md.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn auto_detect_claims_each_bicep_file_once_and_routes_the_rest() {
        let repo = bicep_repo("bicep-auto", &bicep_fixture());
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&LANG.to_string()),
            "auto-detection must select bicep: {log}"
        );
        let modules = pairs(
            &repo.root,
            "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn, f.fqn",
            "m.fqn",
            "f.fqn",
        );
        for f in BICEP_FILES {
            let mods: Vec<&String> = modules
                .iter()
                .filter(|(_, x)| x == f)
                .map(|(m, _)| m)
                .collect();
            assert_eq!(mods.len(), 1, "{f} has exactly one Module: {mods:?}");
            assert!(
                mods[0].starts_with("bicep."),
                "{f} must sit under a bicep. module, not {}",
                mods[0]
            );
        }
        assert!(
            modules
                .iter()
                .any(|(m, x)| x == "README.md" && m.starts_with("md.")),
            "README.md stays under md.: {modules:#?}"
        );
    }

    /// `--language bicep,md` merges two streams into one database: both
    /// `Language` roots exist and the bicep Module and its Struct are intact
    /// (no id collision drops either stream).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn merged_scan_keeps_bicep_ids_distinct_from_other_streams() {
        let repo = bicep_repo(
            "bicep-prefix",
            &[
                (
                    "main.bicep",
                    "resource y 'Microsoft.Storage/storageAccounts@2023-01-01' = {\n  name: 'y'\n}\n",
                ),
                ("README.md", "# scratch\n"),
            ],
        );
        let (out, log) = scan(&repo, &["--language", "bicep,md"]);
        assert!(out.status.success(), "scan: {log}");
        let langs = languages(&log);
        assert!(
            langs.contains(&"bicep".to_string()) && langs.contains(&"md".to_string()),
            "both streams listed: {langs:?}"
        );
        let roots = pairs(
            &repo.root,
            "MATCH (l:Language)-[:Contains]->(m:Module) RETURN l.fqn, m.fqn",
            "l.fqn",
            "m.fqn",
        );
        assert!(
            roots.contains(&("bicep".to_string(), "bicep.main.bicep".to_string())),
            "{roots:#?}"
        );
        assert!(
            roots.iter().any(|(l, m)| l == "md" && m.starts_with("md.")),
            "{roots:#?}"
        );
        let structs = pairs(
            &repo.root,
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'bicep.' RETURN n.fqn, n.path",
            "n.fqn",
            "n.path",
        );
        assert!(
            structs.contains(&(
                "bicep.main.bicep.resource.y".to_string(),
                "main.bicep".to_string()
            )),
            "the bicep stream survives the merge: {structs:#?}"
        );
    }

    /// A syntax-error file keeps its File and Module, and the rest of the
    /// scan is unaffected: `main.bicep`'s Structs and Uses still resolve.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn syntax_error_file_keeps_its_file_and_the_scan_continues() {
        let repo = bicep_repo("bicep-broken", &bicep_fixture());
        let (out, log) = scan(&repo, &["--language", LANG]);
        assert!(out.status.success(), "scan: {log}");
        let files = pairs(
            &repo.root,
            "MATCH (f:File) RETURN f.fqn, f.code_type",
            "f.fqn",
            "f.code_type",
        );
        assert!(
            files.iter().any(|(f, _)| f == "broken.bicep"),
            "the broken file keeps its File node: {files:#?}"
        );
        let uses = pairs(
            &repo.root,
            "MATCH (s:Struct)-[:Uses]->(t:Struct) WHERE s.fqn STARTS WITH 'bicep.' RETURN s.fqn, t.fqn",
            "s.fqn",
            "t.fqn",
        );
        assert!(
            uses.contains(&(
                "bicep.main.bicep.resource.stg".to_string(),
                "bicep.main.bicep.param.location".to_string()
            )),
            "main.bicep still resolves past the broken file: {uses:#?}"
        );
    }

    /// An ARM `.json` template named by a `module` statement: under
    /// auto-detection the structural json stream supplies its File, so the
    /// module Struct's file-dependency edge lands on `arm/template.json`.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn arm_json_module_target_is_a_file_edge_under_auto_detect() {
        let repo = bicep_repo("bicep-arm", &bicep_fixture());
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");
        let edges = file_edges(&repo.root);
        assert!(
            edges.contains(&(
                "bicep.legacy.bicep.module.arm".to_string(),
                "arm/template.json".to_string(),
                r#"{"kind":"module"}"#.to_string()
            )),
            "the ARM module target keeps its file edge: {edges:#?}"
        );
    }

    /// Dependent widening, the import target: `shared/store.bicep` renames its
    /// exported type, so `main.bicep`'s `import {skuName}` loses its symbol
    /// `uses` (main.bicep is not edited, its facts must re-emit); the import's
    /// file edge stays; the spliced graph equals a `--force` rebuild.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_import_retype_reresolves_dependent_import() {
        let p = Parity::new("bicep-parity-retype", &bicep_fixture());
        let edited = STORE_BICEP.replace("type skuName", "type skuTier");
        let uses =
            "Uses \"bicep.main.bicep.import.skuName\" \"bicep.shared/store.bicep.type.skuName\"";
        let file = "Uses \"bicep.main.bicep.import.skuName\" \"shared/store.bicep\"";
        p.step(&[("shared/store.bicep", &edited)], |before, after| {
            assert!(has(before, uses), "import resolved before: {before:#?}");
            assert!(!has(after, uses), "import edge gone after: {after:#?}");
            assert!(has(after, file), "import file edge kept: {after:#?}");
        });
    }

    /// Dependent widening, the module target: `net.bicep` is the target of
    /// `module net`. Adding a variable to it adds a Struct in that file, while
    /// the module's file edge and the rest of the file-dependency set stay
    /// exactly the same; the spliced graph equals a `--force` rebuild.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_module_target_edit_matches_force_rebuild() {
        let p = Parity::new("bicep-parity-module", &bicep_fixture());
        let edited = format!("{NET_BICEP}var extra = 'x'\n");
        p.step(&[("net.bicep", &edited)], |before, after| {
            let extra = "\"bicep.net.bicep.var.extra\"";
            assert!(!before.iter().any(|l| l.contains(extra)), "absent before");
            assert!(
                after.iter().any(|l| l.contains(extra)),
                "added after: {after:#?}"
            );
            assert_eq!(
                file_edges(&p.wt),
                expected_file_edges(),
                "module file edges unchanged"
            );
        });
    }

    /// The file-dependency edges (module, import, using) survive the full
    /// scan, the branch seed, a spliced edit of the root file, and a `--force`
    /// rebuild that reproduces the spliced graph.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/bicepfrontend, db.lbug); run via cargo test-e2e"]
    fn file_dependency_edges_survive_full_and_spliced_builds() {
        let p = Parity::new("bicep-file-deps", &bicep_fixture());
        let want = expected_file_edges();
        assert_eq!(file_edges(&p.repo.root), want, "main full scan");
        assert_eq!(file_edges(&p.wt), want, "branch seed");
        let touched = format!("{MAIN_BICEP}// touched\n");
        p.step(&[("main.bicep", &touched)], |_, _| {
            assert_eq!(file_edges(&p.wt), want, "after a spliced root edit");
        });
    }
}
