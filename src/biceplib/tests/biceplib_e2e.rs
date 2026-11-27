//! The crate's e2e tier: the built `bicepfrontend` binary run over fixture trees
//! written to a scratch temp dir, its JSONL stdout and stderr read back. No user path,
//! `$HOME` or real project is read.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// A unique scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("bicepfrontend-e2e-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Writes `bytes` to `name` (parents created) inside the scratch dir and returns its path.
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("create fixture dir");
        fs::write(&path, bytes).expect("write fixture");
        path
    }

    /// The scratch dir, symlink-resolved: the identity the binary's records carry.
    fn root(&self) -> String {
        fs::canonicalize(&self.0)
            .expect("canonical scratch")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One `bicepfrontend` run: stdout, stderr lines and whether it exited 0.
struct Run {
    stdout: String,
    stderr: Vec<String>,
    ok: bool,
}

/// Runs the built `bicepfrontend` binary with `args`.
fn bicep(args: &[&str]) -> Run {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bicepfrontend"))
        .args(args)
        .output()
        .expect("spawn bicepfrontend");
    Run {
        stdout: String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(str::to_string)
            .collect(),
        ok: out.status.success(),
    }
}

/// Every stdout line parsed as one JSON object.
fn records(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("one JSON value per line");
            assert!(v.is_object(), "{l}");
            v
        })
        .collect()
}

/// The records of one `type`.
fn of<'a>(recs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    recs.iter().filter(|r| r["type"] == kind).collect()
}

/// The string at `key` of `v`.
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().expect(key)
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

/// Each Module fqn, sorted.
fn modules(recs: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = of(recs, "module")
        .iter()
        .map(|r| text(r, "fqn").to_string())
        .collect();
    v.sort();
    v
}

/// Each File as (absolute path, parent module, start line, end line), sorted.
fn files(recs: &[Value]) -> Vec<(String, String, u64, u64)> {
    let mut v: Vec<_> = of(recs, "file")
        .iter()
        .map(|r| {
            (
                text(r, "path").to_string(),
                text(r, "parent").to_string(),
                r["start_line"].as_u64().expect("start_line"),
                r["end_line"].as_u64().expect("end_line"),
            )
        })
        .collect();
    v.sort();
    v
}

/// Struct id to name.
fn names(recs: &[Value]) -> HashMap<String, String> {
    of(recs, "struct")
        .iter()
        .map(|r| (text(r, "id").to_string(), text(r, "name").to_string()))
        .collect()
}

/// Each Struct as (name, parent), sorted.
fn structs(recs: &[Value]) -> Vec<(String, String)> {
    let mut v: Vec<_> = of(recs, "struct")
        .iter()
        .map(|r| pair(text(r, "name"), text(r, "parent")))
        .collect();
    v.sort();
    v
}

/// Each struct-to-struct Uses as (from name, to name), sorted. An unnamed target keeps its id.
fn uses(recs: &[Value]) -> Vec<(String, String)> {
    let by_id = names(recs);
    let mut v: Vec<_> = of(recs, "uses")
        .iter()
        .filter(|r| r.get("properties").is_none())
        .map(|r| {
            let to = text(r, "to");
            (
                by_id[text(r, "from")].clone(),
                by_id.get(to).cloned().unwrap_or(to.to_string()),
            )
        })
        .collect();
    v.sort();
    v
}

/// Each file-dependency Uses as (from name, target path, kind), sorted.
fn file_uses(recs: &[Value]) -> Vec<(String, String, String)> {
    let by_id = names(recs);
    let mut v: Vec<_> = of(recs, "uses")
        .iter()
        .filter(|r| r.get("properties").is_some())
        .map(|r| {
            (
                by_id[text(r, "from")].clone(),
                text(r, "to").to_string(),
                text(&r["properties"], "kind").to_string(),
            )
        })
        .collect();
    v.sort();
    v
}

/// Each Contains between Structs as (parent name, child name), sorted.
fn contains(recs: &[Value]) -> Vec<(String, String)> {
    let by_id = names(recs);
    let mut v: Vec<_> = of(recs, "contains")
        .iter()
        .map(|r| pair(&by_id[text(r, "from")], &by_id[text(r, "to")]))
        .collect();
    v.sort();
    v
}

/// Every record as its canonical JSON text, sorted: the order-free set of a run.
fn sorted(recs: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = recs.iter().map(Value::to_string).collect();
    v.sort();
    v
}

/// The two-file import fixture: `app/main.bicep` imports `skuName` from `shared/store.bicep`.
const IMPORT: &str = "import { skuName } from '../shared/store.bicep'\n";

fn import_tree(dir: &Scratch) {
    dir.write("shared/store.bicep", "type skuName = string\n");
    dir.write("app/main.bicep", IMPORT);
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn tree_emits_modules_files_structs_and_edges() {
        let dir = Scratch::new("tree");
        let main = "param location string\nresource stg 'Microsoft.Storage/storageAccounts@2023-01-01' = {\n  name: 'stg1'\n  location: location\n}\nmodule net './net/vnet.bicep' = {\n  name: 'net'\n}\n";
        let vnet = "param addressPrefix string\nresource vnet 'Microsoft.Network/virtualNetworks@2023-04-01' = {\n  name: 'vnet1'\n  properties: {\n    prefix: addressPrefix\n  }\n}\noutput vnetId string = vnet.id\n";
        dir.write("main.bicep", main);
        dir.write("net/vnet.bicep", vnet);
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(run.stderr, ["bicep: 0 files had syntax errors"]);
        let recs = records(&run.stdout);
        // Only the frontend's vocabulary: `lang_switch` is the ingestor's record, not ours.
        const KINDS: [&str; 9] = [
            "module",
            "file",
            "struct",
            "function",
            "contains",
            "calls",
            "uses",
            "unresolved_use",
            "unresolved",
        ];
        assert!(recs.iter().all(|r| KINDS.contains(&text(r, "type"))));
        assert_eq!(modules(&recs), ["main.bicep", "net/vnet.bicep"]);
        assert_eq!(
            files(&recs),
            [
                (
                    format!("{root}/main.bicep"),
                    "main.bicep".to_string(),
                    1,
                    main.lines().count() as u64
                ),
                (
                    format!("{root}/net/vnet.bicep"),
                    "net/vnet.bicep".to_string(),
                    1,
                    vnet.lines().count() as u64
                ),
            ]
        );
        assert_eq!(
            structs(&recs),
            [
                pair("module.net", "main.bicep"),
                pair("output.vnetId", "net/vnet.bicep"),
                pair("param.addressPrefix", "net/vnet.bicep"),
                pair("param.location", "main.bicep"),
                pair("resource.stg", "main.bicep"),
                pair("resource.vnet", "net/vnet.bicep"),
            ]
        );
        assert_eq!(
            uses(&recs),
            [
                pair("output.vnetId", "resource.vnet"),
                pair("resource.stg", "param.location"),
                pair("resource.vnet", "param.addressPrefix"),
            ]
        );
        assert_eq!(
            file_uses(&recs),
            [(
                "module.net".to_string(),
                format!("{root}/net/vnet.bicep"),
                "module".to_string()
            )]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn functions_calls_and_child_resources_carry_parent_and_contains() {
        let dir = Scratch::new("funcs");
        dir.write(
            "app.bicep",
            "func double(n int) int => n * 2\nfunc quad(n int) int => double(double(n))\nresource plan 'Microsoft.Web/serverfarms@2023-01-01' = {\n  name: 'plan'\n  resource slot 'Microsoft.Web/sites/slots@2023-01-01' = {\n    name: 'slot'\n  }\n}\n",
        );
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let mut functions: Vec<(String, String)> = of(&recs, "function")
            .iter()
            .map(|r| pair(text(r, "name"), text(r, "parent")))
            .collect();
        functions.sort();
        assert_eq!(
            functions,
            [
                pair("func.double", "app.bicep"),
                pair("func.quad", "app.bicep")
            ]
        );
        let fnames: HashMap<String, String> = of(&recs, "function")
            .iter()
            .map(|r| (text(r, "id").to_string(), text(r, "name").to_string()))
            .collect();
        let calls: Vec<(String, String)> = of(&recs, "calls")
            .iter()
            .map(|r| pair(&fnames[text(r, "from")], &fnames[text(r, "to")]))
            .collect();
        assert_eq!(calls, [pair("func.quad", "func.double")]);
        assert_eq!(
            structs(&recs),
            [
                pair("resource.plan", "app.bicep"),
                pair("slot", "app.bicep.resource.plan"),
            ]
        );
        assert_eq!(contains(&recs), [pair("resource.plan", "slot")]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn syntax_error_keeps_file_and_reports_and_scan_continues() {
        let dir = Scratch::new("syntax");
        dir.write(
            "broken.bicep",
            "param p string\nresource bad\nparam q int\n",
        );
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let prefix = format!("bicep: {root}/broken.bicep:2:");
        assert_eq!(run.stderr.len(), 2, "{:?}", run.stderr);
        assert!(run.stderr[0].starts_with(&prefix), "{:?}", run.stderr);
        assert_eq!(
            run.stderr.last().map(String::as_str),
            Some("bicep: 1 file had syntax errors")
        );
        let recs = records(&run.stdout);
        assert_eq!(
            files(&recs),
            [(
                format!("{root}/broken.bicep"),
                "broken.bicep".to_string(),
                1,
                3
            )]
        );
        assert_eq!(
            structs(&recs),
            [
                pair("param.p", "broken.bicep"),
                pair("param.q", "broken.bicep"),
            ]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn duplicate_declaration_is_reported_and_first_kept() {
        let dir = Scratch::new("dup");
        dir.write("dup.bicep", "param p string\nparam p int\n");
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(
            run.stderr,
            [
                format!("bicep: {root}/dup.bicep:2: duplicate declaration param.p"),
                "bicep: 0 files had syntax errors".to_string(),
            ]
        );
        assert_eq!(
            structs(&records(&run.stdout)),
            [pair("param.p", "dup.bicep")]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn nonexistent_directory_scans_nothing_and_succeeds() {
        let dir = Scratch::new("missing");
        let root = format!("{}/absent", dir.root());
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert!(run.stdout.is_empty(), "{}", run.stdout);
        assert_eq!(run.stderr, ["bicep: 0 files had syntax errors"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn module_and_registry_and_missing_paths_resolve_as_specified() {
        let dir = Scratch::new("modules");
        dir.write(
            "main.bicep",
            "module arm './arm/template.json' = {\n  name: 'arm'\n}\nmodule reg 'br:mcr.microsoft.com/bicep/avm/res/storage:1.0.0' = {\n  name: 'reg'\n}\nmodule gone './gone.bicep' = {\n  name: 'gone'\n}\n",
        );
        dir.write("arm/template.json", "{}\n");
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        // The ARM template draws a module edge only: no File record is emitted for it.
        assert_eq!(
            files(&recs).iter().map(|f| f.0.clone()).collect::<Vec<_>>(),
            [format!("{root}/main.bicep")]
        );
        assert_eq!(
            file_uses(&recs),
            [(
                "module.arm".to_string(),
                format!("{root}/arm/template.json"),
                "module".to_string()
            )]
        );
        let mut unresolved: Vec<(String, String)> = of(&recs, "unresolved")
            .iter()
            .map(|r| pair(text(r, "fqn"), text(r, "category")))
            .collect();
        unresolved.sort();
        assert_eq!(
            unresolved,
            [
                pair(
                    "br:mcr.microsoft.com/bicep/avm/res/storage:1.0.0",
                    "external"
                ),
                pair("gone.bicep", "unknown"),
            ]
        );
        let by_id = names(&recs);
        let mut dangling: Vec<(String, String)> = of(&recs, "unresolved_use")
            .iter()
            .map(|r| pair(&by_id[text(r, "from")], text(r, "to")))
            .collect();
        dangling.sort();
        assert_eq!(
            dangling,
            [
                pair("module.gone", "gone.bicep"),
                pair(
                    "module.reg",
                    "br:mcr.microsoft.com/bicep/avm/res/storage:1.0.0"
                ),
            ]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn bicepparam_and_upper_case_bicep_are_claimed() {
        let dir = Scratch::new("params");
        dir.write(
            "app/dev.bicepparam",
            "using './main.bicep'\nextends './base.bicepparam'\nparam location = 'westeurope'\n",
        );
        dir.write("app/base.bicepparam", "param sku = 'Standard_LRS'\n");
        dir.write("app/main.bicep", "param location string\n");
        dir.write("app/Other.BICEP", "param o string\n");
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let mut paths: Vec<String> = files(&recs).into_iter().map(|f| f.0).collect();
        paths.sort();
        let mut expected = [
            format!("{root}/app/base.bicepparam"),
            format!("{root}/app/dev.bicepparam"),
            format!("{root}/app/main.bicep"),
            format!("{root}/app/Other.BICEP"),
        ];
        expected.sort();
        assert_eq!(paths, expected);
        assert_eq!(
            file_uses(&recs),
            [
                (
                    "extends".to_string(),
                    format!("{root}/app/base.bicepparam"),
                    "extends".to_string()
                ),
                (
                    "using".to_string(),
                    format!("{root}/app/main.bicep"),
                    "using".to_string()
                ),
            ]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn excluded_directories_are_not_walked() {
        let dir = Scratch::new("pruned");
        dir.write("main.bicep", "param a string\n");
        for skipped in [
            ".git/x.bicep",
            "node_modules/x.bicep",
            "target/x.bicep",
            ".terraform/x.bicep",
            ".worktrees/x.bicep",
            "apg/x.bicep",
        ] {
            dir.write(skipped, "param x string\n");
        }
        let root = dir.root();
        let run = bicep(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        assert_eq!(modules(&recs), ["main.bicep"]);
        assert!(!run.stdout.contains("x.bicep"), "{}", run.stdout);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn exclude_argument_drops_matching_paths() {
        let dir = Scratch::new("exclude");
        dir.write("main.bicep", "param a string\n");
        dir.write("vendored/x.bicep", "param x string\n");
        let root = dir.root();
        let run = bicep(&[&root, "vendored"]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        assert_eq!(modules(&recs), ["main.bicep"]);
        assert_eq!(structs(&recs), [pair("param.a", "main.bicep")]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn id_prefix_prefixes_every_struct_id() {
        let dir = Scratch::new("prefix");
        dir.write("main.bicep", "param a string\nparam b string\n");
        let root = dir.root();
        let run = bicep(&[&root, "--id-prefix", "bc"]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let ids: Vec<&str> = of(&recs, "struct").iter().map(|r| text(r, "id")).collect();
        assert_eq!(ids, ["bc1", "bc2"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn targets_of_every_file_equal_the_full_scan() {
        let dir = Scratch::new("targets-all");
        import_tree(&dir);
        let root = dir.root();
        let full = bicep(&[&root]);
        assert!(full.ok, "{:?}", full.stderr);
        let side = Scratch::new("targets-all-side");
        let list = side.write(
            "targets.txt",
            format!("{root}/app/main.bicep\n{root}/shared/store.bicep\n"),
        );
        let run = bicep(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(
            sorted(&records(&run.stdout)),
            sorted(&records(&full.stdout))
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn single_target_keeps_its_facts_and_retargets_dropped_uses() {
        let dir = Scratch::new("targets-one");
        import_tree(&dir);
        let import = IMPORT.trim_end();
        let root = dir.root();
        let side = Scratch::new("targets-one-side");
        let list = side.write("targets.txt", format!("{root}/app/main.bicep\n"));
        let run = bicep(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        let expected = [
            json!({"type": "module", "fqn": "app/main.bicep"}),
            json!({"type": "file", "path": format!("{root}/app/main.bicep"), "parent": "app/main.bicep", "start_line": 1, "end_line": 1}),
            json!({"type": "struct", "id": "bp1", "parent": "app/main.bicep", "name": "import.skuName", "path": format!("{root}/app/main.bicep"), "start": 0, "end": import.len(), "start_line": 1, "end_line": 1}),
            json!({"type": "uses", "from": "bp1", "to": format!("{root}/shared/store.bicep"), "properties": {"kind": "import"}}),
            // The import's symbol lives in the dropped store file: its use names the key.
            json!({"type": "uses", "from": "bp1", "to": "shared/store.bicep.type.skuName"}),
        ];
        let mut expected: Vec<String> = expected.iter().map(Value::to_string).collect();
        expected.sort();
        assert_eq!(sorted(&records(&run.stdout)), expected);
    }
}
