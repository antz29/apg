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

    /// Writes `bytes` to `name` inside the scratch dir and returns its path.
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("write fixture");
        path
    }
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
}
