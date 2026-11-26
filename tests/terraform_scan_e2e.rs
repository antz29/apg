//! Phase-02 task-16 (e2e): `apg scan` over Terraform scratch repos, driven
//! through the CANDIDATE binary — never the apg repo. Every fixture uses Azure
//! (`azurerm`) naming. Every test is `#[ignore]`d and runs through
//! `cargo test-e2e`; the staged `terraformfrontend` is required (a missing
//! frontend fails the scan, as the ps e2e does).

mod common;

use common::{ApgCommand, Repo, wt_commit};
use std::path::{Path, PathBuf};
use std::process::Output;

/// The language id every Terraform scan drives (`--language terraform`).
const LANG: &str = "terraform";

/// The layout config the fixture commits: the test/generated/external globs
/// the Terraform paths need. A present `apg/config.json` replaces the built-in
/// code-type defaults (`classify_code_type`), so the globs carry the types.
const CONFIG_JSON: &str = concat!(
    "{\"default\":\"src\",\"types\":[",
    "{\"name\":\"test\",\"globs\":[\"**/tests/**\",\"**/*.tftest.hcl\",\"**/*.tofutest.hcl\"]},",
    "{\"name\":\"generated\",\"globs\":[\"**/gen/**\"]},",
    "{\"name\":\"external\",\"globs\":[\"**/vendor/**\",\"**/.terraform/**\"]}",
    "],\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\"}\n",
);

/// The claimed Terraform files of [`tf_fixture`], repo-relative.
const TF_FILES: &[&str] = &[
    "main.tf",
    "main_override.tf",
    "UPPER.TF",
    "shadow.tf",
    "shadow.tofu",
    "broken.tf",
    "terraform.tfvars",
    ".terraform.lock.hcl",
    "extra/config.tf.json",
    "net/vars.tf",
    "net/net.tf",
    "net/sub/main.tf",
    "gen/gen.tf",
    "vendor/v.tf",
    "tests/x.tftest.hcl",
    "tests/y.tofutest.hcl",
    "checks/z.tftest.hcl",
];

const MAIN_TF: &str = r#"variable "location" {}
variable "resource_group_name" {}
provider "azurerm" {
  features {}
  alias = "west"
}
provider "azurerm" {
  features {}
}
resource "azurerm_resource_group" "main" {
  name     = var.resource_group_name
  location = var.location
}
data "azurerm_resource_group" "existing" {
  name = "rg-existing"
}
output "rg_id" {
  value = azurerm_resource_group.main.id
}
locals {
  name = "web"
}
module "net" {
  source   = "./net"
  location = local.name
}
module "remote" {
  source = "Azure/avm-res-network-virtualnetwork/azurerm"
}
"#;

const MAIN_OVERRIDE_TF: &str = r#"resource "azurerm_resource_group" "main" {
  tags = local.name
}
"#;

const NET_TF: &str = r#"resource "azurerm_virtual_network" "main" {
  location = var.location
}
"#;

const LOCK_HCL: &str = r#"provider "registry.terraform.io/hashicorp/azurerm" {
  version = "4.0.0"
}
"#;

const RUN_HCL: &str = "run \"smoke\" {\n  command = plan\n}\n";

/// The root-and-subdirectory Azure Terraform fixture: every claimed suffix,
/// every block kind, a local and a remote module call, an override file, a
/// tfvars file, test files, a lock file, a `.tofu` shadow, a file that fails
/// to parse, `gen`/`vendor` directories, a plain `.hcl` and an unclaimed `.json`.
fn tf_fixture() -> Vec<(&'static str, &'static str)> {
    vec![
        ("main.tf", MAIN_TF),
        ("main_override.tf", MAIN_OVERRIDE_TF),
        (
            "UPPER.TF",
            "resource \"azurerm_resource_group\" \"upper\" {}\n",
        ),
        (
            "shadow.tf",
            "resource \"azurerm_storage_account\" \"shadowed\" {}\n",
        ),
        (
            "shadow.tofu",
            "resource \"azurerm_storage_account\" \"shadowed\" {}\n",
        ),
        ("broken.tf", "resource \"azurerm_x\" {\n"),
        ("terraform.tfvars", "location = \"westeurope\"\nbogus = 1\n"),
        (".terraform.lock.hcl", LOCK_HCL),
        (
            "extra/config.tf.json",
            "{\"resource\": {\"azurerm_storage_account\": {\"j\": {}}}}\n",
        ),
        ("net/vars.tf", "variable \"location\" {}\n"),
        ("net/net.tf", NET_TF),
        (
            "net/sub/main.tf",
            "module \"up\" {\n  source = \"../\"\n}\n",
        ),
        ("gen/gen.tf", "resource \"azurerm_subnet\" \"g\" {}\n"),
        ("vendor/v.tf", "resource \"azurerm_subnet\" \"v\" {}\n"),
        ("tests/x.tftest.hcl", RUN_HCL),
        ("tests/y.tofutest.hcl", RUN_HCL),
        ("apg/config.json", CONFIG_JSON),
        ("checks/z.tftest.hcl", RUN_HCL),
        (
            ".terraform/modules/m/m.tf",
            "resource \"azurerm_subnet\" \"m\" {}\n",
        ),
        ("notes/plain.hcl", "a = 1\n"),
        ("unclaimed.json", "{}\n"),
        ("README.md", "# scratch\n"),
    ]
}

/// Every expected Terraform Struct as `(fqn, repo-relative path)`.
const EXPECTED_STRUCTS: &[(&str, &str)] = &[
    ("terraform..var.location", "main.tf"),
    ("terraform..var.resource_group_name", "main.tf"),
    ("terraform..provider.azurerm.west", "main.tf"),
    ("terraform..azurerm_resource_group.main", "main.tf"),
    ("terraform..data.azurerm_resource_group.existing", "main.tf"),
    ("terraform..output.rg_id", "main.tf"),
    ("terraform..local.name", "main.tf"),
    ("terraform..module.net", "main.tf"),
    ("terraform..module.remote", "main.tf"),
    ("terraform..azurerm_resource_group.upper", "UPPER.TF"),
    ("terraform..azurerm_storage_account.shadowed", "shadow.tofu"),
    (
        "terraform..lock.registry.terraform.io/hashicorp/azurerm",
        ".terraform.lock.hcl",
    ),
    ("terraform..tfvars.terraform.tfvars", "terraform.tfvars"),
    ("terraform..test.tests/x.tftest.hcl", "tests/x.tftest.hcl"),
    (
        "terraform..test.tests/x.tftest.hcl.run.smoke",
        "tests/x.tftest.hcl",
    ),
    (
        "terraform..test.tests/y.tofutest.hcl",
        "tests/y.tofutest.hcl",
    ),
    (
        "terraform.extra.azurerm_storage_account.j",
        "extra/config.tf.json",
    ),
    ("terraform.net.var.location", "net/vars.tf"),
    ("terraform.net.azurerm_virtual_network.main", "net/net.tf"),
    ("terraform.net/sub.module.up", "net/sub/main.tf"),
    ("terraform.gen.azurerm_subnet.g", "gen/gen.tf"),
    ("terraform.vendor.azurerm_subnet.v", "vendor/v.tf"),
    ("terraform.checks.test.z.tftest.hcl", "checks/z.tftest.hcl"),
    (
        "terraform.checks.test.z.tftest.hcl.run.smoke",
        "checks/z.tftest.hcl",
    ),
];

/// Every `(module, file)` Contains pair the fixture must produce.
const MODULE_FILES: &[(&str, &str)] = &[
    ("terraform.", "main.tf"),
    ("terraform.", "main_override.tf"),
    ("terraform.", "tests/x.tftest.hcl"),
    ("terraform.", ".terraform.lock.hcl"),
    ("terraform.net", "net/net.tf"),
    ("terraform.net", "net/vars.tf"),
    ("terraform.net/sub", "net/sub/main.tf"),
    ("terraform.gen", "gen/gen.tf"),
    ("terraform.vendor", "vendor/v.tf"),
    ("terraform.checks", "checks/z.tftest.hcl"),
    ("terraform.extra", "extra/config.tf.json"),
];

/// The `code_type` of each Struct the fixture pins (a test file's Struct is
/// its `test.<path>`; `gen`/`vendor` blocks carry their directory's type).
const CODE_TYPES: &[(&str, &str)] = &[
    ("terraform..azurerm_resource_group.main", "src"),
    ("terraform..test.tests/x.tftest.hcl", "test"),
    ("terraform..test.tests/y.tofutest.hcl", "test"),
    ("terraform.checks.test.z.tftest.hcl", "test"),
    ("terraform.gen.azurerm_subnet.g", "generated"),
    ("terraform.vendor.azurerm_subnet.v", "external"),
];

/// A scratch repo with `files` committed (no `apg init`, scan or project).
fn tf_repo(tag: &str, files: &[(&str, &str)]) -> Repo {
    let repo = Repo::new(tag);
    for (rel, text) in files {
        repo.write(rel, text);
    }
    repo.commit_all("tf fixture");
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

/// The Struct-to-File Uses the fixture must produce: both `net` configuration
/// files from the root `module "net"` and from `net/sub`'s `../` module, and
/// the override edge of `azurerm_resource_group.main`.
fn expected_file_edges() -> Vec<(String, String, String)> {
    let e =
        |s: &str, f: &str, k: &str| (s.to_string(), f.to_string(), format!(r#"{{"kind":"{k}"}}"#));
    let mut edges = vec![
        e("terraform..module.net", "net/net.tf", "module-source"),
        e("terraform..module.net", "net/vars.tf", "module-source"),
        e("terraform.net/sub.module.up", "net/net.tf", "module-source"),
        e(
            "terraform.net/sub.module.up",
            "net/vars.tf",
            "module-source",
        ),
        e(
            "terraform..azurerm_resource_group.main",
            "main_override.tf",
            "override",
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

/// A `.tf`/`.tf.json`/`.tfvars`/`.tofu`/`.tftest.hcl`/`.tofutest.hcl`/
/// `.terraform.lock.hcl` File test on `v`, in any case.
fn tf_file(v: &str) -> String {
    let f = format!("lower({v}.fqn)");
    let exts = [
        ".tf",
        ".tf.json",
        ".tfvars",
        ".tfvars.json",
        ".tofu",
        ".tofu.json",
        ".tftest.hcl",
        ".tofutest.hcl",
        ".terraform.lock.hcl",
    ];
    let any: Vec<String> = exts
        .iter()
        .map(|e| format!("{f} ENDS WITH '{e}'"))
        .collect();
    format!("({})", any.join(" OR "))
}

/// The whole Terraform graph as sorted lines: `terraform.` Module/Struct/
/// Function nodes, Terraform Files, the `terraform` Language, the
/// UnresolvedTargets those reach, and every Contains/Calls/Uses/UnresolvedCall/
/// UnresolvedUse edge touching that set.
fn tf_graph(dir: &Path) -> Vec<String> {
    let from = |rel: &str, v: &str| {
        format!("EXISTS {{ MATCH (p)-[:{rel}]->({v}) WHERE p.fqn STARTS WITH 'terraform.' }}")
    };
    let unresolved = |v: &str| {
        format!(
            "({} OR {})",
            from("UnresolvedCall", v),
            from("UnresolvedUse", v)
        )
    };
    let in_set = |v: &str| {
        format!(
            "((label({v}) IN ['Module','Struct','Function'] AND {v}.fqn STARTS WITH 'terraform.')
              OR (label({v}) = 'File' AND {})
              OR (label({v}) = 'Language' AND {v}.fqn = 'terraform')
              OR (label({v}) = 'UnresolvedTarget' AND {}))",
            tf_file(v),
            unresolved(v)
        )
    };
    let mut queries = vec![
        (
            "Module",
            "MATCH (n:Module) WHERE n.fqn STARTS WITH 'terraform.' RETURN n.fqn AS a".to_string(),
        ),
        (
            "Struct",
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'terraform.' RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c".to_string(),
        ),
        (
            "Function",
            "MATCH (n:Function) WHERE n.fqn STARTS WITH 'terraform.' RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c".to_string(),
        ),
        (
            "File",
            format!(
                "MATCH (n:File) WHERE {} RETURN n.fqn AS a, n.start_line AS b, n.end_line AS c",
                tf_file("n")
            ),
        ),
        (
            "Language",
            "MATCH (n:Language) WHERE n.fqn = 'terraform' RETURN n.fqn AS a".to_string(),
        ),
        (
            "UnresolvedTarget",
            format!(
                "MATCH (n:UnresolvedTarget) WHERE {} RETURN n.fqn AS a, n.category AS b",
                unresolved("n")
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

/// The full-vs-incremental parity fixture: a scratch repo with `apg init .`
/// committed, a full main `apg scan . --language terraform`, and project
/// `demo` started; every later scan runs in the `demo` worktree.
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
        repo.commit_all("tf fixture");
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
    /// incremental terraform scan (it must splice), check
    /// `precondition(before, after)` on the terraform graphs, then a `--force`
    /// rebuild must reproduce the incremental graph.
    fn step(&self, files: &[(&str, &str)], precondition: impl FnOnce(&[String], &[String])) {
        let before = tf_graph(&self.wt);
        for (rel, text) in files {
            let p = self.wt.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let rels: Vec<&str> = files.iter().map(|(r, _)| *r).collect();
        wt_commit(&self.wt, &rels, "parity edit");
        let log = self.apg(&self.wt, &["scan", ".", "--language", LANG]);
        assert!(spliced(&log), "incremental scan must splice: {log}");
        let incremental = tf_graph(&self.wt);
        precondition(&before, &incremental);
        let log = self.apg(&self.wt, &["scan", ".", "--language", LANG, "--force"]);
        assert!(!spliced(&log), "--force must full-rebuild: {log}");
        assert_eq!(
            incremental,
            tf_graph(&self.wt),
            "incremental (left) terraform graph must equal the --force rebuild (right) ({rels:?})"
        );
    }
}

/// Whether `graph` holds a line starting with `prefix`.
fn has(graph: &[String], prefix: &str) -> bool {
    graph.iter().any(|l| l.starts_with(prefix))
}

mod e2e {
    use super::*;

    /// `--language terraform` selects the frontend, every Terraform File is
    /// located under its directory module (the root rendering `terraform.`),
    /// the `terraform` Language contains `terraform.`, the expected Structs
    /// exist at their files, the override/tfvars/lock/test/module references
    /// draw their uses edges, the structural `.hcl`/`.json` files stay out,
    /// the shadowed and broken files keep their File with zero Structs, the
    /// `.terraform` cache is pruned, and `code_type` classifies test, generated
    /// and external files.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn language_flag_selects_terraform_and_renders_the_fixture_graph() {
        let repo = tf_repo("tf-lang", &tf_fixture());
        let (out, log) = scan(&repo, &["--language", LANG]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&LANG.to_string()),
            "Languages line must list terraform: {log}"
        );
        let root = &repo.root;

        let roots = pairs(
            root,
            "MATCH (l:Language)-[:Contains]->(m:Module) WHERE m.fqn = 'terraform.' RETURN l.fqn, m.fqn",
            "l.fqn",
            "m.fqn",
        );
        assert_eq!(
            roots,
            vec![("terraform".to_string(), "terraform.".to_string())],
            "the terraform Language contains terraform."
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
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'terraform.' RETURN n.fqn, n.path",
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
        assert!(
            structs
                .iter()
                .all(|(_, p)| p != "shadow.tf" && p != "broken.tf"),
            "shadowed and broken files contribute zero Structs: {structs:#?}"
        );

        let uses = pairs(
            root,
            "MATCH (s:Struct)-[:Uses]->(t:Struct) WHERE s.fqn STARTS WITH 'terraform.' RETURN s.fqn, t.fqn",
            "s.fqn",
            "t.fqn",
        );
        for (s, t) in [
            (
                "terraform..azurerm_resource_group.main",
                "terraform..var.location",
            ),
            (
                "terraform..output.rg_id",
                "terraform..azurerm_resource_group.main",
            ),
            ("terraform..module.net", "terraform..local.name"),
            (
                "terraform..tfvars.terraform.tfvars",
                "terraform..var.location",
            ),
            (
                "terraform..azurerm_resource_group.main",
                "terraform..local.name",
            ),
            (
                "terraform.net.azurerm_virtual_network.main",
                "terraform.net.var.location",
            ),
        ] {
            assert!(
                uses.iter().any(|(a, b)| (a.as_str(), b.as_str()) == (s, t)),
                "missing Uses {s} -> {t}: {uses:#?}"
            );
        }

        let unresolved = pairs(
            root,
            "MATCH (s:Struct)-[:UnresolvedUse]->(t:UnresolvedTarget) RETURN s.fqn, t.fqn",
            "s.fqn",
            "t.fqn",
        );
        for (s, t) in [
            (
                "terraform..module.remote",
                "Azure/avm-res-network-virtualnetwork/azurerm",
            ),
            ("terraform..tfvars.terraform.tfvars", "var.bogus"),
        ] {
            assert!(
                unresolved
                    .iter()
                    .any(|(a, b)| (a.as_str(), b.as_str()) == (s, t)),
                "missing UnresolvedUse {s} -> {t}: {unresolved:#?}"
            );
        }

        let edges = file_edges(root);
        assert_eq!(
            edges,
            expected_file_edges(),
            "Struct-to-File uses in the full scan"
        );

        let typed = pairs(
            root,
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'terraform.' RETURN n.fqn, n.code_type",
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
        let files = pairs(
            root,
            "MATCH (f:File) RETURN f.fqn, f.code_type",
            "f.fqn",
            "f.code_type",
        );
        assert!(
            files.iter().all(|(f, _)| !f.contains(".terraform/"))
                && structs.iter().all(|(_, p)| !p.contains(".terraform/")),
            "the .terraform cache is pruned: {files:#?}"
        );
        assert!(
            files.iter().any(|(f, _)| f == "shadow.tf")
                && files.iter().any(|(f, _)| f == "broken.tf"),
            "shadowed and broken Files keep their File node: {files:#?}"
        );

        let runs = pairs(
            root,
            "MATCH (t:Struct)-[:Contains]->(r:Struct) WHERE t.fqn = 'terraform..test.tests/x.tftest.hcl' RETURN t.fqn, r.fqn",
            "t.fqn",
            "r.fqn",
        );
        assert!(
            runs.contains(&(
                "terraform..test.tests/x.tftest.hcl".to_string(),
                "terraform..test.tests/x.tftest.hcl.run.smoke".to_string()
            )),
            "the test Struct contains its run Struct: {runs:#?}"
        );
    }

    /// Auto-detection (no `--language`) selects terraform, every Terraform
    /// file is claimed by the terraform stream alone (one `terraform.` module,
    /// never json/misc/md), a plain `.hcl` stays in misc, an unclaimed `.json`
    /// stays in json, and README.md stays in md.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn auto_detect_claims_each_terraform_file_once_and_routes_the_rest() {
        let repo = tf_repo("tf-auto", &tf_fixture());
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&LANG.to_string()),
            "auto-detection must select terraform: {log}"
        );
        let modules = pairs(
            &repo.root,
            "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn, f.fqn",
            "m.fqn",
            "f.fqn",
        );
        for f in TF_FILES {
            let mods: Vec<&String> = modules
                .iter()
                .filter(|(_, x)| x == f)
                .map(|(m, _)| m)
                .collect();
            assert_eq!(mods.len(), 1, "{f} has exactly one Module: {mods:?}");
            assert!(
                mods[0].starts_with("terraform."),
                "{f} must sit under a terraform. module, not {}",
                mods[0]
            );
        }
        for (f, prefix) in [
            ("notes/plain.hcl", "misc."),
            ("unclaimed.json", "json."),
            ("README.md", "md."),
        ] {
            assert!(
                modules.iter().any(|(m, x)| x == f && m.starts_with(prefix)),
                "{f} stays under {prefix}: {modules:#?}"
            );
        }
    }

    /// `--language terraform,md` merges two streams into one database: both
    /// `Language` roots exist, the terraform Module and its Struct are intact
    /// (no id collision drops either stream).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn merged_scan_keeps_terraform_ids_distinct_from_other_streams() {
        let repo = tf_repo(
            "tf-prefix",
            &[
                ("main.tf", "resource \"azurerm_resource_group\" \"y\" {}\n"),
                ("README.md", "# scratch\n"),
            ],
        );
        let (out, log) = scan(&repo, &["--language", "terraform,md"]);
        assert!(out.status.success(), "scan: {log}");
        let langs = languages(&log);
        assert!(
            langs.contains(&"terraform".to_string()) && langs.contains(&"md".to_string()),
            "both streams listed: {langs:?}"
        );
        let roots = pairs(
            &repo.root,
            "MATCH (l:Language)-[:Contains]->(m:Module) RETURN l.fqn, m.fqn",
            "l.fqn",
            "m.fqn",
        );
        assert!(
            roots.contains(&("terraform".to_string(), "terraform.".to_string())),
            "{roots:#?}"
        );
        assert!(
            roots.iter().any(|(l, m)| l == "md" && m.starts_with("md.")),
            "{roots:#?}"
        );
        let structs = pairs(
            &repo.root,
            "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'terraform.' RETURN n.fqn, n.path",
            "n.fqn",
            "n.path",
        );
        assert!(
            structs.contains(&(
                "terraform..azurerm_resource_group.y".to_string(),
                "main.tf".to_string()
            )),
            "the terraform stream survives the merge: {structs:#?}"
        );
    }

    /// Struct-to-File `module-source` and `override` uses edges (unrooted,
    /// repo-relative File targets, kinds carried) hold after the full scan,
    /// the branch seed, a spliced edit of the root file, and a `--force`
    /// rebuild that reproduces the spliced graph.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn file_dependency_edges_survive_full_and_spliced_builds() {
        let p = Parity::new("tf-file-deps", &tf_fixture());
        let want = expected_file_edges();
        assert_eq!(file_edges(&p.repo.root), want, "main full scan");
        assert_eq!(file_edges(&p.wt), want, "branch seed");
        let touched = format!("{MAIN_TF}# touched\n");
        p.step(&[("main.tf", &touched)], |_, _| {
            assert_eq!(file_edges(&p.wt), want, "after a spliced root edit");
        });
    }

    /// Sibling-file change: `net/net.tf` references `var.location`, declared in
    /// its sibling `net/vars.tf`. Renaming the variable there turns the
    /// reference into an unresolved `var.location`; the spliced graph equals a
    /// `--force` rebuild.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_sibling_file_change_reresolves_module_references() {
        let p = Parity::new("tf-parity-sibling", &tf_fixture());
        let resolved =
            "Uses \"terraform.net.azurerm_virtual_network.main\" \"terraform.net.var.location\"";
        let unresolved =
            "UnresolvedUse \"terraform.net.azurerm_virtual_network.main\" \"var.location\"";
        p.step(
            &[("net/vars.tf", "variable \"location_v2\" {}\n")],
            |before, after| {
                assert!(has(before, resolved), "resolved before: {before:#?}");
                assert!(!has(after, resolved), "no resolved edge after: {after:#?}");
                assert!(has(after, unresolved), "unresolved after: {after:#?}");
            },
        );
    }

    /// Module-source target change: `net/net.tf` is the target of the root
    /// `module "net"`. Adding a local block to it adds a Struct in that file,
    /// the Struct-to-File edges stay exactly the same, and the spliced graph
    /// equals a `--force` rebuild.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_module_source_target_edit_matches_force_rebuild() {
        let p = Parity::new("tf-parity-module", &tf_fixture());
        let edited = format!("{NET_TF}locals {{\n  tag = \"x\"\n}}\n");
        p.step(&[("net/net.tf", &edited)], |before, after| {
            let tag = "\"terraform.net.local.tag\"";
            assert!(!before.iter().any(|l| l.contains(tag)), "absent before");
            assert!(
                after.iter().any(|l| l.contains(tag)),
                "added after: {after:#?}"
            );
            assert_eq!(
                file_edges(&p.wt),
                expected_file_edges(),
                "module-source edges unchanged"
            );
        });
    }

    /// Root-module retargeting: renaming `variable "location"` in `main.tf`
    /// makes the tfvars key `location` and the provider's `var.location`
    /// unresolved (`var.location`, not a Struct); the spliced graph equals a
    /// `--force` rebuild.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg/terraformfrontend, db.lbug); run via cargo test-e2e"]
    fn parity_root_variable_retarget_matches_force_rebuild() {
        let p = Parity::new("tf-parity-retarget", &tf_fixture());
        let edited = MAIN_TF.replace("variable \"location\" {}", "variable \"zone\" {}");
        let uses = "Uses \"terraform..tfvars.terraform.tfvars\" \"terraform..var.location\"";
        let retargeted = "UnresolvedUse \"terraform..tfvars.terraform.tfvars\" \"var.location\"";
        p.step(&[("main.tf", &edited)], |before, after| {
            assert!(has(before, uses), "tfvars resolved before: {before:#?}");
            assert!(!has(after, uses), "tfvars edge retargeted: {after:#?}");
            assert!(
                has(after, retargeted),
                "tfvars key unresolved after: {after:#?}"
            );
        });
    }
}
