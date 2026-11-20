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

/// The call tree's main.ps1: dot-sources, class W, and every command/call/type form.
const CALL_MAIN: &str = r#". ./lib/util.ps1
. ./types.ps1
class W : Base {
    [Color]$c
    [System.String]$s
    [string]$t
    [ciminstance]$ci
}
Util
global:Util
Get-P
Hide-B
Get-A
Get-ChildItem
Compress-Archive
Get-CimInstance
git
sort
.\types.ps1
Foo\Get-A
Foo\Hide-B
Microsoft.PowerShell.Archive\Compress-Archive
CimCmdlets\Get-CimInstance
& ./lib/util.ps1
& { 1 }
[Pair]::new(1)
[Color]::Red()
[System.IO.Path]::GetTempPath()
[math]::Round(1.5)
"#;

/// The call tree's syntax-error script: a dot-source and a call before a local error.
const CALL_BROKEN: &str = ". ./lib/util.ps1\nUtil\n$q = 1 )\n";

/// Writes the command/call/type tree into a fresh scratch dir and runs the binary over it.
fn call_scan(tag: &str) -> (Scratch, String, Run) {
    let dir = Scratch::new(tag);
    dir.write(
        "mods/Foo/Foo.psd1",
        b"@{\n    RootModule = 'Foo.psm1'\n    FunctionsToExport = @('Get-*')\n}\n",
    );
    dir.write(
        "mods/Foo/Foo.psm1",
        b". $PSScriptRoot\\priv.ps1\nfunction Get-A {}\nfunction Hide-B {}\n",
    );
    dir.write("mods/Foo/priv.ps1", b"function Get-P {}\n");
    dir.write("mods/Bar/Bar.psd1", b"@{\n    RootModule = 'Bar.psm1'\n}\n");
    dir.write("mods/Bar/Bar.psm1", b"function Get-A {}\n");
    dir.write(
        "lib/util.ps1",
        b"function Util {}\nclass Shape {}\n$x.sort()\n",
    );
    dir.write(
        "types.ps1",
        b"class Base {}\nenum Color { Red; Green }\nclass Pair {\n    Pair([int]$a) {}\n    [void] Run() { $this.Run() }\n}\n",
    );
    dir.write("main.ps1", CALL_MAIN.as_bytes());
    dir.write("u.ps1", &utf16le_bom("# ünï 😀\n& $sb\n"));
    dir.write("broken.ps1", CALL_BROKEN.as_bytes());
    let root = dir.root();
    let run = psfrontend(&[&root]);
    assert!(run.ok, "{:?}", run.stderr);
    (dir, root, run)
}

/// The id of the one Struct/Function record named `name` located at `path`.
fn unit_id<'r>(recs: &'r [serde_json::Value], path: &str, name: &str) -> &'r str {
    let ids: Vec<&str> = recs
        .iter()
        .filter(|r| matches!(str_of(r, "type"), "struct" | "function"))
        .filter(|r| str_of(r, "path") == path && str_of(r, "name") == name)
        .map(|r| str_of(r, "id"))
        .collect();
    assert_eq!(ids.len(), 1, "{path} {name}");
    ids[0]
}

/// The identity key `ps_rendered` gives the Struct/Function record `id`.
fn key_of(recs: &[serde_json::Value], id: &str) -> String {
    recs.iter()
        .filter(|r| matches!(str_of(r, "type"), "module" | "struct" | "function"))
        .zip(ps_rendered(recs))
        .find(|(r, _)| r["id"] == id)
        .map(|(_, (k, _))| k)
        .unwrap_or_else(|| panic!("no record {id}"))
}

/// The sorted, deduplicated `to` of the `ty` records leaving `from`.
fn edges_from(recs: &[serde_json::Value], ty: &str, from: &str) -> Vec<String> {
    let mut out: Vec<String> = recs
        .iter()
        .filter(|r| r["type"] == ty && r["from"] == from)
        .map(|r| str_of(r, "to").to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The category of the unresolved record `fqn`.
fn category<'r>(recs: &'r [serde_json::Value], fqn: &str) -> &'r str {
    recs.iter()
        .find(|r| r["type"] == "unresolved" && r["fqn"] == fqn)
        .map_or("<none>", |r| str_of(r, "category"))
}

/// The conformance tree's main.ps1: a dot-source, three imports and every top-level command form.
const CONF_MAIN: &str = r#". $PSScriptRoot\tools.ps1
Import-Module Widgets
Import-Module ../conf/Widgets/Nested/Inner.psm1
Import-Module $m
Use-Tool
Get-Widget
Get-Inner
Invoke-Work
Hide-Inner
Remove-Widget
Select-Even
Widgets\Get-Widget
Get-ChildItem
gci
Microsoft.PowerShell.Utility\Write-Output x
git status
& { 1 }
[Widget]::new(3)
[System.IO.Path]::GetTempPath()
"#;

/// The conformance tree: eight files combining every covered construct.
const CONF_FILES: [(&str, &str); 8] = [
    (
        "conf/Widgets/Widgets.psd1",
        "@{\n    RootModule = 'Widgets.psm1'\n    NestedModules = @('Nested\\Inner.psm1', '.\\lib\\Helpers.ps1')\n    FunctionsToExport = @('Get-*', 'Invoke-Work')\n}\n",
    ),
    (
        "conf/Widgets/Widgets.psm1",
        r#". $PSScriptRoot\lib\Private.ps1
function Get-Widget { Select-Even; Format-Widget; Get-Private }
filter Select-Even { $_ }
workflow Invoke-Work { Get-Widget }
function Remove-Widget { }
class Widget : Base {
    [Shape]$Kind
    Widget([int]$n) { }
    [string] Describe() { return $this.Name() }
    [string] Name() { return 'w' }
}
enum Shape { Round; Square }
"#,
    ),
    (
        "conf/Widgets/Nested/Inner.psm1",
        "function Get-Inner { }\nfunction Hide-Inner { }\n",
    ),
    ("conf/Widgets/lib/Helpers.ps1", "function Format-Widget { }\n"),
    (
        "conf/Widgets/lib/Private.ps1",
        "function Get-Private { }\nclass Base { }\n",
    ),
    ("app/tools.ps1", "function Use-Tool { }\n"),
    (
        "app/extras.ps1",
        r#"using module ../conf/Widgets/Widgets.psm1
function Get-One { param($a) $a }
filter Get-Two { param($b) $b }
workflow Get-Three { param($c) Get-One }
enum Size : byte { Small }
class Bag : System.Collections.Generic.List[int] { }
class Pipe : System.IO.Stream { }
configuration Web { Node 'localhost' { File Site { } } }
"#,
    ),
    ("app/main.ps1", CONF_MAIN),
];

/// Writes the conformance tree into a fresh scratch dir and runs the binary over it.
fn conformance_scan(tag: &str) -> (Scratch, String, Run) {
    let dir = Scratch::new(tag);
    for (name, text) in CONF_FILES {
        dir.write(name, text.as_bytes());
    }
    let root = dir.root();
    let run = psfrontend(&[&root]);
    (dir, root, run)
}

/// `recs` as sorted compact JSON lines: ids mapped to identity keys (`key_of`), the `{root}/`
/// prefix stripped from paths, and ids, byte offsets, lines and `file` dropped from every
/// record kind; with `placeholder`, each `<nameless@path:N>` sentinel's `N` becomes `#`.
fn normalise_records(recs: &[serde_json::Value], root: &str, placeholder: bool) -> Vec<String> {
    let ids: Vec<&str> = recs
        .iter()
        .filter(|r| matches!(str_of(r, "type"), "struct" | "function"))
        .map(|r| str_of(r, "id"))
        .collect();
    let prefix = format!("{root}/");
    let mut out: Vec<String> = recs
        .iter()
        .map(|r| {
            let mut r = r.clone();
            let m = r.as_object_mut().expect("object record");
            for k in ["id", "start", "end", "start_line", "end_line", "file"] {
                m.remove(k);
            }
            for k in ["path", "from", "to"] {
                if let Some(v) = m.get(k).and_then(|v| v.as_str()).map(str::to_string) {
                    let v = if ids.contains(&v.as_str()) {
                        key_of(recs, &v)
                    } else {
                        v.strip_prefix(&prefix).unwrap_or(&v).to_string()
                    };
                    m.insert(k.to_string(), v.into());
                }
            }
            for k in ["fqn", "to"] {
                let sentinel = m
                    .get(k)
                    .and_then(|v| v.as_str())
                    .filter(|s| placeholder && s.starts_with("<nameless@"))
                    .and_then(|s| s.rsplit_once(':'));
                if let Some((head, _)) = sentinel {
                    let v = format!("{head}:#>");
                    m.insert(k.to_string(), v.into());
                }
            }
            r.to_string()
        })
        .collect();
    out.sort();
    out
}

/// A byte range in a fixture text.
type Bytes = std::ops::Range<usize>;

/// The region oracle's report on one fixture text, walked from the parse tree itself.
#[derive(Debug)]
struct Regions {
    /// The root node's kind.
    root: &'static str,
    /// Each outermost ERROR node (an ERROR root included): (its byte range, its 1-based
    /// (line, character column), the byte range of the `source_file` child statement holding it).
    errors: Vec<(Bytes, (usize, usize), Bytes)>,
    /// Each MISSING node outside the ERROR regions: (its kind, its 1-based (line, column),
    /// the byte range of the `source_file` child statement holding it).
    missing: Vec<(&'static str, (usize, usize), Bytes)>,
    /// Each function_definition/class_definition inside an ERROR node: (kind, declared name,
    /// index of the region holding it).
    inside: Vec<(&'static str, String, usize)>,
    /// The byte range of each `source_file` child statement.
    stmts: Vec<Bytes>,
}

/// 1-based (line, character column) of `byte` in `src`.
fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let before = &src[..byte];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        1 + before.matches('\n').count(),
        1 + before[line_start..].chars().count(),
    )
}

/// Parses `src` through `psfrontend::parse` and reports its error regions (see `Regions`).
fn regions(src: &str) -> Regions {
    fn walk(n: Node, src: &str, region: Option<usize>, out: &mut Regions) {
        // the source_file child holding `n` (the root itself when `n` is the root)
        let stmt = std::iter::successors(Some(n), Node::parent)
            .find(|a| a.parent().is_none_or(|p| p.parent().is_none()))
            .expect("ancestor")
            .byte_range();
        let mut region = region;
        if n.is_error() && region.is_none() {
            out.errors
                .push((n.byte_range(), line_col(src, n.start_byte()), stmt.clone()));
            region = Some(out.errors.len() - 1);
        } else if n.is_missing() && region.is_none() {
            out.missing
                .push((n.kind(), line_col(src, n.start_byte()), stmt.clone()));
        }
        if let (Some(r), "function_definition" | "class_definition") = (region, n.kind()) {
            let name = n
                .child_by_field_name("name")
                .map_or("", |c| &src[c.byte_range()]);
            out.inside.push((n.kind(), name.to_string(), r));
        }
        let mut cur = n.walk();
        for child in n.children(&mut cur) {
            walk(child, src, region, out);
        }
    }
    let tree = parse(src);
    let root = tree.root_node();
    let mut out = Regions {
        root: root.kind(),
        errors: Vec::new(),
        missing: Vec::new(),
        inside: Vec::new(),
        stmts: Vec::new(),
    };
    if root.kind() == "source_file" {
        let mut cur = root.walk();
        out.stmts = root.children(&mut cur).map(|c| c.byte_range()).collect();
    }
    walk(root, src, None, &mut out);
    out
}

/// The recovery tree's first broken statement: an until with no condition. The pinned grammar
/// reduces the do body (its declarations included) before failing, so the ERROR holds them.
const RECOVERY_BROKEN_1: &str = "do {\n. ./inside.ps1\nImport-Module InsideMod\nInside-Cmd\nfunction Inside-Fn { Helper-Fn }\nclass InsideClass { [int] Go() { return 1 } }\n} until";

/// The recovery tree's second broken statement: a stray closing parenthesis.
const RECOVERY_BROKEN_2: &str = "$q = 1 )";

/// err.ps1 with `b1`/`b2` as its two broken statements (both empty in the clean twin).
fn recovery_err(b1: &str, b2: &str) -> String {
    format!(
        ". ./helper.ps1\n\nImport-Module Dep\n\nfunction Before-Fn {{ param($a) Helper-Fn; Get-ChildItem }}\n\nfilter Before-Filter {{ $_ }}\n\nclass BeforeClass : HelperBase {{ [BeforeEnum]$e; [int] Run() {{ return $this.Run() }} }}\n\nenum BeforeEnum {{ A; B }}\n\n& {{ 1 }}\n\nExt-Before\n\n{b1}\n\nworkflow Mid-Flow {{ param($w) Before-Fn }}\n\nExt-Mid\n\n{b2}\n\nfunction After-Fn {{ Helper-Fn; git }}\n\nclass AfterClass {{ AfterClass() {{ }} }}\n\nenum AfterEnum {{ X }}\n\n[AfterClass]::new()\n\nImport-Module ./mods/Dep/Dep.psm1\n\n. $x\n\nExt-After\n"
    )
}

/// missing.ps1 with `close` before the `;` ending its unclosed parenthesis (`)` in the clean twin).
fn recovery_missing(close: &str) -> String {
    format!("function Miss-Before {{ }}\n\n$y = (1 + 2{close};\n\nfunction Miss-After {{ Miss-Before }}\n")
}

/// The recovery tree's clean files: (name, text).
const RECOVERY_CLEAN: [(&str, &str); 6] = [
    ("a_before.ps1", "function Early { }\n"),
    (
        "helper.ps1",
        "function Helper-Fn { }\nclass HelperBase { }\n",
    ),
    ("inside.ps1", "function Inside-Lib { }\n"),
    ("mods/Dep/Dep.psd1", "@{\n    RootModule = 'Dep.psm1'\n}\n"),
    ("mods/Dep/Dep.psm1", "function Get-Dep { }\n"),
    (
        "z_after.ps1",
        ". ./err.ps1\nAfter-Fn\nMid-Flow\n[AfterClass]::new()\n",
    ),
];

/// Writes the recovery tree (its clean twin when `twin`, err.ps1 UTF-16LE with BOM when `wide`)
/// into a fresh scratch dir and runs the binary over it.
fn recovery_scan(tag: &str, twin: bool, wide: bool) -> (Scratch, String, Run) {
    let dir = Scratch::new(tag);
    for (name, text) in RECOVERY_CLEAN {
        dir.write(name, text.as_bytes());
    }
    let (err, missing) = if twin {
        (recovery_err("", ""), recovery_missing(")"))
    } else {
        (
            recovery_err(RECOVERY_BROKEN_1, RECOVERY_BROKEN_2),
            recovery_missing(""),
        )
    };
    let err = if wide {
        utf16le_bom(&err)
    } else {
        err.into_bytes()
    };
    dir.write("err.ps1", &err);
    dir.write("missing.ps1", missing.as_bytes());
    let root = dir.root();
    let run = psfrontend(&[&root]);
    (dir, root, run)
}

/// Normalised lines grouped by their record `type`.
fn by_kind(lines: &[String]) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for l in lines {
        let v: serde_json::Value = serde_json::from_str(l).expect("normalised JSON");
        out.entry(str_of(&v, "type").to_string())
            .or_default()
            .push(l.clone());
    }
    out
}

/// The whole-file-region tree: whole.ps1 ends inside an unclosed block holding a further
/// unclosed construct.
const WHOLE_PS1: &str =
    ". ./a_first.ps1\nImport-Module Dep\nfunction Keep { }\nGet-A\nfunction F { Get-B\n$x = (1 + \n";

/// The whole-file-region tree's other files: (name, text).
const WHOLE_OTHERS: [(&str, &str); 4] = [
    ("a_first.ps1", "function Early-W { }\n"),
    ("mods/Dep/Dep.psd1", "@{\n    RootModule = 'Dep.psm1'\n}\n"),
    ("mods/Dep/Dep.psm1", "function Get-Dep { }\n"),
    ("z_next.ps1", "function Late { }\nLate\nEarly-W\n"),
];

/// The byte range of `s`'s first occurrence in `text`.
fn span(text: &str, s: &str) -> Bytes {
    let n = text
        .find(s)
        .unwrap_or_else(|| panic!("{s:?} not in fixture"));
    n..n + s.len()
}

/// Whether `inner` lies within `outer`.
fn within(inner: &Bytes, outer: &Bytes) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
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
        // the plain run's records with every id re-prefixed `p`; unresolved names and paths kept
        let mut expected = records(&plain.stdout);
        let ids: Vec<String> = expected
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        for r in &mut expected {
            for k in ["id", "from", "to"] {
                if let Some(v) = r
                    .get_mut(k)
                    .filter(|v| ids.iter().any(|i| v.as_str() == Some(i)))
                {
                    *v = format!("p{}", &v.as_str().expect("id")[1..]).into();
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

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_main() {
        let (_dir, root, run) = call_scan("bin-call-main");
        let recs = records(&run.stdout);
        let p = |rel: &str| format!("{root}/{rel}");
        let main = p("main.ps1");
        let script = unit_id(&recs, &main, "<script>");
        // Foo\Get-A is stem Foo's export, Bar's Get-A ignored; .\types.ps1 a path command
        let mut want = vec![
            unit_id(&recs, &p("lib/util.ps1"), "Util"),
            unit_id(&recs, &p("mods/Foo/priv.ps1"), "Get-P"),
            unit_id(&recs, &p("lib/util.ps1"), "<script>"),
            unit_id(&recs, &p("types.ps1"), "<script>"),
            unit_id(&recs, &p("mods/Foo/Foo.psm1"), "Get-A"),
            unit_id(&recs, &p("types.ps1"), "new"),
        ];
        want.sort();
        assert_eq!(edges_from(&recs, "calls", script), want);
        let called = edges_from(&recs, "unresolved_call", script);
        let named: Vec<(&str, &str)> = called
            .iter()
            .filter(|to| !to.starts_with("<nameless@"))
            .map(|to| (to.as_str(), category(&recs, to)))
            .collect();
        // Get-ChildItem (engine module) and Compress-Archive (in-box Microsoft.PowerShell.Archive)
        // per the PowerShell v7.6.0 src/Modules manifests and InitialSessionState.cs
        let mut want = vec![
            ("Hide-B", "external"),
            ("Get-A", "unknown"),
            ("Get-ChildItem", "stdlib"),
            ("Compress-Archive", "stdlib"),
            ("Get-CimInstance", "external"),
            ("git", "external"),
            ("sort", "unknown"),
            ("Foo\\Hide-B", "external"),
            ("Microsoft.PowerShell.Archive\\Compress-Archive", "stdlib"),
            ("CimCmdlets\\Get-CimInstance", "external"),
            ("Color.Red", "unknown"),
            ("System.IO.Path.GetTempPath", "stdlib"),
            ("math.Round", "external"),
        ];
        want.sort();
        assert_eq!(named, want);
        let sentinels: Vec<&String> = called
            .iter()
            .filter(|to| to.starts_with("<nameless@"))
            .collect();
        assert_eq!(sentinels.len(), 1, "{sentinels:?}");
        assert_eq!(category(&recs, sentinels[0]), "func-value");
        let raw = fs::read(&main).expect("read main.ps1");
        assert!(raw[sentinel_offset(sentinels[0], "main.ps1")..].starts_with(b"&"));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_class_type_uses() {
        let (_dir, root, run) = call_scan("bin-call-type");
        let recs = records(&run.stdout);
        let p = |rel: &str| format!("{root}/{rel}");
        let main = p("main.ps1");
        let w = unit_id(&recs, &main, "W");
        let uses: Vec<_> = recs
            .iter()
            .filter(|r| r["type"] == "uses" && r["from"] == w)
            .collect();
        assert!(
            uses.iter().all(|r| r.get("properties").is_none()),
            "{uses:?}"
        );
        let mut want = vec![
            unit_id(&recs, &p("types.ps1"), "Base"),
            unit_id(&recs, &p("types.ps1"), "Color"),
        ];
        want.sort();
        assert_eq!(edges_from(&recs, "uses", w), want);
        // string is the accelerator for System.String
        let u = |n: &str, c: &str| (n.to_string(), c.to_string());
        assert_eq!(
            unresolved_of(&recs, w),
            [
                u("System.String", "stdlib"),
                u("ciminstance", "external"),
                u("string", "stdlib")
            ]
        );
        let dot = |rel: &str| (p(rel), "dot-source".to_string());
        assert_eq!(
            uses_of(&recs, script_id(&recs, &main)),
            [dot("lib/util.ps1"), dot("types.ps1")]
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_this_recursion() {
        let (_dir, root, run) = call_scan("bin-call-this");
        let recs = records(&run.stdout);
        let run_id = unit_id(&recs, &format!("{root}/types.ps1"), "Run");
        assert_eq!(edges_from(&recs, "calls", run_id), [run_id]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_utf16_sentinel() {
        let (_dir, root, run) = call_scan("bin-call-wide");
        let recs = records(&run.stdout);
        let path = format!("{root}/u.ps1");
        let called = edges_from(&recs, "unresolved_call", unit_id(&recs, &path, "<script>"));
        assert_eq!(called.len(), 1, "{called:?}");
        assert_eq!(category(&recs, &called[0]), "func-value");
        let raw = fs::read(&path).expect("read u.ps1");
        let units: Vec<u16> = raw[sentinel_offset(&called[0], "u.ps1")..]
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let tail = String::from_utf16(&units).expect("UTF-16 slice");
        assert!(tail.starts_with("& $sb"), "{tail:?}");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_syntax_error() {
        let (_dir, root, run) = call_scan("bin-call-err");
        let path = format!("{root}/broken.ps1");
        let mut expected: Vec<String> =
            psfrontend::diag::error_regions(&path, CALL_BROKEN, &parse(CALL_BROKEN))
                .iter()
                .map(|d| format!("ps: {d}"))
                .collect();
        assert!(!expected.is_empty());
        expected.push("ps: 1 file had syntax errors".to_string());
        assert_eq!(run.stderr, expected);
        let recs = records(&run.stdout);
        let util = format!("{root}/lib/util.ps1");
        assert_eq!(
            uses_of(&recs, script_id(&recs, &path)),
            [(util.clone(), "dot-source".to_string())]
        );
        // resolution step 2, the dot-sourced file's functions, under error recovery
        assert_eq!(
            edges_from(&recs, "calls", unit_id(&recs, &path, "<script>")),
            [unit_id(&recs, &util, "Util")]
        );
        assert!(
            !recs
                .iter()
                .any(|r| r["type"] == "unresolved_call" && r["to"] == "Util"),
            "{recs:?}"
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_records_valid() {
        let (_dir, _root, run) = call_scan("bin-call-valid");
        let recs = records(&run.stdout);
        let types = [
            "module",
            "file",
            "struct",
            "function",
            "contains",
            "calls",
            "uses",
            "unresolved_call",
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
        let ids: Vec<&str> = recs
            .iter()
            .filter(|r| matches!(str_of(r, "type"), "struct" | "function"))
            .map(|r| str_of(r, "id"))
            .collect();
        let files: Vec<String> = file_recs(&recs).into_iter().map(|f| f.0).collect();
        for r in &recs {
            let to = str_of(r, "to");
            match str_of(r, "type") {
                "calls" => assert!(ids.contains(&to), "{r}"),
                "uses" if r["properties"]["kind"] == "dot-source" => {
                    assert!(files.iter().any(|f| f == to), "{r}")
                }
                "uses" => assert!(ids.contains(&to), "{r}"),
                "unresolved_call" | "unresolved_use" => assert!(fqns.contains(&to), "{r}"),
                _ => {}
            }
        }
        // stdlib at main.ps1's command site, unknown at lib/util.ps1's instance call
        let sort: Vec<_> = recs
            .iter()
            .filter(|r| r["type"] == "unresolved" && r["fqn"] == "sort")
            .collect();
        assert_eq!(sort.len(), 1);
        assert_eq!(str_of(sort[0], "category"), "unknown");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_calls_targets_parity() {
        let (_dir, root, full) = call_scan("bin-call-tgt");
        let full = records(&full.stdout);
        let main = format!("{root}/main.ps1");
        let kept: Vec<&str> = full
            .iter()
            .filter(|r| {
                matches!(str_of(r, "type"), "struct" | "function") && str_of(r, "path") == main
            })
            .map(|r| str_of(r, "id"))
            .collect();
        let all: Vec<&str> = full
            .iter()
            .filter(|r| matches!(str_of(r, "type"), "struct" | "function"))
            .map(|r| str_of(r, "id"))
            .collect();
        let edge_types = ["calls", "unresolved_call", "uses", "unresolved_use"];
        let named: Vec<&str> = full
            .iter()
            .filter(|r| matches!(str_of(r, "type"), "unresolved_call" | "unresolved_use"))
            .filter(|r| kept.contains(&str_of(r, "from")))
            .map(|r| str_of(r, "to"))
            .collect();
        // main.ps1's records, each `to` naming a dropped record rewritten to its identity key
        let want: Vec<serde_json::Value> = full
            .iter()
            .filter(|r| match str_of(r, "type") {
                "unresolved" => named.contains(&str_of(r, "fqn")),
                t => edge_types.contains(&t) && kept.contains(&str_of(r, "from")),
            })
            .map(|r| {
                let mut r = r.clone();
                let to = str_of(&r, "to").to_string();
                if all.contains(&to.as_str()) && !kept.contains(&to.as_str()) {
                    r["to"] = key_of(&full, &to).into();
                }
                r
            })
            .collect();
        let side = Scratch::new("bin-call-tgt-side");
        let list = side.write("targets.txt", format!("{main}\n").as_bytes());
        let run = psfrontend(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        let got: Vec<serde_json::Value> = records(&run.stdout)
            .into_iter()
            .filter(|r| edge_types.contains(&str_of(r, "type")) || r["type"] == "unresolved")
            .collect();
        // no broken.ps1 edge survives: every kept edge leaves main.ps1
        assert_eq!(got, want);
        // the rewrite pinned: Util's and Base's full-run keys
        let script = unit_id(&full, &main, "<script>");
        let util = unit_id(&full, &format!("{root}/lib/util.ps1"), "Util");
        assert!(
            edges_from(&got, "calls", script).contains(&key_of(&full, util)),
            "{got:?}"
        );
        let base = unit_id(&full, &format!("{root}/types.ps1"), "Base");
        assert!(
            edges_from(&got, "uses", unit_id(&full, &main, "W")).contains(&key_of(&full, base)),
            "{got:?}"
        );
        assert_eq!(category(&got, "sort"), "unknown");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn bin_conformance_tree() {
        use serde_json::json;
        // (0) every fixture text parses to a source_file root with no ERROR/MISSING node
        for (name, text) in CONF_FILES {
            assert_eq!(
                parse_clean(text).root_node().kind(),
                "source_file",
                "{name}"
            );
        }
        let (_dir, root, run) = conformance_scan("bin-conf");
        // (7) exit 0, the summary last, no per-file diagnostic
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(
            run.stderr.last().map(String::as_str),
            Some("ps: 0 files had syntax errors")
        );
        let diag = format!("ps: {root}/");
        assert!(
            !run.stderr.iter().any(|l| l.starts_with(&diag)),
            "{:?}",
            run.stderr
        );
        // (8) determinism: a second run over the same tree, byte for byte
        assert_eq!(psfrontend(&[&root]).stdout, run.stdout);
        let recs = records(&run.stdout);

        // (6) closure: parents, contains/calls/uses endpoints and file-edge targets
        let modules = module_recs(&recs);
        let struct_keys: Vec<String> = recs
            .iter()
            .filter(|r| r["type"] == "struct")
            .map(|r| format!("{}.{}", str_of(r, "parent"), str_of(r, "name")))
            .collect();
        let ids: Vec<&str> = recs
            .iter()
            .filter(|r| matches!(str_of(r, "type"), "struct" | "function"))
            .map(|r| str_of(r, "id"))
            .collect();
        let files: Vec<String> = file_recs(&recs).into_iter().map(|f| f.0).collect();
        for r in &recs {
            let (from, to) = (str_of(r, "from"), str_of(r, "to"));
            match str_of(r, "type") {
                "struct" | "function" => {
                    let p = str_of(r, "parent");
                    assert!(
                        modules.iter().any(|m| m == p) || struct_keys.iter().any(|k| k == p),
                        "{r}"
                    );
                }
                "contains" | "calls" => assert!(ids.contains(&from) && ids.contains(&to), "{r}"),
                "uses" if r.get("properties").is_some() => {
                    assert!(ids.contains(&from) && files.iter().any(|f| f == to), "{r}")
                }
                "uses" => assert!(ids.contains(&from) && ids.contains(&to), "{r}"),
                _ => {}
            }
        }

        // the sentinel offsets: each site's first character in main.ps1's on-disk bytes
        let raw = fs::read(format!("{root}/app/main.ps1")).expect("read main.ps1");
        let site = |s: &str| {
            let n = CONF_MAIN.find(s).expect("site");
            assert!(raw[n..].starts_with(s.as_bytes()), "{s}");
            format!("<nameless@app/main.ps1:{n}>")
        };
        let (amp, imp) = (site("& { 1 }"), site("Import-Module $m"));

        let j = |v: serde_json::Value| v.to_string();
        let unit = |t: &str, parent: &str, name: &str, path: &str, params: &[&str]| {
            let mut v = json!({"type": t, "parent": parent, "name": name, "path": path});
            if !params.is_empty() {
                v["params"] = json!(params);
            }
            j(v)
        };
        let edge = |t: &str, from: &str, to: &str| j(json!({"type": t, "from": from, "to": to}));
        let k = |a: &str, b: &str| format!("{a}.{b}");

        let md = "conf/Widgets/Widgets.psd1";
        let lib = "conf/Widgets/lib";
        let (p_wm, p_in, p_he, p_pr) = (
            "conf/Widgets/Widgets.psm1",
            "conf/Widgets/Nested/Inner.psm1",
            "conf/Widgets/lib/Helpers.ps1",
            "conf/Widgets/lib/Private.ps1",
        );
        let (p_ma, p_to, p_ex) = ("app/main.ps1", "app/tools.ps1", "app/extras.ps1");
        let wm = k(md, "Widgets.psm1");
        let inn = k(md, "Nested/Inner.psm1");
        let he = k(md, "lib/Helpers.ps1");
        let pr = k(lib, "Private.ps1");
        let (ma, to, ex) = ("app.main.ps1", "app.tools.ps1", "app.extras.ps1");
        let wd = k(&wm, "Widget");

        let mut want: Vec<String> = Vec::new();
        // (1) modules and files
        for m in ["app", md, lib] {
            want.push(j(json!({"type": "module", "fqn": m})));
        }
        for (p, m) in [
            (p_ma, "app"),
            (p_to, "app"),
            (p_ex, "app"),
            (md, md),
            (p_wm, md),
            (p_in, md),
            (p_he, md),
            (p_pr, lib),
        ] {
            want.push(j(json!({"type": "file", "path": p, "parent": m})));
        }
        // (2) structs, functions and contains
        let scripts = [
            ("app", "main.ps1", p_ma),
            ("app", "tools.ps1", p_to),
            ("app", "extras.ps1", p_ex),
            (md, "Widgets.psm1", p_wm),
            (md, "Nested/Inner.psm1", p_in),
            (md, "lib/Helpers.ps1", p_he),
            (lib, "Private.ps1", p_pr),
        ];
        for (m, name, p) in scripts {
            let key = k(m, name);
            want.push(unit("struct", m, name, p, &[]));
            want.push(unit("function", &key, "<script>", p, &[]));
            want.push(edge("contains", &key, &k(&key, "<script>")));
        }
        let members: [(&str, &str, &str, &str, &[&str]); 21] = [
            ("function", &wm, "Get-Widget", p_wm, &[]),
            ("function", &wm, "Select-Even", p_wm, &[]),
            ("function", &wm, "Invoke-Work", p_wm, &[]),
            ("function", &wm, "Remove-Widget", p_wm, &[]),
            ("struct", &wm, "Widget", p_wm, &[]),
            ("struct", &wm, "Shape", p_wm, &[]),
            ("function", &wd, "new", p_wm, &["int"]),
            ("function", &wd, "Describe", p_wm, &[]),
            ("function", &wd, "Name", p_wm, &[]),
            ("function", &inn, "Get-Inner", p_in, &[]),
            ("function", &inn, "Hide-Inner", p_in, &[]),
            ("function", &he, "Format-Widget", p_he, &[]),
            ("function", &pr, "Get-Private", p_pr, &[]),
            ("struct", &pr, "Base", p_pr, &[]),
            ("function", to, "Use-Tool", p_to, &[]),
            ("function", ex, "Get-One", p_ex, &[]),
            ("function", ex, "Get-Two", p_ex, &[]),
            ("function", ex, "Get-Three", p_ex, &[]),
            ("struct", ex, "Size", p_ex, &[]),
            ("struct", ex, "Bag", p_ex, &[]),
            ("struct", ex, "Pipe", p_ex, &[]),
        ];
        for (t, parent, name, p, params) in members {
            want.push(unit(t, parent, name, p, params));
            want.push(edge("contains", parent, &k(parent, name)));
        }
        // (3) calls
        let script = k(ma, "<script>");
        for (from, to_key) in [
            (k(&wm, "Get-Widget"), k(&wm, "Select-Even")),
            (k(&wm, "Get-Widget"), k(&he, "Format-Widget")),
            (k(&wm, "Get-Widget"), k(&pr, "Get-Private")),
            (k(&wm, "Invoke-Work"), k(&wm, "Get-Widget")),
            (k(ex, "Get-Three"), k(ex, "Get-One")),
            (k(&wd, "Describe"), k(&wd, "Name")),
            (script.clone(), k(to, "Use-Tool")),
            (script.clone(), k(&wm, "Get-Widget")),
            (script.clone(), k(&inn, "Get-Inner")),
            (script.clone(), k(&wm, "Invoke-Work")),
            (script.clone(), k(&wd, "new")),
        ] {
            want.push(edge("calls", &from, &to_key));
        }
        // (4) uses: four file edges and two type edges
        for (from, file, kind) in [
            (wm.as_str(), p_pr, "dot-source"),
            (ma, p_to, "dot-source"),
            (ma, md, "import-module"),
            (ma, p_in, "import-module"),
        ] {
            want.push(j(
                json!({"type": "uses", "from": from, "to": file, "properties": {"kind": kind}}),
            ));
        }
        want.push(edge("uses", &wd, &k(&wm, "Shape")));
        want.push(edge("uses", &wd, &k(&pr, "Base")));
        // (5) twelve unresolved targets and their edges
        for (fqn, cat) in [
            ("Hide-Inner", "external"),
            ("Remove-Widget", "external"),
            ("Select-Even", "external"),
            ("git", "external"),
            ("Get-ChildItem", "stdlib"),
            ("gci", "stdlib"),
            ("Microsoft.PowerShell.Utility\\Write-Output", "stdlib"),
            ("System.IO.Path.GetTempPath", "stdlib"),
            (amp.as_str(), "func-value"),
        ] {
            want.push(j(
                json!({"type": "unresolved", "fqn": fqn, "category": cat}),
            ));
            want.push(edge("unresolved_call", &script, fqn));
        }
        for (from, fqn, cat) in [
            (ma.to_string(), imp.as_str(), "unknown"),
            (k(ex, "Bag"), "System.Collections.Generic.List", "stdlib"),
            (k(ex, "Pipe"), "System.IO.Stream", "stdlib"),
        ] {
            want.push(j(
                json!({"type": "unresolved", "fqn": fqn, "category": cat}),
            ));
            want.push(edge("unresolved_use", &from, fqn));
        }
        want.sort();
        assert_eq!(normalise_records(&recs, &root, false), want);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn recovery_oracle_preconditions() {
        let err = recovery_err(RECOVERY_BROKEN_1, RECOVERY_BROKEN_2);
        let missing = recovery_missing("");
        // every clean fixture, both twins included, parses error-free under a source_file root
        let mut clean: Vec<(&str, String)> = RECOVERY_CLEAN
            .iter()
            .map(|(n, t)| (*n, t.to_string()))
            .collect();
        clean.push(("err.ps1 twin", recovery_err("", "")));
        clean.push(("missing.ps1 twin", recovery_missing(")")));
        for (name, text) in &clean {
            let r = regions(text);
            assert_eq!(r.root, "source_file", "{name}: {r:?}");
            assert!(r.errors.is_empty() && r.missing.is_empty(), "{name}: {r:?}");
        }
        // err.ps1: two local ERROR regions, each within its own broken statement
        let r = regions(&err);
        println!("err.ps1 oracle: {r:?}");
        assert_eq!(r.root, "source_file", "{r:?}");
        assert!(r.missing.is_empty(), "{r:?}");
        let broken = [span(&err, RECOVERY_BROKEN_1), span(&err, RECOVERY_BROKEN_2)];
        assert_eq!(r.errors.len(), 2, "{r:?}");
        for ((range, _, stmt), b) in r.errors.iter().zip(&broken) {
            // the statement holding the region is the broken one: past its text only blank space
            let tail = &err[b.end..stmt.end.max(b.end)];
            assert!(
                within(range, stmt) && stmt.start >= b.start && tail.trim().is_empty(),
                "{r:?}"
            );
        }
        // missing.ps1: one MISSING ) within its statement, no ERROR
        let m = regions(&missing);
        println!("missing.ps1 oracle: {m:?}");
        assert_eq!(m.root, "source_file", "{m:?}");
        assert!(m.errors.is_empty(), "{m:?}");
        assert_eq!(m.missing.len(), 1, "{m:?}");
        assert_eq!(m.missing[0].0, ")", "{m:?}");
        let mb = span(&missing, "$y = (1 + 2;");
        assert!(within(&m.missing[0].2, &mb), "{m:?}");
        // every site outside the broken statements sits in an intact source_file child
        let intact = |r: &Regions, text: &str, s: &str| {
            let n = span(text, s).start;
            let stmt = r
                .stmts
                .iter()
                .find(|st| st.contains(&n))
                .unwrap_or_else(|| panic!("{s:?} in no statement: {r:?}"));
            assert!(
                !r.errors.iter().any(|e| &e.2 == stmt) && !r.missing.iter().any(|e| &e.2 == stmt),
                "{s:?} shares a statement with a region: {r:?}"
            );
            n
        };
        let sites = [
            ". ./helper.ps1",
            "Import-Module Dep\n",
            "function Before-Fn",
            "filter Before-Filter",
            "class BeforeClass",
            "enum BeforeEnum",
            "& { 1 }",
            "Ext-Before",
            "workflow Mid-Flow",
            "Ext-Mid",
            "function After-Fn",
            "class AfterClass",
            "enum AfterEnum",
            "[AfterClass]::new()",
            "Import-Module ./mods/Dep/Dep.psm1",
            ". $x",
            "Ext-After",
        ];
        let (mut before, mut between, mut after) = (0, 0, 0);
        for s in sites {
            let n = intact(&r, &err, s);
            assert!(!broken.iter().any(|b| b.contains(&n)), "{s:?}");
            match n {
                n if n < broken[0].start => before += 1,
                n if n < broken[1].start => between += 1,
                _ => after += 1,
            }
        }
        assert!(before > 0 && between > 0 && after > 0);
        assert!(intact(&m, &missing, "function Miss-Before") < mb.start);
        assert!(intact(&m, &missing, "function Miss-After") > mb.end);
        // the inside items, their declaration nodes included, lie in the first region
        for s in [". ./inside.ps1", "Import-Module InsideMod", "Inside-Cmd"] {
            assert!(within(&span(&err, s), &r.errors[0].0), "{s:?}: {r:?}");
        }
        for (kind, name) in [
            ("function_definition", "Inside-Fn"),
            ("class_definition", "InsideClass"),
        ] {
            assert!(
                r.inside.contains(&(kind, name.to_string(), 0)),
                "{kind} {name}: {r:?}"
            );
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn recovery_diagnostics() {
        let (_dir, root, run) = recovery_scan("rec-diag", false, false);
        assert!(run.ok, "{:?}", run.stderr);
        let r = regions(&recovery_err(RECOVERY_BROKEN_1, RECOVERY_BROKEN_2));
        let m = regions(&recovery_missing(""));
        assert_eq!((r.errors.len(), m.missing.len()), (2, 1));
        let mut want: Vec<String> = r
            .errors
            .iter()
            .map(|(_, (l, c), _)| format!("ps: {root}/err.ps1:{l}:{c}: syntax error"))
            .collect();
        want.extend(
            m.missing
                .iter()
                .map(|(k, (l, c), _)| format!("ps: {root}/missing.ps1:{l}:{c}: missing {k}")),
        );
        want.push("ps: 2 files had syntax errors".to_string());
        assert_eq!(run.stderr, want);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn recovery_completeness() {
        use serde_json::json;
        let (_dir, root, run) = recovery_scan("rec-full", false, false);
        let (_twin, troot, twin) = recovery_scan("rec-twin", true, false);
        assert!(run.ok, "{:?}", run.stderr);
        assert!(twin.ok, "{:?}", twin.stderr);
        assert_eq!(
            twin.stderr.last().map(String::as_str),
            Some("ps: 0 files had syntax errors")
        );
        let got = normalise_records(&records(&run.stdout), &root, true);
        let want = normalise_records(&records(&twin.stdout), &troot, true);
        assert_eq!(by_kind(&got), by_kind(&want));
        // the equality is not vacuous: the listed records are in it
        let j = |v: serde_json::Value| v.to_string();
        let f = |parent: &str, name: &str, path: &str| {
            j(json!({"type": "function", "parent": parent, "name": name, "path": path}))
        };
        let edge = |t: &str, from: &str, to: &str| j(json!({"type": t, "from": from, "to": to}));
        let (e, ms) = (".err.ps1", ".missing.ps1");
        let mut listed = vec![
            f(e, "Before-Fn", "err.ps1"),
            f(e, "Before-Filter", "err.ps1"),
            f(e, "Mid-Flow", "err.ps1"),
            f(e, "After-Fn", "err.ps1"),
            f(".err.ps1.BeforeClass", "Run", "err.ps1"),
            f(".err.ps1.AfterClass", "new", "err.ps1"),
            f(ms, "Miss-Before", "missing.ps1"),
            f(ms, "Miss-After", "missing.ps1"),
            f(".a_before.ps1", "Early", "a_before.ps1"),
            edge("calls", ".err.ps1.Before-Fn", ".helper.ps1.Helper-Fn"),
            edge(
                "calls",
                ".err.ps1.BeforeClass.Run",
                ".err.ps1.BeforeClass.Run",
            ),
            edge("calls", ".err.ps1.Mid-Flow", ".err.ps1.Before-Fn"),
            edge("calls", ".err.ps1.After-Fn", ".helper.ps1.Helper-Fn"),
            edge(
                "calls",
                ".missing.ps1.Miss-After",
                ".missing.ps1.Miss-Before",
            ),
            edge("calls", ".err.ps1.<script>", ".err.ps1.AfterClass.new"),
            edge("calls", ".z_after.ps1.<script>", ".err.ps1.After-Fn"),
            edge("calls", ".z_after.ps1.<script>", ".err.ps1.Mid-Flow"),
            edge("calls", ".z_after.ps1.<script>", ".err.ps1.AfterClass.new"),
            edge("uses", ".err.ps1.BeforeClass", ".helper.ps1.HelperBase"),
            edge("uses", ".err.ps1.BeforeClass", ".err.ps1.BeforeEnum"),
            edge("unresolved_call", ".err.ps1.Before-Fn", "Get-ChildItem"),
            edge("unresolved_call", ".err.ps1.After-Fn", "git"),
            edge(
                "unresolved_call",
                ".err.ps1.<script>",
                "<nameless@err.ps1:#>",
            ),
            edge("unresolved_use", e, "<nameless@err.ps1:#>"),
        ];
        for (to, kind) in [
            ("helper.ps1", "dot-source"),
            ("mods/Dep/Dep.psd1", "import-module"),
            ("mods/Dep/Dep.psm1", "import-module"),
        ] {
            listed.push(j(
                json!({"type": "uses", "from": e, "to": to, "properties": {"kind": kind}}),
            ));
        }
        for (fqn, cat) in [
            ("Get-ChildItem", "stdlib"),
            ("git", "external"),
            ("Ext-Before", "external"),
            ("Ext-Mid", "external"),
            ("Ext-After", "external"),
            ("<nameless@err.ps1:#>", "func-value"),
            ("<nameless@err.ps1:#>", "unknown"),
        ] {
            listed.push(j(
                json!({"type": "unresolved", "fqn": fqn, "category": cat}),
            ));
        }
        for ext in ["Ext-Before", "Ext-Mid", "Ext-After"] {
            listed.push(edge("unresolved_call", ".err.ps1.<script>", ext));
        }
        for l in &listed {
            assert!(got.contains(l), "missing {l}\n{got:#?}");
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn recovery_inside_region_skipped() {
        // non-vacuous: the oracle puts the inside declarations and sites in the first region
        let err = recovery_err(RECOVERY_BROKEN_1, RECOVERY_BROKEN_2);
        let r = regions(&err);
        assert!(
            r.inside
                .contains(&("function_definition", "Inside-Fn".to_string(), 0)),
            "{r:?}"
        );
        assert!(
            r.inside
                .contains(&("class_definition", "InsideClass".to_string(), 0)),
            "{r:?}"
        );
        for s in [". ./inside.ps1", "Import-Module InsideMod", "Inside-Cmd"] {
            assert!(within(&span(&err, s), &r.errors[0].0), "{s:?}: {r:?}");
        }
        let (_dir, root, run) = recovery_scan("rec-inside", false, false);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        for u in recs
            .iter()
            .filter(|u| matches!(str_of(u, "type"), "struct" | "function"))
        {
            assert!(
                !["Inside-Fn", "InsideClass", "Go"].contains(&str_of(u, "name")),
                "{u}"
            );
        }
        let got = normalise_records(&recs, &root, false);
        for l in &got {
            for name in ["Inside-Fn", "InsideClass", "Inside-Cmd", "InsideMod"] {
                assert!(!l.contains(name), "{l}");
            }
        }
        let file = serde_json::json!({"type": "file", "path": "inside.ps1", "parent": ""});
        assert!(got.contains(&file.to_string()), "{got:#?}");
        for r in &recs {
            assert!(
                !(r["type"] == "uses" && str_of(r, "to") == format!("{root}/inside.ps1")),
                "{r}"
            );
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn recovery_utf16le_bom_matches_utf8() {
        let (_a, root, utf8) = recovery_scan("rec-u8", false, false);
        let (_b, wroot, wide) = recovery_scan("rec-wide", false, true);
        assert!(utf8.ok, "{:?}", utf8.stderr);
        assert!(wide.ok, "{:?}", wide.stderr);
        assert_eq!(
            by_kind(&normalise_records(&records(&wide.stdout), &wroot, true)),
            by_kind(&normalise_records(&records(&utf8.stdout), &root, true))
        );
        let strip = |run: &Run, root: &str| -> Vec<String> {
            let prefix = format!("{root}/");
            run.stderr.iter().map(|l| l.replace(&prefix, "")).collect()
        };
        let lines = strip(&utf8, &root);
        let errs = lines.iter().filter(|l| l.starts_with("ps: err.ps1:"));
        assert_eq!(errs.count(), 2, "{lines:?}");
        assert_eq!(strip(&wide, &wroot), lines);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test e2e:: -- --ignored"]
    fn recovery_whole_file_region() {
        // precondition: whole.ps1 is one ERROR root holding its earlier statements
        let w = regions(WHOLE_PS1);
        println!("whole.ps1 oracle: {w:?}");
        assert_eq!(w.root, "ERROR", "{w:?}");
        assert_eq!(w.errors.len(), 1, "{w:?}");
        assert_eq!(w.errors[0].0, 0..WHOLE_PS1.len(), "{w:?}");
        for s in [
            ". ./a_first.ps1",
            "Import-Module Dep",
            "function Keep",
            "Get-A",
        ] {
            assert!(within(&span(WHOLE_PS1, s), &w.errors[0].0), "{s:?}");
        }
        for (name, text) in WHOLE_OTHERS {
            let r = regions(text);
            assert_eq!(r.root, "source_file", "{name}: {r:?}");
            assert!(r.errors.is_empty() && r.missing.is_empty(), "{name}: {r:?}");
        }
        let scan = |tag: &str, whole: bool| {
            let dir = Scratch::new(tag);
            for (name, text) in WHOLE_OTHERS {
                dir.write(name, text.as_bytes());
            }
            if whole {
                dir.write("whole.ps1", WHOLE_PS1.as_bytes());
            }
            let root = dir.root();
            let run = psfrontend(&[&root]);
            assert!(run.ok, "{:?}", run.stderr);
            (dir, root, run)
        };
        let (_d, root, run) = scan("rec-whole", true);
        let (l, c) = w.errors[0].1;
        assert_eq!(
            run.stderr,
            [
                format!("ps: {root}/whole.ps1:{l}:{c}: syntax error"),
                "ps: 1 file had syntax errors".to_string()
            ]
        );
        let got = normalise_records(&records(&run.stdout), &root, false);
        let is_whole = |l: &String| {
            let v: serde_json::Value = serde_json::from_str(l).expect("normalised JSON");
            str_of(&v, "path") == "whole.ps1" || str_of(&v, "from").starts_with(".whole.ps1")
        };
        let mut whole: Vec<String> = got.iter().filter(|l| is_whole(l)).cloned().collect();
        whole.sort();
        let j = |v: serde_json::Value| v.to_string();
        use serde_json::json;
        let mut want = vec![
            j(json!({"type": "file", "path": "whole.ps1", "parent": ""})),
            j(json!({"type": "struct", "parent": "", "name": "whole.ps1", "path": "whole.ps1"})),
            j(
                json!({"type": "function", "parent": ".whole.ps1", "name": "<script>", "path": "whole.ps1"}),
            ),
            j(json!({"type": "contains", "from": ".whole.ps1", "to": ".whole.ps1.<script>"})),
        ];
        want.sort();
        assert_eq!(whole, want);
        // the scan continued past the whole-file region unaffected
        let (_e, rroot, rest) = scan("rec-whole-rest", false);
        assert_eq!(rest.stderr, ["ps: 0 files had syntax errors"]);
        let others: Vec<String> = got.into_iter().filter(|l| !is_whole(l)).collect();
        assert_eq!(
            others,
            normalise_records(&records(&rest.stdout), &rroot, false)
        );
        let late = json!({"type": "function", "parent": ".z_next.ps1", "name": "Late", "path": "z_next.ps1"});
        assert!(others.contains(&late.to_string()), "{others:#?}");
    }
}
