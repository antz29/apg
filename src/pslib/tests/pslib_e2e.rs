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
        assert!(recs
            .iter()
            .all(|r| r["type"] == "module" || r["type"] == "file"));
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
        let only_b = format!(
            "{{\"type\":\"module\",\"fqn\":\"sub\"}}\n\
             {{\"type\":\"file\",\"path\":\"{b}\",\"parent\":\"sub\",\"start_line\":1,\"end_line\":2}}\n"
        );
        let side = Scratch::new("bin-tgt-side");
        let list = side.write("targets.txt", format!("{b}\n").as_bytes());
        let list = list.to_str().expect("UTF-8 path");

        let run = psfrontend(&[&root, "--targets", list]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(run.stdout, only_b);
        assert_eq!(
            run.stderr.last().map(String::as_str),
            Some("ps: 1 file had syntax errors")
        );

        let run = psfrontend(&[&root, "--module", "sub"]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(run.stdout, only_b);
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
        assert_eq!(run.stdout, plain.stdout);
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
    #[ignore = "e2e tier: real process; run via cargo test e2e:: -- --ignored"]
    fn bin_fails_without_arguments() {
        let run = psfrontend(&[]);
        assert!(!run.ok);
        let last = run.stderr.last().expect("a stderr line");
        assert!(last.starts_with("psfrontend: usage: "), "{last}");
    }
}
