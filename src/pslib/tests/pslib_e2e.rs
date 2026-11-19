//! The crate's e2e tier: fixture files written to a scratch temp dir, read back
//! from disk and parsed through `psfrontend::parse`. No user path, `$HOME` or
//! real project is read.

use psfrontend::parse::parse;
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Tree};

/// A unique scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pslib-e2e-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Writes `bytes` to `name` (parents created) inside the scratch dir and returns its path.
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
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

/// One `psfrontend` run: stdout, stderr lines and whether it exited 0.
struct Run {
    stdout: String,
    stderr: Vec<String>,
    ok: bool,
}

/// Runs the built `psfrontend` binary with `args`.
fn psfrontend(args: &[&str]) -> Run {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_psfrontend"))
        .args(args)
        .output()
        .expect("spawn psfrontend");
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
fn records(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("one JSON value per line");
            assert!(v.is_object(), "{l}");
            v
        })
        .collect()
}

/// The `(path, parent, start_line, end_line)` of every File record, sorted.
fn file_recs(recs: &[serde_json::Value]) -> Vec<(String, String, u64, u64)> {
    let mut out: Vec<_> = recs
        .iter()
        .filter(|r| r["type"] == "file")
        .map(|r| {
            (
                r["path"].as_str().expect("path").to_string(),
                r["parent"].as_str().expect("parent").to_string(),
                r["start_line"].as_u64().expect("start_line"),
                r["end_line"].as_u64().expect("end_line"),
            )
        })
        .collect();
    out.sort();
    out
}

/// The fqn of every Module record, in emission order.
fn module_recs(recs: &[serde_json::Value]) -> Vec<String> {
    recs.iter()
        .filter(|r| r["type"] == "module")
        .map(|r| r["fqn"].as_str().expect("fqn").to_string())
        .collect()
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Reads a UTF-8 file back.
fn read_utf8(path: &Path) -> String {
    String::from_utf8(fs::read(path).expect("read fixture")).expect("UTF-8 fixture")
}

/// `text` as UTF-16LE bytes behind an FF FE byte-order mark.
fn utf16le_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

/// `text` as UTF-16BE bytes behind an FE FF byte-order mark.
fn utf16be_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFE, 0xFF];
    out.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    out
}

/// Reads a UTF-16LE-with-BOM file back, the mark dropped.
fn read_utf16le(path: &Path) -> String {
    let bytes = fs::read(path).expect("read fixture");
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).expect("UTF-16LE BOM");
    let (pairs, rest) = body.as_chunks::<2>();
    assert!(rest.is_empty(), "whole UTF-16 code units");
    let units: Vec<u16> = pairs.iter().map(|b| u16::from_le_bytes(*b)).collect();
    String::from_utf16(&units).expect("valid UTF-16")
}

/// Every node of `kind` in `tree`, in tree order.
fn nodes<'t>(tree: &'t Tree, kind: &str) -> Vec<Node<'t>> {
    fn walk<'t>(node: Node<'t>, kind: &str, out: &mut Vec<Node<'t>>) {
        if node.kind() == kind {
            out.push(node);
        }
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            walk(child, kind, out);
        }
    }
    let mut out = Vec::new();
    walk(tree.root_node(), kind, &mut out);
    out
}

/// The text of every node of `kind` in `tree`, in tree order.
fn texts(tree: &Tree, src: &str, kind: &str) -> Vec<String> {
    nodes(tree, kind)
        .iter()
        .map(|n| src[n.byte_range()].to_string())
        .collect()
}

/// The text of `node`'s `field` child.
fn field<'s>(node: Node, field: &str, src: &'s str) -> &'s str {
    &src[node.child_by_field_name(field).expect(field).byte_range()]
}

/// The kinds from `node` up to the root.
fn ancestry(node: Node) -> Vec<&'static str> {
    std::iter::successors(Some(node), Node::parent)
        .map(|n| n.kind())
        .collect()
}

/// Every ERROR (with its text) and MISSING (with its kind) node in `tree`.
fn errors(tree: &Tree, src: &str) -> Vec<String> {
    fn walk(node: Node, src: &str, out: &mut Vec<String>) {
        if node.is_missing() {
            out.push(format!("MISSING {}", node.kind()));
        } else if node.is_error() {
            out.push(format!("ERROR {:?}", &src[node.byte_range()]));
        }
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            walk(child, src, out);
        }
    }
    let mut out = Vec::new();
    walk(tree.root_node(), src, &mut out);
    out
}

/// Parses `src`, asserting the tree carries no ERROR/MISSING node.
fn parse_clean(src: &str) -> Tree {
    let tree = parse(src);
    assert_eq!(errors(&tree, src), Vec::<String>::new(), "{src:?}");
    tree
}

/// Writes the nine-file module-identity tree; the manifests UTF-16LE-with-BOM when `wide`.
fn module_tree(dir: &Scratch, wide: bool) {
    let manifest = |text: &str| {
        if wide {
            utf16le_bom(text)
        } else {
            text.as_bytes().to_vec()
        }
    };
    dir.write(
        "mods/Foo/Foo.psd1",
        &manifest(
            "@{\n    RootModule = 'Foo.psm1'\n    NestedModules = @('.\\lib\\Helpers.ps1', 'sub/../Other.ps1', 'NotAFile')\n    FunctionsToExport = @('Get-*')\n}\n",
        ),
    );
    dir.write("mods/Foo/Foo.psm1", b"function Get-Foo {}\n");
    dir.write("mods/Foo/lib/Helpers.ps1", b"function Get-Help1 {}\n");
    dir.write("mods/Foo/Other.ps1", b"function Get-Other {}\n");
    dir.write("mods/Foo/loose.ps1", b"Get-Foo\n");
    dir.write("mods/Foo/Data.psd1", &manifest("@{ Setting = 1 }\n"));
    dir.write(
        "mods/Bar/Bar.psd1",
        &manifest("@{\n    NestedModules = '..\\Foo\\Other.ps1'\n}\n"),
    );
    dir.write("mods/Bar/Bar.psm1", b"function Get-Bar {}\n");
    dir.write("root.ps1", b"Get-Bar\n");
}

/// The sorted `(path, parent)` of every File record.
fn file_parents(recs: &[serde_json::Value]) -> Vec<(String, String)> {
    file_recs(recs)
        .into_iter()
        .map(|(p, m, _, _)| (p, m))
        .collect()
}

/// The sorted fqns of every Module record (duplicates kept).
fn sorted_modules(recs: &[serde_json::Value]) -> Vec<String> {
    let mut out = module_recs(recs);
    out.sort();
    out
}

/// Asserts the whole module-identity tree's Module set and File parents under `root`.
fn assert_module_tree(root: &str, recs: &[serde_json::Value]) {
    assert_eq!(
        sorted_modules(recs),
        [
            "",
            "mods/Bar",
            "mods/Bar/Bar.psd1",
            "mods/Foo",
            "mods/Foo/Foo.psd1"
        ]
    );
    let file = |p: &str, m: &str| (format!("{root}/{p}"), m.to_string());
    assert_eq!(
        file_parents(recs),
        [
            file("mods/Bar/Bar.psd1", "mods/Bar/Bar.psd1"),
            file("mods/Bar/Bar.psm1", "mods/Bar"),
            file("mods/Foo/Data.psd1", "mods/Foo"),
            file("mods/Foo/Foo.psd1", "mods/Foo/Foo.psd1"),
            file("mods/Foo/Foo.psm1", "mods/Foo/Foo.psd1"),
            file("mods/Foo/Other.ps1", "mods/Bar/Bar.psd1"),
            file("mods/Foo/lib/Helpers.ps1", "mods/Foo/Foo.psd1"),
            file("mods/Foo/loose.ps1", "mods/Foo"),
            file("root.ps1", ""),
        ]
    );
}

/// The UTF-16LE-with-BOM script of the unit-identity tree: its function after a non-ASCII line.
const U_PS1: &str = "# ünï 😀\nfunction Get-U {\n    'é'\n}\n";

/// The unit-identity tree's scripts handed to `--targets` one at a time.
const UNIT_TARGETS: [&str; 7] = [
    "x.a.ps1",
    ".x/a.ps1",
    "a/b.ps1",
    "m/s.ps1",
    "build.ps1",
    "build/x.ps1",
    "mods/Foo/Foo.psm1",
];

/// Writes the unit-identity tree: a manifest module, the script/Module/function
/// collisions, the build pair and a UTF-16LE-with-BOM script.
fn unit_tree(dir: &Scratch) {
    dir.write(
        "mods/Foo/Foo.psd1",
        b"@{\n    RootModule = 'Foo.psm1'\n    NestedModules = @('lib/Helpers.ps1')\n}\n",
    );
    dir.write(
        "mods/Foo/lib/Helpers.ps1",
        b"function global:Get-Foo {\n    function Inner { }\n}\nfilter Bar { $_ }\nworkflow Baz { }\n",
    );
    dir.write(
        "mods/Foo/Foo.psm1",
        b"class C {\n    [E]$p\n    C() { }\n    M() { }\n    M([int]$x) { }\n}\nenum E { A; B }\n",
    );
    dir.write("x.a.ps1", b"Get-A\n");
    dir.write(".x/a.ps1", b"Get-A\n");
    dir.write("a/b.ps1", b"Get-A\n");
    dir.write("a.b.ps1/f.ps1", b"Get-A\n");
    dir.write("m/s.ps1", b"function C.M { }\nclass C {\n    M() { }\n}\n");
    dir.write("build.ps1", b"function F {}\n");
    dir.write("build/x.ps1", b"function F {}\n");
    dir.write("u.ps1", &utf16le_bom(U_PS1));
}

/// The string field `k` of record `r` (empty when absent).
fn str_of<'r>(r: &'r serde_json::Value, k: &str) -> &'r str {
    r[k].as_str().unwrap_or_default()
}

/// A Function record's params; an empty list is omitted from the JSONL.
fn params_of(r: &serde_json::Value) -> Vec<&str> {
    r["params"]
        .as_array()
        .map(|a| a.iter().filter_map(|p| p.as_str()).collect())
        .unwrap_or_default()
}

/// Phase 7's `ps` rendering re-implemented over `recs`: each Module/Struct/Function
/// record's (key, `ps.` + key) — Module key = fqn, Struct key = parent.name verbatim,
/// Functions grouped by exact (parent, name) as the ingestor's render_function_fqns does.
fn ps_rendered(recs: &[serde_json::Value]) -> Vec<(String, String)> {
    let mut groups: std::collections::HashMap<(&str, &str), usize> = Default::default();
    for r in recs.iter().filter(|r| r["type"] == "function") {
        *groups
            .entry((str_of(r, "parent"), str_of(r, "name")))
            .or_default() += 1;
    }
    recs.iter()
        .filter_map(|r| {
            let (parent, name) = (str_of(r, "parent"), str_of(r, "name"));
            let key = match str_of(r, "type") {
                "module" => str_of(r, "fqn").to_string(),
                "struct" => format!("{parent}.{name}"),
                "function" if groups[&(parent, name)] > 1 => {
                    format!("{parent}.{name}({})", params_of(r).join(","))
                }
                "function" => format!("{parent}.{name}"),
                _ => return None,
            };
            Some((key.clone(), format!("ps.{key}")))
        })
        .collect()
}

/// The `ps.`-rendered FQN of the record keyed `key` in `recs`.
fn rendered_of(recs: &[serde_json::Value], key: &str) -> String {
    ps_rendered(recs)
        .into_iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no record keyed {key:?}"))
        .1
}

/// The Struct/Function records located in `path` plus the Contains edges leaving them.
fn file_units(recs: &[serde_json::Value], path: &str) -> Vec<serde_json::Value> {
    let ids: Vec<&str> = recs
        .iter()
        .filter(|r| matches!(str_of(r, "type"), "struct" | "function") && str_of(r, "path") == path)
        .map(|r| str_of(r, "id"))
        .collect();
    recs.iter()
        .filter(|r| match str_of(r, "type") {
            "struct" | "function" => ids.contains(&str_of(r, "id")),
            "contains" => ids.contains(&str_of(r, "from")),
            _ => false,
        })
        .cloned()
        .collect()
}

/// The file-dependency tree's main.ps1: two dot-sources and six imports.
const DEP_MAIN: &str = ". $PSScriptRoot\\lib\\util.ps1\n. $x\nImport-Module ./mods/Foo/Foo.psm1\nImport-Module Foo\nImport-Module Bar\nImport-Module Microsoft.PowerShell.Utility\nImport-Module PSReadLine\nImport-Module $m\n";

/// The file-dependency tree's syntax-error script: a dot-source before a local error.
const DEP_BROKEN: &str = ". ./lib/Util.ps1\n$q = 1 )\n";

/// Writes the file-dependency tree into a fresh scratch dir and runs the binary over it.
fn dep_scan(tag: &str) -> (Scratch, String, Run) {
    let dir = Scratch::new(tag);
    dir.write("mods/Foo/Foo.psd1", b"@{\n    RootModule = 'Foo.psm1'\n}\n");
    dir.write("mods/Foo/Foo.psm1", b"function Get-Foo {}\n");
    dir.write("mods/Bar/Bar.psd1", b"@{\n    RootModule = 'Bar.psm1'\n}\n");
    dir.write(
        "other/Bar/Bar.psd1",
        b"@{\n    RootModule = 'Bar.psm1'\n}\n",
    );
    dir.write("lib/Util.ps1", b"function Get-Util {}\n");
    dir.write("main.ps1", DEP_MAIN.as_bytes());
    dir.write("u.ps1", &utf16le_bom("# ünï 😀\n. $y\n"));
    dir.write("broken.ps1", DEP_BROKEN.as_bytes());
    dir.write(
        "dot_then_import.ps1",
        b". ./mods/Foo/Foo.psm1\nImport-Module ./mods/Foo/Foo.psm1\n",
    );
    dir.write(
        "import_then_dot.ps1",
        b"Import-Module ./mods/Foo/Foo.psm1\n. ./mods/Foo/Foo.psm1\n",
    );
    dir.write(
        "two_imports.ps1",
        b"Import-Module ./mods/Foo/Foo.psm1\n. ./mods/Foo/Foo.psm1\nImport-Module ./mods/Foo/Foo.psm1\n",
    );
    let root = dir.root();
    let run = psfrontend(&[&root]);
    assert!(run.ok, "{:?}", run.stderr);
    (dir, root, run)
}

/// The id of the script Struct (parented by a Module) located at `path`.
fn script_id<'r>(recs: &'r [serde_json::Value], path: &str) -> &'r str {
    let modules = module_recs(recs);
    let ids: Vec<&str> = recs
        .iter()
        .filter(|r| r["type"] == "struct" && str_of(r, "path") == path)
        .filter(|r| modules.iter().any(|m| m == str_of(r, "parent")))
        .map(|r| str_of(r, "id"))
        .collect();
    assert_eq!(ids.len(), 1, "{path}");
    ids[0]
}

/// The sorted `(to, kind)` of the uses records leaving `id`.
fn uses_of(recs: &[serde_json::Value], id: &str) -> Vec<(String, String)> {
    let mut out: Vec<_> = recs
        .iter()
        .filter(|r| r["type"] == "uses" && r["from"] == id)
        .map(|r| {
            let kind = r["properties"]["kind"].as_str().expect("kind");
            (str_of(r, "to").to_string(), kind.to_string())
        })
        .collect();
    out.sort();
    out
}

/// The sorted `(to, category)` of the unresolved_use records leaving `id`.
fn unresolved_of(recs: &[serde_json::Value], id: &str) -> Vec<(String, String)> {
    let mut out: Vec<_> = recs
        .iter()
        .filter(|r| r["type"] == "unresolved_use" && r["from"] == id)
        .map(|r| {
            let to = str_of(r, "to");
            let cat = recs
                .iter()
                .find(|u| u["type"] == "unresolved" && u["fqn"] == to)
                .map_or("<none>", |u| str_of(u, "category"));
            (to.to_string(), cat.to_string())
        })
        .collect();
    out.sort();
    out
}

/// The raw offset `N` of a `<nameless@{rel}:N>` sentinel.
fn sentinel_offset(fqn: &str, rel: &str) -> usize {
    fqn.strip_prefix(&format!("<nameless@{rel}:"))
        .and_then(|s| s.strip_suffix('>'))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("not a {rel} sentinel: {fqn}"))
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn dot_sourcing_ps1() {
        let dir = Scratch::new("dot");
        let path = dir.write("main.ps1", b". $PSScriptRoot\\lib.ps1\n. ./other.ps1\n");
        let src = read_utf8(&path);
        let tree = parse_clean(&src);
        let ops = nodes(&tree, "invocation_operator_command");
        let shape: Vec<_> = ops
            .iter()
            .map(|n| (field(*n, "operator", &src), field(*n, "command", &src)))
            .collect();
        assert_eq!(
            shape,
            [(".", "$PSScriptRoot\\lib.ps1"), (".", "./other.ps1")]
        );
        let first = ops[0].child_by_field_name("command").expect("command");
        assert_eq!(first.kind(), "command_argument");
        let parts: Vec<_> = (0..first.named_child_count())
            .map(|i| first.named_child(i as u32).expect("child").kind())
            .collect();
        assert_eq!(parts, ["variable", "argument_fragment"]);
        assert_eq!(texts(&tree, &src, "variable"), ["$PSScriptRoot"]);
        assert_eq!(texts(&tree, &src, "argument_fragment"), ["\\lib.ps1"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn using_module_psm1() {
        let dir = Scratch::new("using");
        let path = dir.write("Mod.psm1", b"using module ./Foo.psm1\n");
        let src = read_utf8(&path);
        let tree = parse_clean(&src);
        assert_eq!(
            texts(&tree, &src, "using_statement"),
            ["using module ./Foo.psm1"]
        );
        assert_eq!(texts(&tree, &src, "command_argument"), ["./Foo.psm1"]);
        assert!(nodes(&tree, "command").is_empty());
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn dsc_configuration_ps1() {
        let dir = Scratch::new("dsc");
        let path = dir.write(
            "web.ps1",
            b"configuration Web { Node 'localhost' { File Site { } } }\n",
        );
        let src = read_utf8(&path);
        let tree = parse_clean(&src);
        let res = nodes(&tree, "resource_statement");
        assert_eq!(res.len(), 1);
        assert_eq!(
            (field(res[0], "type", &src), field(res[0], "name", &src)),
            ("File", "Site")
        );
        assert_eq!(
            ancestry(res[0]),
            [
                "resource_statement",
                "script_block",
                "node_statement",
                "script_block",
                "configuration_definition",
                "source_file"
            ]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn psd1_manifest() {
        let dir = Scratch::new("psd1");
        let path = dir.write(
            "Foo.psd1",
            b"@{\n    RootModule = 'Foo.psm1'\n    NestedModules = @('lib\\Helpers.ps1')\n    FunctionsToExport = @('Get-Foo')\n}\n",
        );
        let src = read_utf8(&path);
        let tree = parse_clean(&src);
        let hashes = nodes(&tree, "hashtable_expression");
        assert_eq!(hashes.len(), 1);
        assert_eq!(&src[hashes[0].byte_range()], src.trim_end());
        // walked up from the hashtable, so no wrapper count is assumed
        assert_eq!(
            ancestry(hashes[0]),
            [
                "hashtable_expression",
                "command_expression",
                "pipeline",
                "source_file"
            ]
        );
        let keys: Vec<_> = nodes(&tree, "hash_entry")
            .into_iter()
            .map(|e| field(e, "key", &src))
            .collect();
        assert_eq!(keys, ["RootModule", "NestedModules", "FunctionsToExport"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn utf16le_bom_ps1_matches_utf8_twin() {
        let dir = Scratch::new("utf16");
        let text = "# café\nWrite-Output 'héllo'\n. ./lib.ps1\n";
        let utf8 = read_utf8(&dir.write("twin.ps1", text.as_bytes()));
        let utf16 = read_utf16le(&dir.write("wide.ps1", &utf16le_bom(text)));
        assert_eq!(utf16, utf8);
        let (t8, t16) = (parse_clean(&utf8), parse_clean(&utf16));
        assert_eq!(t16.root_node().to_sexp(), t8.root_node().to_sexp());
        assert_eq!(texts(&t16, &utf16, "command_name"), ["Write-Output"]);
        assert_eq!(
            texts(&t16, &utf16, "invocation_operator_command"),
            [". ./lib.ps1"]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_claims_and_walks() {
        let dir = Scratch::new("bin-walk");
        for name in [
            "a.ps1",
            "sub/B.PSM1",
            "sub/c.Psd1",
            "sub/apg/q.ps1",
            "notes.txt",
            "x.ps",
            "target/t.ps1",
            "node_modules/n.ps1",
            ".git/g.ps1",
            "apg/p.ps1",
        ] {
            dir.write(name, b"Get-A\n");
        }
        let root = dir.root();
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        // a script Struct, its <script> Function and their contains edge per .ps1/.psm1, none per .psd1
        let count = |t: &str| recs.iter().filter(|r| r["type"] == t).count();
        assert_eq!(
            ["module", "file", "struct", "function", "contains"].map(count),
            [3, 4, 3, 3, 3]
        );
        // plus Get-A's one unresolved record and each <script>'s unresolved call to it
        assert_eq!(["unresolved", "unresolved_call"].map(count), [1, 3]);
        assert_eq!(recs.len(), 20);
        let file = |p: &str, m: &str| (format!("{root}/{p}"), m.to_string(), 1, 1);
        assert_eq!(
            file_recs(&recs),
            [
                file("a.ps1", ""),
                file("sub/B.PSM1", "sub"),
                file("sub/apg/q.ps1", "sub/apg"),
                file("sub/c.Psd1", "sub"),
            ]
        );
        let mut modules = module_recs(&recs);
        modules.sort();
        assert_eq!(modules, ["", "sub", "sub/apg"]);
        assert_eq!(run.stderr, ["ps: 0 files had syntax errors"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_decodes_encodings() {
        let dir = Scratch::new("bin-enc");
        let text = "# café ünïcødé\nWrite-Output 'héllo 😀'\n$x = 'ß'\n";
        dir.write("u8.ps1", text.as_bytes());
        dir.write("bom.ps1", &[b"\xEF\xBB\xBF", text.as_bytes()].concat());
        dir.write("le.ps1", &utf16le_bom(text));
        dir.write("be.ps1", &utf16be_bom(text));
        let root = dir.root();
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let file = |p: &str| (format!("{root}/{p}"), String::new(), 1, 3);
        assert_eq!(
            file_recs(&records(&run.stdout)),
            [
                file("be.ps1"),
                file("bom.ps1"),
                file("le.ps1"),
                file("u8.ps1")
            ]
        );
        assert_eq!(run.stderr, ["ps: 0 files had syntax errors"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_reports_syntax_errors() {
        let dir = Scratch::new("bin-err");
        let broken = "Get-A\n$x = )\nfunction f {\n";
        dir.write("broken.ps1", broken.as_bytes());
        dir.write("clean.ps1", b"Get-B\n");
        let root = dir.root();
        let path = format!("{root}/broken.ps1");
        let mut expected: Vec<String> =
            psfrontend::diag::error_regions(&path, broken, &parse(broken))
                .iter()
                .map(|d| format!("ps: {d}"))
                .collect();
        assert!(!expected.is_empty());
        expected.push("ps: 1 file had syntax errors".to_string());
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(run.stderr, expected);
        assert_eq!(
            file_recs(&records(&run.stdout)),
            [
                (path, String::new(), 1, 3),
                (format!("{root}/clean.ps1"), String::new(), 1, 1)
            ]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_targets_module_and_handoff_flags() {
        let dir = Scratch::new("bin-tgt");
        dir.write("a.ps1", b"Get-A\n");
        dir.write("broken.ps1", b"$x = )\n");
        dir.write("sub/b.ps1", b"Get-B\nGet-C\n");
        let root = dir.root();
        let b = format!("{root}/sub/b.ps1");
        // b.ps1's records, its script Struct and <script> Function ids `n{s}`/`n{f}`, then
        // its two external command calls
        let only_b = |s: usize, f: usize| {
            format!(
                "{{\"type\":\"module\",\"fqn\":\"sub\"}}\n\
                 {{\"type\":\"file\",\"path\":\"{b}\",\"parent\":\"sub\",\"start_line\":1,\"end_line\":2}}\n\
                 {{\"type\":\"struct\",\"id\":\"n{s}\",\"parent\":\"sub\",\"name\":\"b.ps1\",\"path\":\"{b}\",\"start\":0,\"end\":12,\"start_line\":1,\"end_line\":2}}\n\
                 {{\"type\":\"function\",\"id\":\"n{f}\",\"parent\":\"sub.b.ps1\",\"name\":\"<script>\",\"file\":\"{b}\",\"path\":\"{b}\",\"start\":0,\"end\":12,\"start_line\":1,\"end_line\":2}}\n\
                 {{\"type\":\"contains\",\"from\":\"n{s}\",\"to\":\"n{f}\"}}\n\
                 {{\"type\":\"unresolved\",\"fqn\":\"Get-B\",\"category\":\"external\"}}\n\
                 {{\"type\":\"unresolved_call\",\"from\":\"n{f}\",\"to\":\"Get-B\"}}\n\
                 {{\"type\":\"unresolved\",\"fqn\":\"Get-C\",\"category\":\"external\"}}\n\
                 {{\"type\":\"unresolved_call\",\"from\":\"n{f}\",\"to\":\"Get-C\"}}\n"
            )
        };
        let side = Scratch::new("bin-tgt-side");
        let list = side.write("targets.txt", format!("{b}\n").as_bytes());
        let list = list.to_str().expect("UTF-8 path");

        let run = psfrontend(&[&root, "--targets", list]);
        assert!(run.ok, "{:?}", run.stderr);
        // ids are assigned over the whole tree (a.ps1 and broken.ps1 first) before filtering
        assert_eq!(run.stdout, only_b(5, 6));
        assert_eq!(
            run.stderr.last().map(String::as_str),
            Some("ps: 1 file had syntax errors")
        );

        let run = psfrontend(&[&root, "--module", "sub"]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(run.stdout, only_b(1, 2));
        assert_eq!(run.stderr, ["ps: 0 files had syntax errors"]);

        let plain = psfrontend(&[&root]);
        let cache = side.root();
        let run = psfrontend(&[
            &root,
            "--id-prefix",
            "p",
            "--cache-dir",
            &cache,
            "--cache-key",
            "k",
        ]);
        assert!(plain.ok && run.ok, "{:?}", run.stderr);
        assert!(run.stdout.contains("\"id\":\"p1\""), "{}", run.stdout);
        // the plain run's records with every id re-prefixed `p`
        let mut expected = records(&plain.stdout);
        for r in &mut expected {
            for k in ["id", "from", "to"] {
                if let Some(v) = r.get_mut(k) {
                    *v = v.as_str().expect("id").replacen('n', "p", 1).into();
                }
            }
        }
        assert_eq!(records(&run.stdout), expected);
        assert_eq!(run.stderr, plain.stderr);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_module_identity() {
        let dir = Scratch::new("bin-mod");
        module_tree(&dir, false);
        let root = dir.root();
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_module_tree(&root, &records(&run.stdout));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_module_identity_utf16_manifests() {
        let dir = Scratch::new("bin-mod-wide");
        module_tree(&dir, true);
        let root = dir.root();
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_module_tree(&root, &records(&run.stdout));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_module_identity_targets() {
        let dir = Scratch::new("bin-mod-tgt");
        module_tree(&dir, false);
        let root = dir.root();
        let helpers = format!("{root}/mods/Foo/lib/Helpers.ps1");
        let side = Scratch::new("bin-mod-tgt-side");
        let list = side.write("targets.txt", format!("{helpers}\n").as_bytes());
        let run = psfrontend(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        assert_eq!(sorted_modules(&recs), ["mods/Foo/Foo.psd1"]);
        assert_eq!(
            file_parents(&recs),
            [(helpers, "mods/Foo/Foo.psd1".to_string())]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_unit_names() {
        let dir = Scratch::new("bin-unit");
        unit_tree(&dir);
        let root = dir.root();
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let of = |t: &'static str| recs.iter().filter(move |r| r["type"] == t);
        let modules: Vec<&str> = of("module").map(|r| str_of(r, "fqn")).collect();
        // one script Struct (parent a Module) and one <script> Function it contains, per script
        for f in of("file") {
            let path = str_of(f, "path");
            if path.ends_with(".psd1") {
                continue;
            }
            let scripts: Vec<_> = of("struct")
                .filter(|r| str_of(r, "path") == path && modules.contains(&str_of(r, "parent")))
                .collect();
            let bodies: Vec<_> = of("function")
                .filter(|r| str_of(r, "path") == path && r["name"] == "<script>")
                .collect();
            assert_eq!((scripts.len(), bodies.len()), (1, 1), "{path}");
            let (s, b) = (scripts[0], bodies[0]);
            let key = format!("{}.{}", str_of(s, "parent"), str_of(s, "name"));
            assert_eq!(str_of(b, "parent"), key, "{path}");
            assert!(
                of("contains").any(|e| e["from"] == s["id"] && e["to"] == b["id"]),
                "{path}"
            );
        }
        // the sorted names of the Struct/Function records under `parent`
        let names = |parent: &str| -> Vec<&str> {
            let mut out: Vec<&str> = recs
                .iter()
                .filter(|r| matches!(str_of(r, "type"), "struct" | "function"))
                .filter(|r| str_of(r, "parent") == parent)
                .map(|r| str_of(r, "name"))
                .collect();
            out.sort();
            out
        };
        // base-relative script names; Get-Foo without global:, the nested function kept
        let md = "mods/Foo/Foo.psd1";
        assert_eq!(names(md), ["Foo.psm1", "lib/Helpers.ps1"]);
        assert_eq!(
            names(&format!("{md}.lib/Helpers.ps1")),
            ["<script>", "Bar", "Baz", "Get-Foo", "Inner"]
        );
        // the collisions alone are suffixed
        let mut suffixed: Vec<(String, &str)> = recs
            .iter()
            .filter(|r| str_of(r, "name").contains("//"))
            .map(|r| {
                let rel = str_of(r, "path")
                    .strip_prefix(&format!("{root}/"))
                    .expect("in root");
                (rel.to_string(), str_of(r, "name"))
            })
            .collect();
        suffixed.sort();
        let row = |p: &str, n: &'static str| (p.to_string(), n);
        assert_eq!(
            suffixed,
            [
                row(".x/a.ps1", "a.ps1//.x/a.ps1"),
                row("a/b.ps1", "b.ps1//a/b.ps1"),
                row("m/s.ps1", "C.M//m/s.ps1:2"),
                row("m/s.ps1", "M//m/s.ps1:4"),
                row("x.a.ps1", "x.a.ps1//x.a.ps1"),
            ]
        );
        // C's overloads carry their signatures and render as groups of one
        let ck = format!("{md}.Foo.psm1.C");
        let methods: Vec<(&str, Vec<&str>)> = of("function")
            .filter(|r| str_of(r, "parent") == ck)
            .map(|r| (str_of(r, "name"), params_of(r)))
            .collect();
        assert_eq!(
            methods,
            [("new", vec![]), ("M()", vec![]), ("M(int)", vec!["int"])]
        );
        assert_eq!(
            rendered_of(&recs, &format!("{ck}.M()")),
            format!("ps.{ck}.M()")
        );
        assert_eq!(
            rendered_of(&recs, &format!("{ck}.M(int)")),
            format!("ps.{ck}.M(int)")
        );
        // the build pair: a root script beside a same-named directory, none suffixed
        assert_eq!(names(""), ["build.ps1", "u.ps1", "x.a.ps1//x.a.ps1"]);
        assert_eq!(names(".build.ps1"), ["<script>", "F"]);
        assert_eq!(names("build"), ["x.ps1"]);
        assert_eq!(names("build.x.ps1"), ["<script>", "F"]);
        for (key, fqn) in [
            ("", "ps."),
            (".build.ps1", "ps..build.ps1"),
            (".build.ps1.F", "ps..build.ps1.F"),
            ("build", "ps.build"),
            ("build.x.ps1", "ps.build.x.ps1"),
            ("build.x.ps1.F", "ps.build.x.ps1.F"),
        ] {
            assert_eq!(rendered_of(&recs, key), fqn);
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_unit_keys_unique() {
        let dir = Scratch::new("bin-unit-uniq");
        unit_tree(&dir);
        let run = psfrontend(&[&dir.root()]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let rendered = ps_rendered(&recs);
        let fqns: std::collections::HashSet<&String> = rendered.iter().map(|(_, f)| f).collect();
        assert_eq!(fqns.len(), rendered.len(), "duplicate FQN in {rendered:?}");
        let keys: std::collections::HashSet<&String> = rendered.iter().map(|(k, _)| k).collect();
        let mut groups = std::collections::HashSet::new();
        for r in &recs {
            let parent = str_of(r, "parent");
            match str_of(r, "type") {
                "function" => {
                    assert!(groups.insert((parent, str_of(r, "name"))), "{r}");
                }
                "struct" => {}
                _ => continue,
            }
            assert!(
                keys.contains(&parent.to_string()),
                "parent of {r} is no key"
            );
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_unit_utf16_offsets() {
        let dir = Scratch::new("bin-unit-wide");
        unit_tree(&dir);
        let root = dir.root();
        let run = psfrontend(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let path = format!("{root}/u.ps1");
        let recs = records(&run.stdout);
        let f = recs
            .iter()
            .find(|r| r["type"] == "function" && r["name"] == "Get-U")
            .expect("Get-U");
        assert_eq!(str_of(f, "path"), path);
        let raw = fs::read(&path).expect("read u.ps1");
        let at = |k: &str| f[k].as_u64().expect(k) as usize;
        let units: Vec<u16> = raw[at("start")..at("end")]
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&units).expect("UTF-16 slice"),
            "function Get-U {\n    'é'\n}"
        );
        assert_eq!((at("start_line"), at("end_line")), (2, 4));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_unit_targets_parity() {
        let dir = Scratch::new("bin-unit-tgt");
        unit_tree(&dir);
        let root = dir.root();
        let full = psfrontend(&[&root]);
        assert!(full.ok, "{:?}", full.stderr);
        let full = records(&full.stdout);
        let side = Scratch::new("bin-unit-tgt-side");
        for p in UNIT_TARGETS {
            let path = format!("{root}/{p}");
            let list = side.write("targets.txt", format!("{path}\n").as_bytes());
            let run = psfrontend(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
            assert!(run.ok, "{:?}", run.stderr);
            let want = file_units(&full, &path);
            assert!(want.len() >= 3, "{p}");
            let got: Vec<_> = records(&run.stdout)
                .into_iter()
                .filter(|r| matches!(str_of(r, "type"), "struct" | "function" | "contains"))
                .collect();
            assert_eq!(got, want, "{p}");
        }
    }

    #[test]
    #[ignore = "e2e tier: real process; run via cargo test e2e:: -- --ignored"]
    fn bin_fails_without_arguments() {
        let run = psfrontend(&[]);
        assert!(!run.ok);
        let last = run.stderr.last().expect("a stderr line");
        assert!(last.starts_with("psfrontend: usage: "), "{last}");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_file_deps_main() {
        let (_dir, root, run) = dep_scan("bin-dep-main");
        let recs = records(&run.stdout);
        let main = format!("{root}/main.ps1");
        let id = script_id(&recs, &main);
        let dep = |p: &str, k: &str| (format!("{root}/{p}"), k.to_string());
        let uses = uses_of(&recs, id);
        assert_eq!(
            uses,
            [
                dep("lib/Util.ps1", "dot-source"),
                dep("mods/Foo/Foo.psd1", "import-module"),
                dep("mods/Foo/Foo.psm1", "import-module"),
            ]
        );
        let files: Vec<String> = file_recs(&recs).into_iter().map(|f| f.0).collect();
        assert!(uses.iter().all(|(to, _)| files.contains(to)), "{files:?}");
        let unresolved = unresolved_of(&recs, id);
        let named: Vec<(&str, &str)> = unresolved
            .iter()
            .filter(|(to, _)| !to.starts_with("<nameless@"))
            .map(|(t, c)| (t.as_str(), c.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("Bar", "unknown"),
                ("Microsoft.PowerShell.Utility", "stdlib"),
                ("PSReadLine", "external"),
            ]
        );
        // each sentinel's offset starts its site's command in the on-disk bytes
        let raw = fs::read(&main).expect("read main.ps1");
        let mut sites: Vec<&str> = unresolved
            .iter()
            .filter(|(to, _)| to.starts_with("<nameless@"))
            .map(|(to, cat)| {
                assert_eq!(cat, "unknown", "{to}");
                let tail = &raw[sentinel_offset(to, "main.ps1")..];
                [". $x", "Import-Module $m"]
                    .into_iter()
                    .find(|c| tail.starts_with(c.as_bytes()))
                    .unwrap_or_else(|| panic!("{to} starts no site"))
            })
            .collect();
        sites.sort();
        assert_eq!(sites, [". $x", "Import-Module $m"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_file_deps_utf16_sentinel() {
        let (_dir, root, run) = dep_scan("bin-dep-wide");
        let recs = records(&run.stdout);
        let path = format!("{root}/u.ps1");
        let unresolved = unresolved_of(&recs, script_id(&recs, &path));
        assert_eq!(unresolved.len(), 1, "{unresolved:?}");
        let (to, cat) = &unresolved[0];
        assert_eq!(cat, "unknown");
        let raw = fs::read(&path).expect("read u.ps1");
        let units: Vec<u16> = raw[sentinel_offset(to, "u.ps1")..]
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let tail = String::from_utf16(&units).expect("UTF-16 slice");
        assert!(tail.starts_with(". $y"), "{tail:?}");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_file_deps_syntax_error() {
        let (_dir, root, run) = dep_scan("bin-dep-err");
        let path = format!("{root}/broken.ps1");
        let mut expected: Vec<String> =
            psfrontend::diag::error_regions(&path, DEP_BROKEN, &parse(DEP_BROKEN))
                .iter()
                .map(|d| format!("ps: {d}"))
                .collect();
        assert!(!expected.is_empty());
        expected.push("ps: 1 file had syntax errors".to_string());
        assert_eq!(run.stderr, expected);
        let recs = records(&run.stdout);
        assert_eq!(
            uses_of(&recs, script_id(&recs, &path)),
            [(format!("{root}/lib/Util.ps1"), "dot-source".to_string())]
        );
        // the scan continued: every fixture file, those walked after broken.ps1 included
        let mut files: Vec<String> = file_recs(&recs)
            .into_iter()
            .map(|f| {
                f.0.strip_prefix(&format!("{root}/"))
                    .expect("in root")
                    .to_string()
            })
            .collect();
        files.sort();
        assert_eq!(
            files,
            [
                "broken.ps1",
                "dot_then_import.ps1",
                "import_then_dot.ps1",
                "lib/Util.ps1",
                "main.ps1",
                "mods/Bar/Bar.psd1",
                "mods/Foo/Foo.psd1",
                "mods/Foo/Foo.psm1",
                "other/Bar/Bar.psd1",
                "two_imports.ps1",
                "u.ps1",
            ]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_file_deps_records_valid() {
        let (_dir, _root, run) = dep_scan("bin-dep-valid");
        let recs = records(&run.stdout);
        let types = [
            "module",
            "file",
            "struct",
            "function",
            "contains",
            "uses",
            "unresolved_use",
            "unresolved",
        ];
        assert!(
            recs.iter().all(|r| types.contains(&str_of(r, "type"))),
            "{recs:?}"
        );
        let fqns: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "unresolved")
            .map(|r| str_of(r, "fqn"))
            .collect();
        let unique: std::collections::HashSet<&&str> = fqns.iter().collect();
        assert_eq!(unique.len(), fqns.len(), "{fqns:?}");
        // every unresolved_use names an emitted unresolved record
        for r in recs.iter().filter(|r| r["type"] == "unresolved_use") {
            assert!(fqns.contains(&str_of(r, "to")), "{r}");
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_file_deps_targets_parity() {
        let (_dir, root, full) = dep_scan("bin-dep-tgt");
        let full = records(&full.stdout);
        let main = format!("{root}/main.ps1");
        let id = script_id(&full, &main).to_string();
        let named: Vec<&str> = full
            .iter()
            .filter(|r| r["type"] == "unresolved_use" && r["from"] == id.as_str())
            .map(|r| str_of(r, "to"))
            .collect();
        let deps = |recs: &[serde_json::Value]| -> Vec<serde_json::Value> {
            recs.iter()
                .filter(|r| match str_of(r, "type") {
                    "uses" | "unresolved_use" => r["from"] == id.as_str(),
                    "unresolved" => named.contains(&str_of(r, "fqn")),
                    _ => false,
                })
                .cloned()
                .collect()
        };
        let want = deps(&full);
        let side = Scratch::new("bin-dep-tgt-side");
        let list = side.write("targets.txt", format!("{main}\n").as_bytes());
        let run = psfrontend(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        let got = records(&run.stdout);
        let dep_types = ["uses", "unresolved_use", "unresolved"];
        let got_deps: Vec<_> = got
            .iter()
            .filter(|r| dep_types.contains(&str_of(r, "type")))
            .cloned()
            .collect();
        assert_eq!(got_deps, want);
        // the dropped files keep their paths as uses targets
        assert_eq!(file_recs(&got).len(), 1);
        assert_eq!(uses_of(&got, &id).len(), 3);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_file_deps_kind_precedence() {
        let (_dir, root, run) = dep_scan("bin-dep-kind");
        let recs = records(&run.stdout);
        let psm1 = format!("{root}/mods/Foo/Foo.psm1");
        for s in [
            "dot_then_import.ps1",
            "import_then_dot.ps1",
            "two_imports.ps1",
        ] {
            let id = script_id(&recs, &format!("{root}/{s}"));
            let to_psm1: Vec<_> = recs
                .iter()
                .filter(|r| r["type"] == "uses" && r["from"] == id && str_of(r, "to") == psm1)
                .collect();
            assert_eq!(to_psm1.len(), 1, "{s}");
            assert_eq!(
                to_psm1[0]["properties"],
                serde_json::json!({"kind": "dot-source"}),
                "{s}"
            );
        }
    }
}
