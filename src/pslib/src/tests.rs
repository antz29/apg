//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules, with shared fixtures and helpers at this root.

use crate::builtins::{is_builtin, is_system_type, ACCELERATORS, ALIASES, COMMANDS};
use crate::diag::{error_regions, error_summary, write_diagnostic, Diagnostic};
use crate::identity::{
    self, base_relative, emitted_name, is_disambiguated, record_key, render, Identity,
};
use crate::manifest::{assign, read_manifest, wildcard_match, ExportSpec, ManifestKeys, ModuleMap};
use crate::parse::parse;
use crate::paths::{normalise, path_arg, resolve, FileIndex, PathArg};
use crate::resolve::{
    command_target, file_deps, import_arg, module_category, nameless_key, resolve_site,
    select_member, split_qualified, type_name, type_target, CommandRes, Dep, DepKind, DepSite,
    DotSourceMap, ImportArg, ManifestStems, SiteKind, StemMatch, Target, TypeIndex, TypeRes,
    INBOX_MODULES,
};
use crate::scan::{is_ps_source, parse_args, Emitter, Rec};
use crate::source::{decode, decode_mapped, line_count, position, span, Span};
use crate::units::{
    assign_ordinals, bare_name, param_type, property_type_name, scan_units, FileUnits, Unit,
    UnitKind,
};
use std::collections::BTreeSet;
use std::path::Path;
use tree_sitter::Node;

/// An owned argv from string literals.
fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// A dot-sourcing script: an anchored and a relative operand.
const DOT_SOURCING: &str = ". $PSScriptRoot\\lib.ps1\n. ./other.ps1\n";

/// A `using module` statement.
const USING_MODULE: &str = "using module ./Foo.psm1\n";

/// A DSC configuration block with a node and a resource.
const DSC_CONFIG: &str = "configuration Web {\n    Node 'localhost' {\n        File Site {\n            DestinationPath = 'C:\\site'\n        }\n    }\n}\n";

/// A `.psd1` manifest hashtable.
const MANIFEST: &str = "@{\n    RootModule = 'Foo.psm1'\n    NestedModules = @('lib\\Helpers.ps1')\n    FunctionsToExport = @('Get-Foo')\n}\n";

/// Every ERROR (with its text) and MISSING (with its kind) node of `src`'s
/// parse, in tree order.
fn errors(src: &str) -> Vec<String> {
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
    let tree = parse(src);
    let mut out = Vec::new();
    walk(tree.root_node(), src, &mut out);
    out
}

/// A compact rendering of `src`'s whole parse tree: named nodes by kind,
/// anonymous tokens quoted (bare brackets and `;` skipped), each field-bound
/// child prefixed `field:`, named leaves followed by their text, a lone
/// unfielded child chained with `>` and several children in `( )`.
fn dump(src: &str) -> String {
    fn render(node: Node, field: Option<&str>, src: &str, out: &mut String) {
        if let Some(f) = field {
            out.push_str(f);
            out.push(':');
        }
        if node.is_missing() {
            out.push_str("MISSING ");
        }
        if node.is_named() {
            out.push_str(node.kind());
        } else {
            out.push_str(&format!("'{}'", node.kind()));
        }
        let mut kids = Vec::new();
        let mut cur = node.walk();
        if cur.goto_first_child() {
            loop {
                let (f, kid) = (cur.field_name(), cur.node());
                let bracket = kid.kind().chars().all(|c| "(){}[];".contains(c));
                if kid.is_named() || f.is_some() || kid.is_missing() || !bracket {
                    kids.push((f, kid));
                }
                if !cur.goto_next_sibling() {
                    break;
                }
            }
        }
        match kids.as_slice() {
            [] if node.is_named() => out.push_str(&format!("{:?}", &src[node.byte_range()])),
            [] => {}
            [(None, only)] => {
                out.push('>');
                render(*only, None, src, out);
            }
            _ => {
                out.push('(');
                for (i, (f, kid)) in kids.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    render(*kid, *f, src, out);
                }
                out.push(')');
            }
        }
    }
    let tree = parse(src);
    let mut out = String::new();
    render(tree.root_node(), None, src, &mut out);
    out
}

/// Asserts each `(source, expected dump)` pair, reporting every mismatch.
fn check(cases: &[(&str, &str)]) {
    let bad: Vec<String> = cases
        .iter()
        .filter_map(|(src, want)| {
            let got = dump(src);
            (got != *want).then(|| format!("{src:?}\n  got: {got:?}"))
        })
        .collect();
    assert!(bad.is_empty(), "grammar shape drift:\n{}", bad.join("\n"));
}

/// Asserts `src` parses to a `source_file` root with a direct-child pipeline
/// whose text is `cmd`, holding no ERROR or MISSING node.
fn clean_top_pipeline(src: &str, cmd: &str) {
    let tree = parse(src);
    let root = tree.root_node();
    assert_eq!(root.kind(), "source_file", "{src:?}");
    let mut cur = root.walk();
    let hit = root
        .children(&mut cur)
        .find(|n| n.kind() == "pipeline" && &src[n.byte_range()] == cmd);
    assert!(
        hit.is_some_and(|p| !p.has_error()),
        "{cmd:?} swallowed in {src:?}"
    );
}

/// Each `[[package]]` entry's `(name, version)` in a Cargo.lock text.
fn lock_packages(lock: &str) -> Vec<(&str, &str)> {
    fn field<'a>(entry: &'a str, key: &str) -> &'a str {
        entry
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(" = "))
            .map_or("", |v| v.trim_matches('"'))
    }
    lock.split("[[package]]")
        .skip(1)
        .map(|e| (field(e, "name"), field(e, "version")))
        .collect()
}

/// `s` as UTF-16 bytes with a BOM, big-endian when `be`.
fn utf16_bytes(s: &str, be: bool) -> Vec<u8> {
    let bom = if be { [0xFE, 0xFF] } else { [0xFF, 0xFE] };
    let units = s
        .encode_utf16()
        .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() });
    bom.into_iter().chain(units).collect()
}

/// `s` as UTF-8 bytes behind a UTF-8 BOM.
fn utf8_bom(s: &str) -> Vec<u8> {
    [&[0xEF, 0xBB, 0xBF][..], s.as_bytes()].concat()
}

/// A file ending inside nested unclosed blocks: parses to a whole-file ERROR root.
const NESTED_OPEN_AT_EOF: &str = "Get-A\nfunction F { Get-B\n$x = (1 + \n";

/// Asserts `src` parses to an ERROR root (requirements.requirement.ps-error-recovery).
fn assert_error_root(src: &str) {
    assert_eq!(parse(src).root_node().kind(), "ERROR", "{src:?}");
}

/// Asserts `src` parses to a `source_file` root (requirements.requirement.ps-error-recovery).
fn assert_source_root(src: &str) {
    assert_eq!(parse(src).root_node().kind(), "source_file", "{src:?}");
}

/// [`MANIFEST`] followed by a broken statement outside its hashtable.
const MANIFEST_BROKEN_ELSEWHERE: &str = "@{\n    RootModule = 'Foo.psm1'\n    NestedModules = @('lib\\Helpers.ps1')\n    FunctionsToExport = @('Get-Foo')\n}\n$x = )\n";

/// A manifest whose NestedModules entry lies inside a local ERROR node.
const MANIFEST_BROKEN_ENTRY: &str = "@{\n    NestedModules ) = 'a.ps1'\n    RootModule = 'Foo.psm1'\n    FunctionsToExport = @('Get-Foo')\n}\n";

/// A manifest-shaped `.psd1` ending inside nested unclosed constructs: a whole-file ERROR root.
const MANIFEST_OPEN_AT_EOF: &str = "@{ RootModule = 'Foo.psm1'\nNestedModules = @(\n";

/// `read_manifest` over `src`'s parse.
fn keys_of(src: &str) -> Option<ManifestKeys> {
    read_manifest(src, &parse(src))
}

fn mk(root: Option<PathArg>, nested: Vec<PathArg>, exports: ExportSpec) -> Option<ManifestKeys> {
    Some(ManifestKeys {
        root_module: root,
        nested_modules: nested,
        exports,
    })
}

fn pats(ps: &[&str]) -> ExportSpec {
    ExportSpec::Patterns(ps.iter().map(|p| s(p)).collect())
}

/// The `mods/Foo` manifest: every NestedModules entry form `assign` meets.
const FOO_PSD1: &str = "@{\n    RootModule = 'Foo.psm1'\n    NestedModules = @(\n        '.\\lib\\Helpers.ps1', 'sub/../Other.ps1', 'Bare', 'x.dll', 'Other.psd1',\n        'missing.ps1', '..\\..\\..\\up.ps1', $x, 'LIB\\CASE.PS1', '..\\Shared\\s.ps1',\n        '..\\both.ps1', \"$PSScriptRoot\\anchored.ps1\"\n    )\n    FunctionsToExport = 'Get-Foo'\n}\n";

/// Every file of the in-memory repo, assigned with its parsed manifests plus a
/// hand-built `mods/Bar/Bar.psd1` (listed last, sorting first) naming `../both.ps1`.
fn module_fixture() -> ModuleMap {
    let files = [
        "run.ps1",
        "mods/both.ps1",
        "mods/Bar/Bar.psd1",
        "mods/Shared/s.ps1",
        "mods/Foo/Foo.psd1",
        "mods/Foo/Foo.psm1",
        "mods/Foo/lib/Helpers.ps1",
        "mods/Foo/lib/case.ps1",
        "mods/Foo/Other.ps1",
        "mods/Foo/Other.psd1",
        "mods/Foo/Broken.psd1",
        "mods/Foo/x.dll",
        "mods/Foo/anchored.ps1",
        "mods/Foo/loose.ps1",
    ]
    .map(s);
    // the repo's .psd1 sources: a manifest, a data file and a whole-file ERROR
    let psd1 = [
        ("mods/Foo/Foo.psd1", FOO_PSD1),
        ("mods/Foo/Other.psd1", "@{ Setting = 1 }\n"),
        ("mods/Foo/Broken.psd1", MANIFEST_OPEN_AT_EOF),
    ];
    let mut manifests: Vec<(String, ManifestKeys)> = psd1
        .into_iter()
        .filter_map(|(p, src)| Some((s(p), keys_of(src)?)))
        .collect();
    let bar = mk(None, vec![st("..\\both.ps1", false)], ExportSpec::All).unwrap();
    manifests.push((s("mods/Bar/Bar.psd1"), bar));
    assign(&files, &manifests)
}

/// The scan-root-relative files of the module-record fixture, in scan order.
const MODULE_FILES: [&str; 6] = [
    "mods/Foo/Foo.psd1",
    "mods/Foo/Foo.psm1",
    "mods/Foo/lib/Helpers.ps1",
    "mods/Foo/loose.ps1",
    "mods/Foo/Data.psd1",
    "root.ps1",
];

/// A module-record fixture file's source: the [`MANIFEST`], a data `.psd1` or a script.
fn module_source(path: &str) -> &'static str {
    match path {
        "mods/Foo/Foo.psd1" => MANIFEST,
        "mods/Foo/Data.psd1" => "@{ Setting = 1 }\n",
        _ => "Get-A\n",
    }
}

/// `files` assigned with the parsed keys of each `(path, source)` manifest candidate.
fn module_map(files: &[&str], psd1: &[(&str, &str)]) -> ModuleMap {
    let manifests: Vec<(String, ManifestKeys)> = psd1
        .iter()
        .filter_map(|(p, src)| Some((s(p), keys_of(src)?)))
        .collect();
    let files: Vec<String> = files.iter().map(|p| s(p)).collect();
    assign(&files, &manifests)
}

/// An Emitter over `/r` (with `map` set, when given) that scanned `files` under `/r`.
fn module_emitter(map: Option<ModuleMap>, files: &[&str]) -> Emitter {
    let mut e = Emitter::new(Path::new("/r"), "n");
    if let Some(map) = map {
        e.set_modules(map);
    }
    for p in files {
        e.scan_file(&format!("/r/{p}"), module_source(p).as_bytes());
    }
    e
}

/// The File record of fixture file `path` under `/r`, parented to `parent`.
fn module_file(path: &str, parent: &str) -> Rec {
    file(
        &format!("/r/{path}"),
        parent,
        line_count(module_source(path)),
    )
}

/// `src`'s error regions as `(line, col, message)`.
fn regions(src: &str) -> Vec<(usize, usize, String)> {
    error_regions("f.ps1", src, &parse(src))
        .into_iter()
        .map(|d| (d.line, d.col, d.message))
        .collect()
}

/// The raw bytes `[start, end)` decoded strictly: UTF-8 when `be` is `None`,
/// else UTF-16 in that byte order.
fn raw_text(bytes: &[u8], be: Option<bool>, start: usize, end: usize) -> String {
    let raw = &bytes[start..end];
    let Some(be) = be else {
        return std::str::from_utf8(raw).expect("valid UTF-8").to_owned();
    };
    let (units, odd) = raw.as_chunks::<2>();
    assert!(odd.is_empty(), "odd UTF-16 slice {raw:?}");
    let order = if be {
        u16::from_be_bytes
    } else {
        u16::from_le_bytes
    };
    char::decode_utf16(units.iter().map(|&u| order(u)))
        .collect::<Result<String, _>>()
        .expect("valid UTF-16")
}

fn s(x: &str) -> String {
    x.to_string()
}

fn file(path: &str, parent: &str, end_line: usize) -> Rec {
    Rec::File {
        path: s(path),
        parent: s(parent),
        start_line: 1,
        end_line,
    }
}

/// `node` and every node beneath it, in tree order.
fn descendants(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = vec![node];
    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        out.extend(descendants(child));
    }
    out
}

/// The `command` field of the first `invocation_operator_command` with
/// operator `op`, anywhere under `root`.
fn operand<'t>(root: Node<'t>, op: &str) -> Option<Node<'t>> {
    descendants(root)
        .into_iter()
        .find(|n| {
            n.kind() == "invocation_operator_command"
                && n.child_by_field_name("operator")
                    .is_some_and(|o| o.kind() == op)
        })?
        .child_by_field_name("command")
}

/// The first `command_argument` of the first `command` named `name`
/// (case-insensitively), with `src` its text source.
fn first_arg<'t>(root: Node<'t>, src: &str, name: &str) -> Option<Node<'t>> {
    let cmd = descendants(root).into_iter().find(|n| {
        n.kind() == "command"
            && n.child_by_field_name("name").is_some_and(|c| {
                c.kind() == "command_name" && src[c.byte_range()].eq_ignore_ascii_case(name)
            })
    })?;
    let mut cur = cmd.walk();
    let arg = cmd
        .named_children(&mut cur)
        .find(|n| n.kind() == "command_argument");
    arg
}

/// The value of hash entry `key` in the first direct child of the
/// `source_file` root that is (or wraps exactly) a `hashtable_expression`.
fn manifest_value<'t>(root: Node<'t>, src: &str, key: &str) -> Option<Node<'t>> {
    let mut cur = root.walk();
    let table = root.named_children(&mut cur).find_map(|mut n| {
        while matches!(n.kind(), "pipeline" | "command_expression") && n.named_child_count() == 1 {
            n = n.named_child(0)?;
        }
        (n.kind() == "hashtable_expression").then_some(n)
    })?;
    let mut cur = table.walk();
    let entry = table.named_children(&mut cur).find(|e| {
        e.kind() == "hash_entry"
            && e.child_by_field_name("key")
                .is_some_and(|k| &src[k.byte_range()] == key)
    })?;
    entry.child_by_field_name("value")
}

/// `path_arg` of the node `locate` finds in `src`, which must parse with no
/// ERROR or MISSING node.
fn arg_of(src: &str, locate: impl Fn(Node<'_>) -> Option<Node<'_>>) -> PathArg {
    assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
    let tree = parse(src);
    let node = locate(tree.root_node()).unwrap_or_else(|| panic!("no site in {src:?}"));
    path_arg(node, src)
}

fn st(text: &str, anchored: bool) -> PathArg {
    PathArg::Static {
        text: s(text),
        anchored,
    }
}

/// Operand spellings (after the operator) and their classification.
fn operand_cells() -> Vec<(&'static str, PathArg)> {
    vec![
        ("./lib.ps1", st("./lib.ps1", false)),
        ("'lib.ps1'", st("lib.ps1", false)),
        ("\"lib.ps1\"", st("lib.ps1", false)),
        (r"$PSScriptRoot\lib.ps1", st("lib.ps1", true)),
        ("\"$PSScriptRoot/lib.ps1\"", st("lib.ps1", true)),
        ("\"$psscriptroot\\lib.ps1\"", st("lib.ps1", true)),
        (
            r"'$PSScriptRoot\lib.ps1'",
            st(r"$PSScriptRoot\lib.ps1", false),
        ),
        ("$path", PathArg::Computed),
        ("\"$env:X\\lib.ps1\"", PathArg::Computed),
        ("\"$PSScriptRoot$x.ps1\"", PathArg::Computed),
        ("(Join-Path $PSScriptRoot lib.ps1)", PathArg::Computed),
        ("./l`ib.ps1", PathArg::Computed),
        (r"$PSScriptRoot\l`ib.ps1", PathArg::Computed),
        ("\"l`ib.ps1\"", PathArg::Computed),
        ("'`lib.ps1'", st("`lib.ps1", false)),
        (r"${PSScriptRoot}\x.ps1", st("x.ps1", true)),
        ("\"${PSScriptRoot}/x.ps1\"", st("x.ps1", true)),
        (r"${psscriptroot}\x.ps1", st("x.ps1", true)),
        (r"$PSSCRIPTROOT\x.ps1", st("x.ps1", true)),
        ("$psscriptroot/x.ps1", st("x.ps1", true)),
    ]
}

/// A hand-built unit spanning raw bytes `start..end` on line 1, ordinal unset.
fn unit_at(kind: UnitKind, name: &str, start: usize, end: usize) -> Unit {
    Unit {
        kind,
        name: s(name),
        params: Vec::new(),
        overloaded: false,
        owner: None,
        span: Span {
            start,
            end,
            start_line: 1,
            end_line: 1,
        },
        ordinal: 0,
        property_types: Vec::new(),
        bases: vec![],
    }
}

/// A hand-built member of class unit `owner` with parameter types `params`.
fn member(kind: UnitKind, name: &str, owner: usize, params: &[&str]) -> Unit {
    Unit {
        owner: Some(owner),
        params: argv(params),
        ..unit_at(kind, name, 0, 0)
    }
}

/// A hand-built file: `<script>`, class `C` (unit 1) with methods M() (2),
/// M(int) (3), M(int,string) (4), N(object) (5) and constructors new() (6),
/// new(int) (7).
fn class_file() -> FileUnits {
    use UnitKind::*;
    FileUnits {
        path: s("c.ps1"),
        file_span: unit_at(ScriptBody, "", 0, 0).span,
        units: vec![
            unit_at(ScriptBody, "<script>", 0, 0),
            unit_at(Class, "C", 0, 0),
            member(Method, "M", 1, &[]),
            member(Method, "M", 1, &["int"]),
            member(Method, "M", 1, &["int", "string"]),
            member(Method, "N", 1, &["object"]),
            member(Constructor, "C", 1, &[]),
            member(Constructor, "C", 1, &["int"]),
        ],
    }
}

/// A top-level statement, function/filter/workflow units and a nested function.
const SCRIPT_UNITS: &str =
    "Get-A\nfunction Foo {\n    function Nested { }\n}\nfilter Bar { }\nworkflow Baz { }\n";

/// A class with typed properties, an overload group, a repeated signature, a
/// constructor and a lone method, then an enum and a case-repeated class.
const CLASS_UNITS: &str = "class C {\n    [D]$P\n    [D[]]$Q\n    [string]$S\n    M() { }\n    M([int]$x) { }\n    M($y) { }\n    m([Int]$z) { }\n    C([int]$a) { }\n    N([string]$s) { }\n}\nenum E { A; B }\nclass c { }\n";

/// The units of `bytes` decoded, parsed and scanned as `f.ps1`.
fn units_of(bytes: &[u8]) -> FileUnits {
    let src = decode_mapped(bytes);
    scan_units("f.ps1", &src, &parse(&src.text))
}

/// Each unit's `(kind, name, ordinal)`, in unit order.
fn unit_rows(fu: &FileUnits) -> Vec<(UnitKind, &str, usize)> {
    fu.units
        .iter()
        .map(|u| (u.kind, u.name.as_str(), u.ordinal))
        .collect()
}

/// The units of `src` parsed and scanned, keyed as the repo-relative `path`.
fn units_at(path: &str, src: &str) -> FileUnits {
    FileUnits {
        path: s(path),
        ..units_of(src.as_bytes())
    }
}

/// A hand-built file `path`: `<script>`, then one unit per `(kind, name)`.
fn type_file(path: &str, types: &[(UnitKind, &str)]) -> FileUnits {
    let mut units = vec![unit_at(UnitKind::ScriptBody, "<script>", 0, 0)];
    units.extend(types.iter().map(|&(k, n)| unit_at(k, n, 0, 0)));
    FileUnits {
        path: s(path),
        file_span: units[0].span,
        units,
    }
}

/// A dot-source map from `(script, dot-sourced files)` pairs.
fn dots(pairs: &[(&str, &[&str])]) -> DotSourceMap {
    pairs.iter().map(|(p, ds)| (s(p), argv(ds))).collect()
}

/// `TypeRes::One` naming unit `index` of `path`.
fn one(path: &str, index: usize) -> TypeRes {
    TypeRes::One {
        path: s(path),
        index,
    }
}

/// The function/class/enum/method declaration nodes under `root` whose name
/// field reads `name`, in tree order.
fn decls<'t>(root: Node<'t>, src: &str, name: &str) -> Vec<Node<'t>> {
    descendants(root)
        .into_iter()
        .filter(|n| {
            matches!(
                n.kind(),
                "function_definition" | "class_definition" | "enum_definition" | "class_method"
            ) && n
                .child_by_field_name("name")
                .is_some_and(|f| &src[f.byte_range()] == name)
        })
        .collect()
}

/// Each declaration named `name` in `src`'s parse, in tree order, as (it has
/// an ERROR ancestor, its body holds an ERROR node).
fn decl_errors(src: &str, name: &str) -> Vec<(bool, bool)> {
    let tree = parse(src);
    decls(tree.root_node(), src, name)
        .into_iter()
        .map(|n| {
            let mut up = n.parent();
            let mut inside = false;
            while let Some(p) = up {
                inside |= p.is_error();
                up = p.parent();
            }
            let body = n
                .child_by_field_name("body")
                .is_some_and(|b| descendants(b).iter().any(|d| d.is_error()));
            (inside, body)
        })
        .collect()
}

/// An in-memory tree of `(path, text)`: its ModuleMap (manifests = its parsed
/// `.psd1` files), every file's units and the claimed paths.
fn identity_fixture(tree: &[(&str, &str)]) -> (ModuleMap, Vec<FileUnits>, BTreeSet<String>) {
    let files: Vec<&str> = tree.iter().map(|(p, _)| *p).collect();
    let psd1: Vec<(&str, &str)> = tree
        .iter()
        .filter(|(p, _)| p.ends_with(".psd1"))
        .copied()
        .collect();
    let units = tree
        .iter()
        .map(|(p, text)| {
            let src = decode_mapped(text.as_bytes());
            scan_units(p, &src, &parse(&src.text))
        })
        .collect();
    let paths = files.iter().map(|p| s(p)).collect();
    (module_map(&files, &psd1), units, paths)
}

/// Asserts `id`'s whole-tree invariants: distinct keys, distinct Function
/// (parent, name) pairs, every parent a key of the right owner, and keys() =
/// the modules plus record_key(parent, name) of every script and unit.
fn assert_identity(map: &ModuleMap, files: &[FileUnits], id: &Identity) {
    let mut keys = id.keys();
    let distinct: std::collections::HashSet<&String> = keys.iter().collect();
    assert_eq!(distinct.len(), keys.len(), "duplicate key in {keys:?}");
    let mut rebuilt = map.modules();
    let mut functions = std::collections::HashSet::new();
    for f in files {
        let p = f.path.as_str();
        let ext = p.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        let script = matches!(ext.as_deref(), Some("ps1" | "psm1"));
        let Some(name) = id.script_name(p) else {
            assert!(!script, "{p} has no script name");
            continue;
        };
        assert!(script, "{p} named {name}");
        let parent = id.script_parent(p);
        assert_eq!(parent, map.module(p), "{p}");
        assert!(distinct.contains(&parent), "{p} parent {parent}");
        let script_key = record_key(&parent, name);
        for (i, u) in f.units.iter().enumerate() {
            let parent = id.unit_parent(p, i);
            let owner = u.owner.map_or_else(
                || script_key.clone(),
                |o| record_key(&id.unit_parent(p, o), id.unit_name(p, o)),
            );
            assert_eq!(parent, owner, "{p} unit {i}");
            assert!(distinct.contains(&parent), "{p} unit {i} parent {parent}");
            let name = id.unit_name(p, i);
            if !matches!(u.kind, UnitKind::Class | UnitKind::Enum) {
                assert!(
                    functions.insert((parent.clone(), s(name))),
                    "{parent} {name}"
                );
            }
            rebuilt.push(record_key(&parent, name));
        }
        rebuilt.push(script_key);
    }
    keys.sort();
    rebuilt.sort();
    assert_eq!(rebuilt, keys);
}

/// `tree` resolved, its invariants asserted.
fn resolved(tree: &[(&str, &str)]) -> (ModuleMap, Vec<FileUnits>, Identity) {
    let (map, files, paths) = identity_fixture(tree);
    let id = identity::resolve(&map, &files, &paths);
    assert_identity(&map, &files, &id);
    (map, files, id)
}

/// Every unit name of `path`, in unit order.
fn unit_names<'i>(id: &'i Identity, files: &[FileUnits], path: &str) -> Vec<&'i str> {
    let n = files
        .iter()
        .find(|f| f.path == path)
        .expect(path)
        .units
        .len();
    (0..n).map(|i| id.unit_name(path, i)).collect()
}

/// `tree`'s ModuleMap: its paths assigned with the parsed keys of its `.psd1` sources.
fn tree_map<B: AsRef<[u8]>>(tree: &[(&str, B)]) -> ModuleMap {
    let files: Vec<&str> = tree.iter().map(|(p, _)| *p).collect();
    let psd1: Vec<(&str, String)> = tree
        .iter()
        .filter(|(p, _)| p.ends_with(".psd1"))
        .map(|(p, b)| (*p, decode_mapped(b.as_ref()).text))
        .collect();
    let psd1: Vec<(&str, &str)> = psd1.iter().map(|(p, t)| (*p, t.as_str())).collect();
    module_map(&files, &psd1)
}

/// An Emitter over `/r` that, with `tree`'s ModuleMap set, scanned each `(path, bytes)`
/// as `/r/<path>` in order, then finished.
fn emit_tree<B: AsRef<[u8]>>(tree: &[(&str, B)]) -> Emitter {
    let mut e = Emitter::new(Path::new("/r"), "n");
    e.set_modules(tree_map(tree));
    for (p, b) in tree {
        e.scan_file(&format!("/r/{p}"), b.as_ref());
    }
    e.finish();
    e
}

/// `identity::resolve` rebuilt from `tree`'s in-memory inputs: its ModuleMap, the
/// units of each decoded file and its path set.
fn tree_identity<B: AsRef<[u8]>>(tree: &[(&str, B)]) -> Identity {
    let units: Vec<FileUnits> = tree
        .iter()
        .map(|(p, b)| {
            let src = decode_mapped(b.as_ref());
            scan_units(p, &src, &parse(&src.text))
        })
        .collect();
    let paths = tree.iter().map(|(p, _)| s(p)).collect();
    identity::resolve(&tree_map(tree), &units, &paths)
}

/// Phase 7's `ps` rendering re-implemented over `recs`: each Module/Struct/Function
/// record's (key, `ps.` + key), Functions grouped by exact (parent, name) as the
/// ingestor's render_function_fqns does.
fn ps_rendered(recs: &[Rec]) -> Vec<(String, String)> {
    let mut groups: std::collections::HashMap<(&str, &str), usize> = Default::default();
    for r in recs {
        if let Rec::Function { parent, name, .. } = r {
            *groups.entry((parent, name)).or_default() += 1;
        }
    }
    recs.iter()
        .filter_map(|r| {
            let key = match r {
                Rec::Module { fqn } => fqn.clone(),
                Rec::Struct { parent, name, .. } => format!("{parent}.{name}"),
                Rec::Function {
                    parent,
                    name,
                    params,
                    ..
                } if groups[&(parent.as_str(), name.as_str())] > 1 => {
                    format!("{parent}.{name}({})", params.join(","))
                }
                Rec::Function { parent, name, .. } => format!("{parent}.{name}"),
                _ => return None,
            };
            Some((key.clone(), format!("ps.{key}")))
        })
        .collect()
}

/// `tree` emitted, asserting its rendered keys are distinct and equal the rebuilt
/// identity's keys, no Function (parent, name) group has two members, and every
/// Struct/Function parent is a key.
fn assert_emitted<B: AsRef<[u8]>>(tree: &[(&str, B)]) -> Emitter {
    let e = emit_tree(tree);
    let mut keys: Vec<String> = ps_rendered(e.records())
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    keys.sort();
    let set: BTreeSet<&String> = keys.iter().collect();
    assert_eq!(set.len(), keys.len(), "duplicate key in {keys:?}");
    let mut want = tree_identity(tree).keys();
    want.sort();
    assert_eq!(keys, want);
    let mut functions = std::collections::HashSet::new();
    for r in e.records() {
        let parent = match r {
            Rec::Function { parent, name, .. } => {
                assert!(functions.insert((parent, name)), "{parent} {name}");
                parent
            }
            Rec::Struct { parent, .. } => parent,
            _ => continue,
        };
        assert!(set.contains(parent), "parent {parent} is no key");
    }
    e
}

/// The `ps.`-rendered FQN of the record keyed `key` in `recs`.
fn rendered_of(recs: &[Rec], key: &str) -> String {
    ps_rendered(recs)
        .into_iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no record keyed {key}"))
        .1
}

/// The resolution fixture: the scanned paths (`tools/x.dll` absent) assigned with
/// hand-built manifests — Foo plus two `Bar` manifests sharing a stem — and indexed.
fn resolve_fixture() -> (ModuleMap, FileIndex, ManifestStems) {
    let files = [
        "mods/Foo/Foo.psd1",
        "mods/Foo/Foo.psm1",
        "mods/Foo/lib/Helpers.ps1",
        "mods/Foo/Data.psd1",
        "mods/Bar/Bar.psd1",
        "other/Bar/Bar.PSD1",
        "scripts/a.ps1",
        "scripts/B.ps1",
        "scripts/b.ps1",
    ]
    .map(s);
    let foo = mk(
        Some(st("Foo.psm1", false)),
        vec![st("lib\\Helpers.ps1", false)],
        ExportSpec::All,
    );
    let bar = || mk(None, vec![], ExportSpec::All).unwrap();
    let manifests = [
        (s("mods/Foo/Foo.psd1"), foo.unwrap()),
        (s("mods/Bar/Bar.psd1"), bar()),
        (s("other/Bar/Bar.PSD1"), bar()),
    ];
    let map = assign(&files, &manifests);
    let stems = ManifestStems::new(&map);
    (map, FileIndex::new(files), stems)
}

/// A script with a top-level statement, class C (a typed property, a constructor, an
/// overload pair and a repeated signature), enum E and function F.
const EMIT_SCRIPT: &str = "Get-A\nclass C {\n    [E]$p\n    C() { }\n    M() { }\n    M([int]$x) { }\n    M([Int]$y) { }\n}\nenum E { A; B }\nfunction F { }\n";

/// The whole-tree uniqueness fixture: every collision identity resolves.
const UNIQUE_TREE: [(&str, &str); 15] = [
    ("x.a.ps1", "Get-A\n"),
    (".x/a.ps1", "Get-A\n"),
    ("a.b/c.ps1", "Get-A\n"),
    ("a/b.c.ps1", "Get-A\n"),
    ("a.b.ps1/f.ps1", "Get-A\n"),
    ("a/b.ps1", "Get-A\n"),
    ("m/a.ps1", "class B { }\n"),
    ("m.a.ps1.B/f.ps1", "Get-A\n"),
    ("m/s.ps1", "function C.M { }\nclass C {\n    M() { }\n}\n"),
    ("s.ps1", "class C {\n    M() { }\n    M([int]$x) { }\n}\n"),
    (".s.ps1.C.M(int)/f.ps1", "Get-A\n"),
    ("build.ps1", "function F {}\n"),
    ("build/x.ps1", "function F {}\n"),
    ("q/one.ps1", "function G { }\n"),
    ("q/two.ps1", "function G { }\n"),
];

/// `file_deps` over `bytes` decoded (source::decode_mapped) and parsed.
fn deps_of(bytes: &[u8]) -> Vec<DepSite> {
    let src = decode_mapped(bytes);
    file_deps(&src, &parse(&src.text))
}

/// Every dot-source invocation and `Import-Module` command node of `src`'s parse,
/// in tree order, as (its text, it has an ERROR ancestor).
fn site_ancestry(src: &str) -> Vec<(&str, bool)> {
    let tree = parse(src);
    descendants(tree.root_node())
        .into_iter()
        .filter(|n| match n.kind() {
            "invocation_operator_command" => n
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "."),
            "command" => n
                .child_by_field_name("name")
                .is_some_and(|c| src[c.byte_range()].eq_ignore_ascii_case("Import-Module")),
            _ => false,
        })
        .map(|n| {
            let mut up = n.parent();
            let mut inside = false;
            while let Some(p) = up {
                inside |= p.is_error();
                up = p.parent();
            }
            (&src[n.byte_range()], inside)
        })
        .collect()
}

/// The file-dependency fixture's script: every dot-source and Import-Module site form, in
/// source order, the last inside a function body.
const DEP_MAIN: &str = ". $PSScriptRoot\\lib\\util.ps1\n. ./lib/util.ps1\n. $x\n. { 1 }\nImport-Module ./mods/Foo/Foo.psm1\nImport-Module Foo\nImport-Module Bar\nImport-Module Microsoft.PowerShell.Utility\nImport-Module PSReadLine\nImport-Module PSReadLine\nImport-Module ./lib/util.ps1\nImport-Module $m\nfunction F { . ./lib/util.ps1 }\n";

/// The file-dependency fixture tree with `main` as main.ps1's bytes: Foo's manifest and root
/// module, two stem-colliding Bar manifests, a dot-sourced script and a data file, the two
/// `.psd1` sources each carrying sites of their own.
fn dep_tree(main: Vec<u8>) -> Vec<(&'static str, Vec<u8>)> {
    [
        (
            "mods/Foo/Foo.psd1",
            "@{ RootModule = 'Foo.psm1' }\n. ../../lib/util.ps1\nImport-Module ManifestOnly\n",
        ),
        ("mods/Foo/Foo.psm1", "function Get-Foo { }\n"),
        ("mods/Bar/Bar.psd1", "@{ FunctionsToExport = '*' }\n"),
        ("other/Bar/Bar.psd1", "@{ FunctionsToExport = '*' }\n"),
        ("lib/util.ps1", "function Get-Util { }\n"),
        (
            "data/cfg.psd1",
            "@{ Setting = 1 }\n. ../lib/util.ps1\nImport-Module CfgOnly\n",
        ),
    ]
    .map(|(p, src)| (p, src.as_bytes().to_vec()))
    .into_iter()
    .chain([("main.ps1", main)])
    .collect()
}

/// Three scripts naming Foo.psm1 by dot-source and Import-Module in every order and count.
const PRECEDENCE_TREE: [(&str, &str); 5] = [
    ("mods/Foo/Foo.psd1", "@{ RootModule = 'Foo.psm1' }\n"),
    ("mods/Foo/Foo.psm1", "function Get-Foo { }\n"),
    (
        "dot_then_import.ps1",
        ". ./mods/Foo/Foo.psm1\nImport-Module ./mods/Foo/Foo.psm1\n",
    ),
    (
        "import_then_dot.ps1",
        "Import-Module ./mods/Foo/Foo.psm1\n. ./mods/Foo/Foo.psm1\n",
    ),
    (
        "two_imports.ps1",
        "Import-Module ./mods/Foo/Foo.psm1\n. ./mods/Foo/Foo.psm1\nImport-Module ./mods/Foo/Foo.psm1\n",
    ),
];

/// `recs`' file-dependency records (uses, unresolved uses, unresolved targets), in order.
fn dep_recs(recs: &[Rec]) -> Vec<Rec> {
    recs.iter()
        .filter(|r| {
            matches!(
                r,
                Rec::Uses { .. } | Rec::UnresolvedUse { .. } | Rec::Unresolved { .. }
            )
        })
        .cloned()
        .collect()
}

/// A uses record from `from` to the File path `to`, of dependency `kind`.
fn uses(from: &str, to: &str, kind: &str) -> Rec {
    Rec::Uses {
        from: s(from),
        to: s(to),
        properties: [(s("kind"), s(kind))].into(),
    }
}

/// The id of the script Struct (its file's first Struct) emitted for `path` in `recs`.
fn script_id(recs: &[Rec], path: &str) -> String {
    recs.iter()
        .find_map(|r| match r {
            Rec::Struct { id, path: p, .. } if p == path => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no script Struct for {path}"))
}

mod unit {
    use super::*;

    #[test]
    fn shared_fixtures_parse_clean() {
        for src in [DOT_SOURCING, USING_MODULE, DSC_CONFIG, MANIFEST] {
            assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
        }
        check(&[
            (
                DOT_SOURCING,
                "source_file(pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\")))) pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>bare_word\"./other.ps1\"))))",
            ),
            (
                USING_MODULE,
                "source_file>using_statement('using' kind:'module' value:command_argument>bare_word\"./Foo.psm1\")",
            ),
            (
                DSC_CONFIG,
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"Web\" body:script_block>node_statement(keyword:'node' name:single_quoted_string\"'localhost'\" body:script_block>resource_statement(type:type_identifier\"File\" name:bare_word\"Site\" body:resource_body(properties:hash_entry(key:identifier\"DestinationPath\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'C:\\\\site'\")))))))",
            ),
            (
                MANIFEST,
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"RootModule\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Foo.psm1'\"))) hash_entry(key:identifier\"NestedModules\" '=' value:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:single_quoted_string\"'lib\\\\Helpers.ps1'\")))))) hash_entry(key:identifier\"FunctionsToExport\" '=' value:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:single_quoted_string\"'Get-Foo'\")))))))))",
            ),
        ]);
    }

    #[test]
    fn a_script_root() {
        check(&[(
            "param($a, [int]$b)\nGet-A\n$x = 1\n",
            "source_file(param_block('param' parameters:parameter_list(parameter(name:variable\"$a\") ',' parameter(types:type_literal>type_name(name:type_identifier\"int\") name:variable\"$b\"))) pipeline(elements:command(name:command_name>bare_word\"Get-A\")) assignment_statement(left:variable\"$x\" operator:'=' right:pipeline(elements:command_expression(expression:integer_literal\"1\"))))",
        )]);
    }

    #[test]
    fn b_function_filter_workflow() {
        check(&[
            (
                "function F($a) { Get-A }",
                "source_file>function_definition(keyword:'function' name:function_name\"F\" parameters:parameter_list>parameter(name:variable\"$a\") body:script_block>pipeline(elements:command(name:command_name>bare_word\"Get-A\")))",
            ),
            (
                "function F { param($a) Get-A }",
                "source_file>function_definition(keyword:'function' name:function_name\"F\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"Get-A\"))))",
            ),
            (
                "function F {\n  param($a)\n  Get-A\n}",
                "source_file>function_definition(keyword:'function' name:function_name\"F\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"Get-A\"))))",
            ),
            (
                "filter Fl { $_ }",
                "source_file>function_definition(keyword:'filter' name:function_name\"Fl\" body:script_block>pipeline(elements:command_expression(expression:variable\"$_\")))",
            ),
            (
                "workflow W { param($a) Get-A }",
                "source_file>function_definition(keyword:'workflow' name:function_name\"W\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"Get-A\"))))",
            ),
            (
                "workflow W {\n  param($a)\n  Get-A\n}",
                "source_file>function_definition(keyword:'workflow' name:function_name\"W\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"Get-A\"))))",
            ),
            (
                "function Outer { function Inner { } Inner }",
                "source_file>function_definition(keyword:'function' name:function_name\"Outer\" body:script_block(function_definition(keyword:'function' name:function_name\"Inner\" body:script_block\"{ }\") pipeline(elements:command(name:command_name>bare_word\"Inner\"))))",
            ),
        ]);
    }

    #[test]
    fn c_scope_prefixed_names() {
        check(&[
            (
                "function global:Foo { }",
                "source_file>function_definition(keyword:'function' name:function_name\"global:Foo\" body:script_block\"{ }\")",
            ),
            (
                "function script:Foo { }",
                "source_file>function_definition(keyword:'function' name:function_name\"script:Foo\" body:script_block\"{ }\")",
            ),
            (
                "function local:Foo { }",
                "source_file>function_definition(keyword:'function' name:function_name\"local:Foo\" body:script_block\"{ }\")",
            ),
            (
                "function private:Foo { }",
                "source_file>function_definition(keyword:'function' name:function_name\"private:Foo\" body:script_block\"{ }\")",
            ),
            (
                "script:Foo -X 1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"script:Foo\" command_parameter(name:command_parameter_name\"-X\") command_argument>integer_literal\"1\"))",
            ),
        ]);
    }

    #[test]
    fn d_class_and_enum() {
        check(&[
            (
                "class C : B, I {\n  [Foo]$P\n  $U\n  C([int]$x, $y) { }\n  [void] M([int]$x, $y) { }\n  static [int] S() { return 1 }\n}",
                "source_file>class_definition('class' name:identifier\"C\" ':' base_types:type_name(name:type_identifier\"B\") ',' base_types:type_name(name:type_identifier\"I\") body:class_body(members:class_property(types:type_literal>type_name(name:type_identifier\"Foo\") name:variable\"$P\") members:class_property(name:variable\"$U\") members:class_method(name:identifier\"C\" parameters:parameter_list(parameter(types:type_literal>type_name(name:type_identifier\"int\") name:variable\"$x\") ',' parameter(name:variable\"$y\")) body:statement_block\"{ }\") members:class_method(return_type:type_literal>type_name(name:type_identifier\"void\") name:identifier\"M\" parameters:parameter_list(parameter(types:type_literal>type_name(name:type_identifier\"int\") name:variable\"$x\") ',' parameter(name:variable\"$y\")) body:statement_block\"{ }\") members:class_method(modifiers:'static' return_type:type_literal>type_name(name:type_identifier\"int\") name:identifier\"S\" parameters:parameter_list\"()\" body:statement_block>return_statement(keyword:'return' value:pipeline(elements:command_expression(expression:integer_literal\"1\"))))))",
            ),
            (
                "enum E { A\n B = 2 }",
                "source_file>enum_definition('enum' name:identifier\"E\" body:enum_body(members:enum_member(name:identifier\"A\") members:enum_member(name:identifier\"B\" '=' value:integer_literal\"2\")))",
            ),
        ]);
    }

    #[test]
    fn e_static_and_instance_invocation() {
        check(&[
            (
                "[T]::M($a)",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"T\") operator:'::' member:identifier\"M\") arguments:argument_list>variable\"$a\")))",
            ),
            (
                "[T]::new($a)",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"T\") operator:'::' member:identifier\"new\") arguments:argument_list>variable\"$a\")))",
            ),
            (
                "$this.M($a, $b)",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:variable\"$this\" operator:'.' member:identifier\"M\") arguments:argument_list(variable\"$a\" ',' variable\"$b\"))))",
            ),
        ]);
    }

    #[test]
    fn f_call_and_dot_operands() {
        check(&[
            (
                "& { Get-A }",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>script_block>pipeline(elements:command(name:command_name>bare_word\"Get-A\")))))",
            ),
            (
                "& $sb",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>variable\"$sb\")))",
            ),
            (
                "& ('a' + 'b')",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>parenthesized_expression>pipeline(elements:command_expression(expression:binary_expression(left:single_quoted_string\"'a'\" operator:'+' right:single_quoted_string\"'b'\"))))))",
            ),
            (
                "& 'C:\\x.ps1'",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>single_quoted_string\"'C:\\\\x.ps1'\")))",
            ),
            (
                ". { Get-A }",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>script_block>pipeline(elements:command(name:command_name>bare_word\"Get-A\")))))",
            ),
            (
                ". $sb",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>variable\"$sb\")))",
            ),
            (
                ". ('a' + 'b')",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>parenthesized_expression>pipeline(elements:command_expression(expression:binary_expression(left:single_quoted_string\"'a'\" operator:'+' right:single_quoted_string\"'b'\"))))))",
            ),
            (
                ". 'lib.ps1'",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>single_quoted_string\"'lib.ps1'\")))",
            ),
        ]);
    }

    #[test]
    fn g_path_literals() {
        check(&[
            (
                ". ./lib.ps1",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>bare_word\"./lib.ps1\")))",
            ),
            (
                ". './lib.ps1'",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>single_quoted_string\"'./lib.ps1'\")))",
            ),
            (
                ". \"./lib.ps1\"",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>expandable_string('\"' string_content\"./lib.ps1\" '\"'))))",
            ),
            (
                ". $PSScriptRoot\\lib.ps1",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\"))))",
            ),
            (
                ". \"$PSScriptRoot/lib.ps1\"",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>expandable_string('\"' variable\"$PSScriptRoot\" string_content\"/lib.ps1\" '\"'))))",
            ),
            (
                "Import-Module ./lib.ps1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"./lib.ps1\"))",
            ),
            (
                "Import-Module './lib.ps1'",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>single_quoted_string\"'./lib.ps1'\"))",
            ),
            (
                "Import-Module \"./lib.ps1\"",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>expandable_string('\"' string_content\"./lib.ps1\" '\"')))",
            ),
            (
                "Import-Module $PSScriptRoot\\lib.ps1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\")))",
            ),
            (
                "Import-Module \"$PSScriptRoot/lib.ps1\"",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>expandable_string('\"' variable\"$PSScriptRoot\" string_content\"/lib.ps1\" '\"')))",
            ),
            (
                "Import-Module -Name ./lib.ps1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\") command_argument>bare_word\"./lib.ps1\"))",
            ),
            (
                "Import-Module -Name './lib.ps1'",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\") command_argument>single_quoted_string\"'./lib.ps1'\"))",
            ),
            (
                "Import-Module -Name \"./lib.ps1\"",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\") command_argument>expandable_string('\"' string_content\"./lib.ps1\" '\"')))",
            ),
            (
                "Import-Module -Name $PSScriptRoot\\lib.ps1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\") command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\")))",
            ),
            (
                "Import-Module -Name \"$PSScriptRoot/lib.ps1\"",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\") command_argument>expandable_string('\"' variable\"$PSScriptRoot\" string_content\"/lib.ps1\" '\"')))",
            ),
        ]);
    }

    #[test]
    fn h_commands_using_dsc_hashtables() {
        check(&[
            (
                "Get-Foo -Name x 'y' $z",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Get-Foo\" command_parameter(name:command_parameter_name\"-Name\") command_argument>bare_word\"x\" command_argument>single_quoted_string\"'y'\" command_argument>variable\"$z\"))",
            ),
            (
                "using module Foo",
                "source_file>using_statement('using' kind:'module' value:command_argument>bare_word\"Foo\")",
            ),
            (
                "using module 'Foo.psm1'",
                "source_file>using_statement('using' kind:'module' value:single_quoted_string\"'Foo.psm1'\")",
            ),
            (
                "configuration C { }",
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block\"{ }\")",
            ),
            (
                "@{ A = 1; 'B' = 'x'; C = @('y') }",
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"A\" '=' value:pipeline(elements:command_expression(expression:integer_literal\"1\"))) hash_entry(key:single_quoted_string\"'B'\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'x'\"))) hash_entry(key:identifier\"C\" '=' value:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:single_quoted_string\"'y'\")))))))))",
            ),
        ]);
    }

    #[test]
    fn i_error_and_missing() {
        let srcs = [
            "function F { Get-A\n$x = (1 + \n",
            "function F {",
            "(1 + 2",
            "$x = @(1",
            "if ($x) {",
            "class C {",
            "$x = ",
        ];
        let got: Vec<Vec<String>> = srcs.iter().map(|s| errors(s)).collect();
        assert_eq!(
            got,
            [
                vec!["ERROR \"function F { Get-A\\n$x = (1 + \\n\""],
                vec!["ERROR \"function F {\""],
                vec!["MISSING )"],
                vec!["MISSING )"],
                vec!["MISSING }"],
                vec!["MISSING }"],
                vec!["ERROR \"=\""],
            ]
        );
        // A file ending inside nested unclosed constructs is wholly one ERROR region.
        let dumps = [
            "ERROR('function' function_name\"F\" pipeline(elements:command(name:command_name>bare_word\"Get-A\")) variable\"$x\" '=' integer_literal\"1\" '+')",
            "source_file>ERROR('function' function_name\"F\")",
            "source_file>pipeline(elements:command_expression(expression:parenthesized_expression(pipeline(elements:command_expression(expression:binary_expression(left:integer_literal\"1\" operator:'+' right:integer_literal\"2\"))) MISSING ')')))",
            "source_file>assignment_statement(left:variable\"$x\" operator:'=' right:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:integer_literal\"1\")) MISSING ')'))))",
            "source_file>if_statement('if' condition:condition>pipeline(elements:command_expression(expression:variable\"$x\")) body:statement_block>MISSING '}')",
            "source_file>class_definition('class' name:identifier\"C\" body:class_body>MISSING '}')",
            "source_file(pipeline(elements:command_expression(expression:variable\"$x\")) ERROR>'=')",
        ];
        check(&srcs.into_iter().zip(dumps).collect::<Vec<_>>());
    }

    #[test]
    fn j_colon_bound_parameters() {
        check(&[
            (
                "Import-Module Foo -Force:$true",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"Foo\" command_parameter(name:command_parameter_name\"-Force\" ':' value:command_argument>variable\"$true\")))",
            ),
            (
                "Import-Module Foo -Force:$false",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"Foo\" command_parameter(name:command_parameter_name\"-Force\" ':' value:command_argument>variable\"$false\")))",
            ),
            (
                "Import-Module Foo -Prefix:P",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"Foo\" command_parameter(name:command_parameter_name\"-Prefix\" ':' value:command_argument>bare_word\"P\")))",
            ),
            (
                "Import-Module -Name:Foo",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\" ':' value:command_argument>bare_word\"Foo\")))",
            ),
            (
                "Import-Module -name:'./x.psm1'",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-name\" ':' value:command_argument>single_quoted_string\"'./x.psm1'\")))",
            ),
            (
                "Import-Module -Name:$m",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\" ':' value:command_argument>variable\"$m\")))",
            ),
        ]);
    }

    #[test]
    fn k_hashtable_arguments() {
        check(&[
            (
                "Import-Module -FullyQualifiedName @{ModuleName='Foo'; ModuleVersion='1.0'}",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-FullyQualifiedName\") command_argument>hashtable_expression('@{' hash_entry(key:identifier\"ModuleName\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Foo'\"))) hash_entry(key:identifier\"ModuleVersion\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'1.0'\"))))))",
            ),
            (
                "Import-Module -FullyQualifiedName:@{ModuleName='Foo'}",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-FullyQualifiedName\" ':' value:command_argument>hashtable_expression('@{' hash_entry(key:identifier\"ModuleName\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Foo'\")))))))",
            ),
        ]);
    }

    #[test]
    fn l_import_module_variants() {
        check(&[
            (
                "ipmo Foo",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"ipmo\" command_argument>bare_word\"Foo\"))",
            ),
            (
                "import-module Foo",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"import-module\" command_argument>bare_word\"Foo\"))",
            ),
            (
                "Microsoft.PowerShell.Core\\Import-Module Foo",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Microsoft.PowerShell.Core\\\\Import-Module\" command_argument>bare_word\"Foo\"))",
            ),
            (
                "Import-Module Foo -Force -Global -PassThru -Verbose -Debug -WhatIf -Confirm -Na Bar",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"Foo\" command_parameter(name:command_parameter_name\"-Force\") command_parameter(name:command_parameter_name\"-Global\") command_parameter(name:command_parameter_name\"-PassThru\") command_parameter(name:command_parameter_name\"-Verbose\") command_parameter(name:command_parameter_name\"-Debug\") command_parameter(name:command_parameter_name\"-WhatIf\") command_parameter(name:command_parameter_name\"-Confirm\") command_parameter(name:command_parameter_name\"-Na\") command_argument>bare_word\"Bar\"))",
            ),
            (
                "Import-Module Foo -Prefix P",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"Foo\" command_parameter(name:command_parameter_name\"-Prefix\") command_argument>bare_word\"P\"))",
            ),
            (
                "Import-Module -ModuleInfo $mi",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-ModuleInfo\") command_argument>variable\"$mi\"))",
            ),
            (
                "Import-Module A, B",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>array_literal_expression(command_argument>bare_word\"A\" ',' command_argument>bare_word\"B\")))",
            ),
            (
                "Import-Module $m",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>variable\"$m\"))",
            ),
            (
                "Import-Module \"$x\\Foo.psm1\"",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>expandable_string('\"' variable\"$x\" string_content\"\\\\Foo.psm1\" '\"')))",
            ),
            (
                "Import-Module (Join-Path $PSScriptRoot x.ps1)",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>parenthesized_expression>pipeline(elements:command(name:command_name>bare_word\"Join-Path\" command_argument>variable\"$PSScriptRoot\" command_argument>bare_word\"x.ps1\"))))",
            ),
            (
                "Import-Module -Force",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Force\")))",
            ),
        ]);
    }

    #[test]
    fn m_expandable_string_operands() {
        check(&[
            (
                ". \"$psscriptroot\\lib.ps1\"",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>expandable_string('\"' variable\"$psscriptroot\" string_content\"\\\\lib.ps1\" '\"'))))",
            ),
            (
                ". \"$env:X\\lib.ps1\"",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>expandable_string('\"' variable\"$env:X\" string_content\"\\\\lib.ps1\" '\"'))))",
            ),
            (
                ". \"$PSScriptRoot$x.ps1\"",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>expandable_string('\"' variable\"$PSScriptRoot\" variable\"$x\" string_content\".ps1\" '\"'))))",
            ),
            (
                ". \"$($x)\\lib.ps1\"",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>expandable_string('\"' interpolation(expression:sub_expression('$(' pipeline(elements:command_expression(expression:variable\"$x\")))) string_content\"\\\\lib.ps1\" '\"'))))",
            ),
            (
                ". '$PSScriptRoot\\lib.ps1'",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>single_quoted_string\"'$PSScriptRoot\\\\lib.ps1'\")))",
            ),
        ]);
    }

    #[test]
    fn n_manifest_hashtables() {
        check(&[
            (
                "# manifest\n@{ RootModule = 'Foo.psm1' }\n",
                "source_file(comment\"# manifest\" pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"RootModule\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Foo.psm1'\")))))))",
            ),
            (
                "@{ 'RootModule' = 'Foo.psm1'; rootmodule = 'x' }",
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:single_quoted_string\"'RootModule'\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Foo.psm1'\"))) hash_entry(key:identifier\"rootmodule\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'x'\"))))))",
            ),
            (
                "@{ NestedModules = 'a.ps1'; FunctionsToExport = 'Get-A' }",
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"NestedModules\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'a.ps1'\"))) hash_entry(key:identifier\"FunctionsToExport\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Get-A'\"))))))",
            ),
            (
                "@{ NestedModules = @('a.ps1', 'b.ps1'); FunctionsToExport = @('Get-A') }",
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"NestedModules\" '=' value:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:array_literal_expression(single_quoted_string\"'a.ps1'\" ',' single_quoted_string\"'b.ps1'\"))))))) hash_entry(key:identifier\"FunctionsToExport\" '=' value:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:single_quoted_string\"'Get-A'\")))))))))",
            ),
            (
                "@{ NestedModules = 'a.ps1', 'b.ps1'; FunctionsToExport = 'A', 'B' }",
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"NestedModules\" '=' value:pipeline(elements:command_expression(expression:array_literal_expression(single_quoted_string\"'a.ps1'\" ',' single_quoted_string\"'b.ps1'\")))) hash_entry(key:identifier\"FunctionsToExport\" '=' value:pipeline(elements:command_expression(expression:array_literal_expression(single_quoted_string\"'A'\" ',' single_quoted_string\"'B'\")))))))",
            ),
            (
                "@{ NestedModules = @(); FunctionsToExport = @() }",
                "source_file>pipeline(elements:command_expression(expression:hashtable_expression('@{' hash_entry(key:identifier\"NestedModules\" '=' value:pipeline(elements:command_expression(expression:array_expression>'@('))) hash_entry(key:identifier\"FunctionsToExport\" '=' value:pipeline(elements:command_expression(expression:array_expression>'@('))))))",
            ),
        ]);
    }

    #[test]
    fn o_name_and_type_variants() {
        check(&[
            (
                "function C.M { }",
                "source_file>function_definition(keyword:'function' name:function_name\"C.M\" body:script_block\"{ }\")",
            ),
            (
                "class C { [D[]]$Q }",
                "source_file>class_definition('class' name:identifier\"C\" body:class_body(members:class_property(types:type_literal>type_name(name:type_identifier\"D\") name:variable\"$Q\")))",
            ),
            (
                "class C { M([System.Collections.Generic.List[string]]$l) { } }",
                "source_file>class_definition('class' name:identifier\"C\" body:class_body(members:class_method(name:identifier\"M\" parameters:parameter_list>parameter(types:type_literal>type_name(name:type_identifier\"System.Collections.Generic.List\" type_name(name:type_identifier\"string\")) name:variable\"$l\") body:statement_block\"{ }\")))",
            ),
        ]);
    }

    #[test]
    fn p_nested_sites() {
        check(&[
            (
                "function F { . ./a.ps1; Import-Module B }",
                "source_file>function_definition(keyword:'function' name:function_name\"F\" body:script_block(pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>bare_word\"./a.ps1\"))) pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"B\"))))",
            ),
            (
                "class C { M() { . ./a.ps1; Import-Module B } }",
                "source_file>class_definition('class' name:identifier\"C\" body:class_body(members:class_method(name:identifier\"M\" parameters:parameter_list\"()\" body:statement_block(pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>bare_word\"./a.ps1\"))) pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"B\"))))))",
            ),
            (
                "$s = { . ./a.ps1; Import-Module B }",
                "source_file>assignment_statement(left:variable\"$s\" operator:'=' right:pipeline(elements:command_expression(expression:script_block(pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>bare_word\"./a.ps1\"))) pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"B\"))))))",
            ),
            (
                "configuration C { Import-DscResource -ModuleName PSDesiredStateConfiguration }",
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block>import_dsc_resource_statement(keyword:'Import-DscResource' elements:command_parameter(name:command_parameter_name\"-ModuleName\") elements:command_argument>bare_word\"PSDesiredStateConfiguration\"))",
            ),
        ]);
    }

    #[test]
    fn q_command_name_variants() {
        check(&[
            (
                "% { $_ }",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"%\" command_argument>script_block>pipeline(elements:command_expression(expression:variable\"$_\"))))",
            ),
            (
                "? { $_ }",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"?\" command_argument>script_block>pipeline(elements:command_expression(expression:variable\"$_\"))))",
            ),
            (
                "ls",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"ls\"))",
            ),
            (
                "Microsoft.PowerShell.Utility\\Write-Output x",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Microsoft.PowerShell.Utility\\\\Write-Output\" command_argument>bare_word\"x\"))",
            ),
            (
                "./x.ps1 a",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"./x.ps1\" command_argument>bare_word\"a\"))",
            ),
            (
                "git.exe status",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"git.exe\" command_argument>bare_word\"status\"))",
            ),
        ]);
    }

    #[test]
    fn r_call_operand_variants() {
        check(&[
            (
                "& Get-Foo -X 1",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>bare_word\"Get-Foo\") command_parameter(name:command_parameter_name\"-X\") command_argument>integer_literal\"1\"))",
            ),
            (
                "& $PSScriptRoot\\lib.ps1",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\"))))",
            ),
            (
                "& $function:Foo",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>variable\"$function:Foo\")))",
            ),
            (
                "& (Get-X)",
                "source_file>pipeline(elements:command(name:invocation_operator_command(operator:'&' command:command_argument>parenthesized_expression>pipeline(elements:command(name:command_name>bare_word\"Get-X\")))))",
            ),
        ]);
    }

    #[test]
    fn s_pipelines_and_nested_commands() {
        check(&[
            (
                "Get-A | Get-B | Get-C",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Get-A\") '|' elements:command(name:command_name>bare_word\"Get-B\") '|' elements:command(name:command_name>bare_word\"Get-C\"))",
            ),
            (
                "Get-A { Get-B }",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Get-A\" command_argument>script_block>pipeline(elements:command(name:command_name>bare_word\"Get-B\"))))",
            ),
        ]);
    }

    #[test]
    fn t_this_invocation_variants() {
        check(&[
            (
                "$this.M()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:variable\"$this\" operator:'.' member:identifier\"M\") arguments:argument_list\"()\")))",
            ),
            (
                "$this.M(@(1, 2), [Foo]::N(3, 4))",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:variable\"$this\" operator:'.' member:identifier\"M\") arguments:argument_list(array_expression('@(' pipeline(elements:command_expression(expression:array_literal_expression(integer_literal\"1\" ',' integer_literal\"2\")))) ',' invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"Foo\") operator:'::' member:identifier\"N\") arguments:argument_list(integer_literal\"3\" ',' integer_literal\"4\"))))))",
            ),
            (
                "$THIS.m(1)",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:variable\"$THIS\" operator:'.' member:identifier\"m\") arguments:argument_list>integer_literal\"1\")))",
            ),
        ]);
    }

    #[test]
    fn u_static_member_variants() {
        check(&[
            (
                "[T]::new()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"T\") operator:'::' member:identifier\"new\") arguments:argument_list\"()\")))",
            ),
            (
                "[T]::M()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"T\") operator:'::' member:identifier\"M\") arguments:argument_list\"()\")))",
            ),
            (
                "[T]::M(1, 2, 3)",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"T\") operator:'::' member:identifier\"M\") arguments:argument_list(integer_literal\"1\" ',' integer_literal\"2\" ',' integer_literal\"3\"))))",
            ),
            (
                "[T]::P",
                "source_file>pipeline(elements:command_expression(expression:member_access(object:type_literal>type_name(name:type_identifier\"T\") operator:'::' member:identifier\"P\")))",
            ),
            (
                "[System.Collections.Generic.List[int]]::new()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:type_literal>type_name(name:type_identifier\"System.Collections.Generic.List\" type_name(name:type_identifier\"int\")) operator:'::' member:identifier\"new\") arguments:argument_list\"()\")))",
            ),
        ]);
    }

    #[test]
    fn v_instance_member_variants() {
        check(&[
            (
                "$x.M()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:variable\"$x\" operator:'.' member:identifier\"M\") arguments:argument_list\"()\")))",
            ),
            (
                "(Get-X).M()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:parenthesized_expression>pipeline(elements:command(name:command_name>bare_word\"Get-X\")) operator:'.' member:identifier\"M\") arguments:argument_list\"()\")))",
            ),
            (
                "$this.P.Q()",
                "source_file>pipeline(elements:command_expression(expression:invocation_expression(function:member_access(object:member_access(object:variable\"$this\" operator:'.' member:identifier\"P\") operator:'.' member:identifier\"Q\") arguments:argument_list\"()\")))",
            ),
            (
                "$x.P",
                "source_file>pipeline(elements:command_expression(expression:member_access(object:variable\"$x\" operator:'.' member:identifier\"P\")))",
            ),
        ]);
    }

    #[test]
    fn w_class_and_enum_variants() {
        check(&[
            (
                "class C : System.Collections.Generic.List[int] { }",
                "source_file>class_definition('class' name:identifier\"C\" ':' base_types:type_name(name:type_identifier\"System.Collections.Generic.List\" type_name(name:type_identifier\"int\")) body:class_body\"{ }\")",
            ),
            (
                "enum E : byte { A }",
                "source_file>enum_definition('enum' name:identifier\"E\" ':' underlying_type:type_name(name:type_identifier\"byte\") body:enum_body(members:enum_member(name:identifier\"A\")))",
            ),
            (
                "class C { [int]$P = (Get-Count) }",
                "source_file>class_definition('class' name:identifier\"C\" body:class_body(members:class_property(types:type_literal>type_name(name:type_identifier\"int\") name:variable\"$P\" '=' value:parenthesized_expression>pipeline(elements:command(name:command_name>bare_word\"Get-Count\")))))",
            ),
        ]);
    }

    #[test]
    fn x_dsc_resource_blocks() {
        let src = "configuration C { Node x { File f { } } }";
        assert_eq!(errors(src), Vec::<String>::new());
        check(&[(
            src,
            "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block>node_statement(keyword:'node' name:bare_word\"x\" body:script_block>resource_statement(type:type_identifier\"File\" name:bare_word\"f\" body:resource_body\"{ }\")))",
        )]);
    }

    /// requirements.requirement.ps-valid-syntax-parse: valid syntax parses clean.
    #[test]
    fn y_valid_syntax_fixtures() {
        let cases: &[(&str, &str)] = &[
            (
                "./x.ps1 a",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"./x.ps1\" command_argument>bare_word\"a\"))",
            ),
            (
                ".\\x.ps1 a",
                "source_file>pipeline(elements:command(name:command_name>bare_word\".\\\\x.ps1\" command_argument>bare_word\"a\"))",
            ),
            (
                "sub\\x.ps1 a",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"sub\\\\x.ps1\" command_argument>bare_word\"a\"))",
            ),
            (
                "Import-Module $PSScriptRoot\\lib.ps1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\")))",
            ),
            (
                "Import-Module -Name $PSScriptRoot\\lib.ps1",
                "source_file>pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_parameter(name:command_parameter_name\"-Name\") command_argument(variable\"$PSScriptRoot\" argument_fragment\"\\\\lib.ps1\")))",
            ),
            (
                "function F { param($a) X }",
                "source_file>function_definition(keyword:'function' name:function_name\"F\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"X\"))))",
            ),
            (
                "filter F { param($a) X }",
                "source_file>function_definition(keyword:'filter' name:function_name\"F\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"X\"))))",
            ),
            (
                "workflow W { param($a) X }",
                "source_file>function_definition(keyword:'workflow' name:function_name\"W\" body:script_block(param_block('param' parameters:parameter_list>parameter(name:variable\"$a\")) pipeline(elements:command(name:command_name>bare_word\"X\"))))",
            ),
            (
                "enum E : byte { A }",
                "source_file>enum_definition('enum' name:identifier\"E\" ':' underlying_type:type_name(name:type_identifier\"byte\") body:enum_body(members:enum_member(name:identifier\"A\")))",
            ),
            (
                "class C : System.Collections.Generic.List[int] { }",
                "source_file>class_definition('class' name:identifier\"C\" ':' base_types:type_name(name:type_identifier\"System.Collections.Generic.List\" type_name(name:type_identifier\"int\")) body:class_body\"{ }\")",
            ),
            (
                "class D : System.IO.Stream { }",
                "source_file>class_definition('class' name:identifier\"D\" ':' base_types:type_name(name:type_identifier\"System.IO.Stream\") body:class_body\"{ }\")",
            ),
            (
                "configuration C { }",
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block\"{ }\")",
            ),
            (
                "configuration C { Node x { File f { } } }",
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block>node_statement(keyword:'node' name:bare_word\"x\" body:script_block>resource_statement(type:type_identifier\"File\" name:bare_word\"f\" body:resource_body\"{ }\")))",
            ),
            (
                "configuration C { Node x { Get-Inner; File f { DestinationPath = (Get-Path); Ensure = 'Present' } } }",
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block>node_statement(keyword:'node' name:bare_word\"x\" body:script_block(pipeline(elements:command(name:command_name>bare_word\"Get-Inner\")) resource_statement(type:type_identifier\"File\" name:bare_word\"f\" body:resource_body(properties:hash_entry(key:identifier\"DestinationPath\" '=' value:pipeline(elements:command_expression(expression:parenthesized_expression>pipeline(elements:command(name:command_name>bare_word\"Get-Path\"))))) properties:hash_entry(key:identifier\"Ensure\" '=' value:pipeline(elements:command_expression(expression:single_quoted_string\"'Present'\"))))))))",
            ),
            (
                "configuration C { Node x { . ./lib.ps1; Import-Module Foo } }",
                "source_file>configuration_definition('configuration' name:command_argument>bare_word\"C\" body:script_block>node_statement(keyword:'node' name:bare_word\"x\" body:script_block(pipeline(elements:command(name:invocation_operator_command(operator:'.' command:command_argument>bare_word\"./lib.ps1\"))) pipeline(elements:command(name:command_name>bare_word\"Import-Module\" command_argument>bare_word\"Foo\")))))",
            ),
            (
                "using module ./Foo.psm1",
                "source_file>using_statement('using' kind:'module' value:command_argument>bare_word\"./Foo.psm1\")",
            ),
        ];
        for (src, _) in cases {
            assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
            assert_eq!(parse(src).root_node().kind(), "source_file", "{src:?}");
        }
        check(cases);
    }

    /// requirements.requirement.ps-error-recovery: invalid input stays a local
    /// region, except a file ending inside nested unclosed constructs.
    #[test]
    fn z_invalid_input_locality() {
        // (A) local regions, alone and after a clean statement.
        let a: &[(&str, &[&str], &str)] = &[
            (
                "(1 + 2",
                &["MISSING )"],
                "source_file>pipeline(elements:command_expression(expression:parenthesized_expression(pipeline(elements:command_expression(expression:binary_expression(left:integer_literal\"1\" operator:'+' right:integer_literal\"2\"))) MISSING ')')))",
            ),
            (
                "Get-A\n(1 + 2",
                &["MISSING )"],
                "source_file(pipeline(elements:command(name:command_name>bare_word\"Get-A\")) pipeline(elements:command_expression(expression:parenthesized_expression(pipeline(elements:command_expression(expression:binary_expression(left:integer_literal\"1\" operator:'+' right:integer_literal\"2\"))) MISSING ')'))))",
            ),
            (
                "$x = @(1",
                &["MISSING )"],
                "source_file>assignment_statement(left:variable\"$x\" operator:'=' right:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:integer_literal\"1\")) MISSING ')'))))",
            ),
            (
                "Get-A\n$x = @(1",
                &["MISSING )"],
                "source_file(pipeline(elements:command(name:command_name>bare_word\"Get-A\")) assignment_statement(left:variable\"$x\" operator:'=' right:pipeline(elements:command_expression(expression:array_expression('@(' pipeline(elements:command_expression(expression:integer_literal\"1\")) MISSING ')')))))",
            ),
            (
                "$x = ",
                &["ERROR \"=\""],
                "source_file(pipeline(elements:command_expression(expression:variable\"$x\")) ERROR>'=')",
            ),
            (
                "Get-A\n$x = ",
                &["ERROR \"=\""],
                "source_file(pipeline(elements:command(name:command_name>bare_word\"Get-A\")) pipeline(elements:command_expression(expression:variable\"$x\")) ERROR>'=')",
            ),
        ];
        for (src, errs, _) in a {
            assert_eq!(parse(src).root_node().kind(), "source_file", "{src:?}");
            assert_eq!(errors(src), *errs, "{src:?}");
            if src.starts_with("Get-A\n") {
                clean_top_pipeline(src, "Get-A");
            }
        }
        check(&a.iter().map(|(s, _, d)| (*s, *d)).collect::<Vec<_>>());

        // (B) the whole-file exception, and its closed variant staying local.
        let open = "Get-A\nfunction F { Get-B\n$x = (1 + \n";
        assert_eq!(parse(open).root_node().kind(), "ERROR");
        assert_eq!(errors(open), [format!("ERROR {open:?}")]);
        let closed = "Get-A\nfunction F { Get-B\n$x = (1 + \n}\nGet-C\n";
        clean_top_pipeline(closed, "Get-A");
        clean_top_pipeline(closed, "Get-C");
        check(&[
            (
                open,
                "ERROR(pipeline(elements:command(name:command_name>bare_word\"Get-A\")) 'function' function_name\"F\" pipeline(elements:command(name:command_name>bare_word\"Get-B\")) variable\"$x\" '=' integer_literal\"1\" '+')",
            ),
            (
                closed,
                "source_file(pipeline(elements:command(name:command_name>bare_word\"Get-A\")) function_definition(keyword:'function' name:function_name\"F\" body:script_block(pipeline(elements:command(name:command_name>bare_word\"Get-B\")) pipeline(elements:command_expression(expression:variable\"$x\")) ERROR('=' integer_literal\"1\" '+'))) pipeline(elements:command(name:command_name>bare_word\"Get-C\")))",
            ),
        ]);
        assert_eq!(errors(closed), ["ERROR \"= (1 + \\n\""]);
    }

    /// solution.constraint.ps-single-grammar-link: exactly one crate exports
    /// `tree_sitter_powershell`, and it is the pinned woodruffw grammar.
    #[test]
    fn single_grammar_link() {
        let manifest = include_str!("../Cargo.toml");
        let deps: Vec<&str> = manifest
            .split("[dependencies]")
            .nth(1)
            .expect("[dependencies]")
            .lines()
            .take_while(|l| !l.starts_with('['))
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(deps.contains(&r#"woodruffw-trough-tree-sitter-powershell = "=0.1.6""#));
        assert!(deps.contains(&r#"tree-sitter = "=0.26.13""#));
        assert!(!deps.iter().any(|l| l.starts_with("tree-sitter-powershell")));

        let pkgs = lock_packages(include_str!("../Cargo.lock"));
        let ps: Vec<_> = pkgs
            .iter()
            .filter(|(n, _)| n.contains("powershell"))
            .collect();
        assert_eq!(ps, [&("woodruffw-trough-tree-sitter-powershell", "0.1.6")]);
        assert!(pkgs.contains(&("tree-sitter", "0.26.13")));
        assert!(!pkgs.iter().any(|(n, _)| *n == "tree-sitter-powershell"));
    }

    #[test]
    fn source_decodes_four_encodings() {
        for s in ["Get-A\n$x = 1\n", "é😀x\n"] {
            let plain = s.as_bytes().to_vec();
            let bom8 = [&[0xEF, 0xBB, 0xBF][..], s.as_bytes()].concat();
            assert_eq!(decode(&plain), s);
            assert_eq!(decode(&bom8), decode(&plain), "UTF-8 BOM dropped");
            assert_eq!(decode(&utf16_bytes(s, false)), s, "UTF-16LE");
            assert_eq!(decode(&utf16_bytes(s, true)), s, "UTF-16BE");
        }
    }

    #[test]
    fn source_decodes_lossily() {
        let r = '\u{FFFD}';
        // (bytes, text, decoded range of the U+FFFD, its raw invalid bytes)
        let cases: [(&[u8], String, usize, &[u8]); 4] = [
            (b"a\xE2\x82b", format!("a{r}b"), 1, b"\xE2\x82"),
            (b"\xFF\xFEa\x00\x62", format!("a{r}"), 1, b"\x62"),
            (
                b"\xFF\xFEa\x00\x00\xD8b\x00",
                format!("a{r}b"),
                1,
                b"\x00\xD8",
            ),
            (
                b"\xFE\xFF\x00a\xD8\x00\x00b",
                format!("a{r}b"),
                1,
                b"\xD8\x00",
            ),
        ];
        for (bytes, text, at, invalid) in cases {
            assert_eq!(decode(bytes), text, "{bytes:?}");
            let d = decode_mapped(bytes);
            assert_eq!(d.raw_offset(d.text.len()), bytes.len());
            // start = the invalid run's start; end = the next char's raw start
            // (or the raw length), i.e. the byte right after the run
            let sp = span(&d, at, at + r.len_utf8());
            assert_eq!(&bytes[sp.start..sp.end], invalid, "{bytes:?}");
        }
    }

    #[test]
    fn source_line_count() {
        for (text, n) in [
            ("", 1),
            ("a", 1),
            ("a\n", 1),
            ("a\nb", 2),
            ("a\nb\n", 2),
            ("a\r\nb\r\n", 2),
            ("a\r\nb\nc", 3),
        ] {
            assert_eq!(line_count(text), n, "{text:?}");
        }
    }

    #[test]
    fn source_spans_are_raw_offsets() {
        // bytes: "ab\n" 0..3, c 3, é 4..6, 😀 6..10, d 10, \n 11, x 12, \n 13
        let s = "ab\ncé😀d\nx\n";
        let encodings = [
            (s.as_bytes().to_vec(), None),
            ([&[0xEF, 0xBB, 0xBF][..], s.as_bytes()].concat(), None),
            (utf16_bytes(s, false), Some(false)),
            (utf16_bytes(s, true), Some(true)),
        ];
        // (start, end, start_line, end_line)
        let ranges = [
            (0, 2, 1, 1),   // single line
            (1, 11, 1, 2),  // multi-line
            (0, 3, 1, 1),   // ends just past a newline
            (4, 6, 2, 2),   // the non-ASCII char
            (6, 10, 2, 2),  // the astral char
            (10, 11, 2, 2), // after the astral char
            (6, 13, 2, 3),
            (12, 14, 3, 3),
        ];
        for (i, (bytes, be)) in encodings.iter().enumerate() {
            let d = decode_mapped(bytes);
            assert_eq!(d.text, s);
            let raw = |b: usize| match (i, be) {
                (0, _) => b,
                (_, None) => b + 3,
                _ => 2 + 2 * s[..b].encode_utf16().count(),
            };
            for (a, b, sl, el) in ranges {
                let sp = span(&d, a, b);
                assert_eq!((sp.start, sp.end), (raw(a), raw(b)), "enc {i} {a}..{b}");
                assert_eq!((sp.start_line, sp.end_line), (sl, el), "enc {i} {a}..{b}");
                assert_eq!(raw_text(bytes, *be, sp.start, sp.end), &s[a..b]);
            }
            assert_eq!(d.raw_offset(s.len()), bytes.len());
        }
        // anchors: after é / after 😀 (2 units = 4 bytes) in UTF-16
        let le = decode_mapped(&utf16_bytes(s, false));
        assert_eq!((le.raw_offset(6), le.raw_offset(10)), (12, 16));
        let bom = decode_mapped(&encodings[1].0);
        assert_eq!((bom.raw_offset(6), bom.raw_offset(10)), (9, 13));
    }

    #[test]
    fn source_position_counts_chars() {
        let s = "ab\ncé😀d";
        assert_eq!(position(s, 0), (1, 1));
        assert_eq!(position(s, 2), (1, 3));
        assert_eq!(position(s, 3), (2, 1));
        assert_eq!(position(s, 6), (2, 3)); // after é (2 bytes, 1 char)
        assert_eq!(position(s, 10), (2, 4)); // after 😀 (4 bytes, 1 char)
    }

    #[test]
    fn diag_formats_lines_and_summary() {
        let d = Diagnostic {
            file: "a/b.ps1".to_string(),
            line: 3,
            col: 7,
            message: "syntax error".to_string(),
        };
        assert_eq!(d.to_string(), "a/b.ps1:3:7: syntax error");
        let mut out = Vec::new();
        write_diagnostic(&mut out, &d).unwrap();
        assert_eq!(out, b"ps: a/b.ps1:3:7: syntax error\n");
        assert_eq!(error_summary(0), "ps: 0 files had syntax errors");
        assert_eq!(error_summary(1), "ps: 1 file had syntax errors");
        assert_eq!(error_summary(2), "ps: 2 files had syntax errors");
    }

    #[test]
    fn scan_parse_args() {
        let a = parse_args(&argv(&[
            "root",
            "--module",
            "a",
            "--module",
            "b",
            "--id-prefix",
            "p",
            "--targets",
            "t.txt",
            "--cache-dir",
            "c",
            "--cache-key",
            "k",
            "skip",
            "--module",
        ]))
        .unwrap();
        assert_eq!(a.root, "root");
        assert_eq!(a.modules, ["a", "b"]);
        assert_eq!(a.id_prefix, "p");
        assert_eq!(a.targets.as_deref(), Some("t.txt"));
        // cache flags eat their values; an unknown arg and a valueless flag are excludes
        assert_eq!(a.excludes, ["skip", "--module"]);

        let d = parse_args(&argv(&["root"])).unwrap();
        assert_eq!(d.id_prefix, "n");
        assert!(d.modules.is_empty() && d.targets.is_none() && d.excludes.is_empty());

        assert!(parse_args(&[])
            .unwrap_err()
            .starts_with("usage: psfrontend"));
    }

    #[test]
    fn scan_is_ps_source() {
        for p in ["a.ps1", "B.PSM1", "c.Psd1"] {
            assert!(is_ps_source(Path::new(p)), "{p}");
        }
        for p in ["a.ps", "a.txt", "ps1"] {
            assert!(!is_ps_source(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn scan_fresh_id_uses_prefix() {
        let mut e = Emitter::new(Path::new("/r"), "ps");
        assert_eq!(e.fresh_id(), "ps1");
        assert_eq!(e.fresh_id(), "ps2");
    }

    #[test]
    fn scan_rec_serialises_unified_keys() {
        let s = |x: &str| x.to_string();
        let cases = [
            (Rec::Module { fqn: s("m") }, "module", vec!["fqn", "type"]),
            (
                Rec::File {
                    path: s("/r/a.ps1"),
                    parent: s("m"),
                    start_line: 1,
                    end_line: 3,
                },
                "file",
                vec!["end_line", "parent", "path", "start_line", "type"],
            ),
            (
                Rec::Struct {
                    id: s("n1"),
                    parent: s("m"),
                    name: s("C"),
                    path: s("/r/a.ps1"),
                    start: 4,
                    end: 20,
                    start_line: 2,
                    end_line: 3,
                },
                "struct",
                vec![
                    "end",
                    "end_line",
                    "id",
                    "name",
                    "parent",
                    "path",
                    "start",
                    "start_line",
                    "type",
                ],
            ),
            (
                Rec::Function {
                    id: s("n2"),
                    parent: s("m"),
                    name: s("f"),
                    params: Vec::new(),
                    file: s("/r/a.ps1"),
                    path: s("/r/a.ps1"),
                    start: 0,
                    end: 9,
                    start_line: 1,
                    end_line: 1,
                },
                "function",
                vec![
                    "end",
                    "end_line",
                    "file",
                    "id",
                    "name",
                    "parent",
                    "path",
                    "start",
                    "start_line",
                    "type",
                ],
            ),
        ];
        for (rec, tag, keys) in cases {
            let v = serde_json::to_value(&rec).unwrap();
            let obj = v.as_object().unwrap();
            assert_eq!(obj["type"], tag);
            let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
            got.sort_unstable();
            assert_eq!(got, keys, "{tag}");
        }
        let v = serde_json::to_value(Rec::Struct {
            id: s("n1"),
            parent: s("m"),
            name: s("C"),
            path: s("p"),
            start: 4,
            end: 20,
            start_line: 2,
            end_line: 3,
        })
        .unwrap();
        assert_eq!(
            (&v["start"], &v["end"], &v["start_line"], &v["end_line"]),
            (&4.into(), &20.into(), &2.into(), &3.into())
        );
        let f = serde_json::to_value(Rec::Function {
            id: s("n2"),
            parent: s("m"),
            name: s("f"),
            params: vec![s("x")],
            file: s("p"),
            path: s("p"),
            start: 0,
            end: 9,
            start_line: 1,
            end_line: 1,
        })
        .unwrap();
        assert_eq!(f["params"], serde_json::json!(["x"]));
    }

    #[test]
    fn paths_normalise_segments() {
        let n = |p: &str| normalise(p.split('/')).map(|v| v.join("/"));
        assert_eq!(n("./a/./b").as_deref(), Some("a/b"));
        assert_eq!(n("a/../b").as_deref(), Some("b"));
        assert_eq!(n("a//b").as_deref(), Some("a/b"));
        assert_eq!(n("../x"), None);
        assert_eq!(n("a/b/../../.."), None);
    }

    #[test]
    fn paths_resolve_against_file_dir() {
        let r = |text| resolve("mods/Foo", text);
        for t in [r"lib\x.ps1", "lib/x.ps1", r".\lib\x.ps1"] {
            assert_eq!(r(t).as_deref(), Some("mods/Foo/lib/x.ps1"), "{t}");
        }
        assert_eq!(
            r(r"lib\sub/x.ps1").as_deref(),
            Some("mods/Foo/lib/sub/x.ps1")
        );
        assert_eq!(r(r"..\Bar\b.psm1").as_deref(), Some("mods/Bar/b.psm1"));
        assert_eq!(r(r"..\..\..\x.ps1"), None);
        assert_eq!(resolve("", "x.ps1").as_deref(), Some("x.ps1"));
        // A path normalising to the repo root resolves to the empty path.
        assert_eq!(resolve("", ".").as_deref(), Some(""));
        assert_eq!(resolve("", ".."), None);
        for t in [
            "/x.ps1",
            r"\x.ps1",
            r"C:\x.ps1",
            r"\\srv\share\x.ps1",
            "~/x.ps1",
            "",
        ] {
            assert_eq!(r(t), None, "{t:?}");
            assert_eq!(resolve("", t), None, "{t:?}");
        }
    }

    #[test]
    fn paths_file_index_case_rules() {
        let idx = FileIndex::new(["a/b.ps1", "a.ps1", "A.ps1"].map(s));
        assert_eq!(idx.find("a/b.ps1"), Some("a/b.ps1"));
        assert_eq!(idx.find("A/B.PS1"), Some("a/b.ps1"));
        assert_eq!(idx.find("a.ps1"), Some("a.ps1"));
        assert_eq!(idx.find("A.ps1"), Some("A.ps1"));
        assert_eq!(idx.find("A.PS1"), None);
        assert_eq!(idx.find("c.ps1"), None);
    }

    #[test]
    fn manifest_wildcard_match() {
        let yes = [
            ("Get-Foo", "get-foo"),
            ("get-foo", "GET-FOO"),
            ("*-Foo", "Get-Foo"),
            ("Get-*o", "Get-Foo"),
            ("Get-*", "Get-Foo"),
            ("*", "Anything"),
            ("*", ""),
            ("G*t-F*", "Get-Foo"),
            ("Get-F?o", "Get-Foo"),
            ("[ab]ar", "Bar"),
            ("[a-c]ar", "car"),
            ("a`*b", "a*b"),
            ("a`[b]", "a[b]"),
            // An unclosed `[` is a literal `[`.
            ("a[b", "a[b"),
        ];
        for (p, n) in yes {
            assert!(wildcard_match(p, n), "{p:?} should match {n:?}");
        }
        let no = [
            ("Get-Foo", "Get-Bar"),
            ("Get-F?o", "Get-Fo"),
            ("Get-F?o", "Get-Fooo"),
            ("[ab]ar", "car"),
            ("[a-c]ar", "dar"),
            ("a`*b", "axxb"),
            ("a`[b]", "ab"),
            ("a[b", "ab"),
            ("Get-*", "Set-Foo"),
        ];
        for (p, n) in no {
            assert!(!wildcard_match(p, n), "{p:?} should not match {n:?}");
        }
    }

    #[test]
    fn manifest_export_spec() {
        let names = ["Remove-A", "Set-Item", "Get-A"];
        assert!(ExportSpec::All.matches("Anything"));
        assert_eq!(ExportSpec::All.exported(names), names);
        let pats = ExportSpec::Patterns(vec![s("Get-*"), s("Set-Item")]);
        assert!(!pats.matches("Remove-A"));
        assert_eq!(pats.exported(names), ["Set-Item", "Get-A"]);
        assert_eq!(ExportSpec::Patterns(vec![s("*")]).exported(names), names);
        assert!(ExportSpec::Patterns(vec![]).exported(names).is_empty());
    }

    #[test]
    fn units_bare_name_strips_scope_prefixes() {
        for n in [
            "global:Foo",
            "Script:Foo",
            "LOCAL:Foo",
            "private:Foo",
            "GLOBAL:Foo",
        ] {
            assert_eq!(bare_name(n), "Foo", "{n:?}");
        }
        for n in ["Foo", "env:Foo"] {
            assert_eq!(bare_name(n), n);
        }
    }

    #[test]
    fn units_param_type_defaults_to_object() {
        assert_eq!(param_type(Some("int")), "int");
        let list = "System.Collections.Generic.List[string]";
        assert_eq!(param_type(Some(list)), list);
        assert_eq!(param_type(None), "object");
    }

    #[test]
    fn units_property_type_name_strips_array_suffixes() {
        for t in ["Foo", "Foo[]", "Foo[][]"] {
            assert_eq!(property_type_name(t), "Foo", "{t:?}");
        }
        assert_eq!(property_type_name("System.String"), "System.String");
    }

    #[test]
    fn units_assign_ordinals_by_start_offset() {
        use UnitKind::*;
        // Declaration order: script body, class, its members, then the function.
        let mut units = vec![
            unit_at(ScriptBody, "<script>", 0, 100),
            unit_at(Class, "C", 0, 80),
            unit_at(Method, "M", 40, 50),
            unit_at(Constructor, "C", 60, 70),
            unit_at(Function, "F", 10, 30),
            unit_at(Function, "Nested", 20, 25),
        ];
        assign_ordinals(&mut units);
        let got: Vec<usize> = units.iter().map(|u| u.ordinal).collect();
        assert_eq!(got, [1, 2, 5, 6, 3, 4]);

        // Equal start offsets keep declaration order.
        let mut ties = vec![
            unit_at(ScriptBody, "<script>", 0, 20),
            unit_at(Function, "B", 5, 10),
            unit_at(Function, "A", 5, 15),
        ];
        assign_ordinals(&mut ties);
        let got: Vec<usize> = ties.iter().map(|u| u.ordinal).collect();
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn identity_base_relative() {
        assert_eq!(
            base_relative("mods/Foo/lib/Helpers.ps1", "mods/Foo"),
            "lib/Helpers.ps1"
        );
        assert_eq!(base_relative("a.ps1", ""), "a.ps1");
        assert_eq!(
            base_relative("mods/Foo/Other.ps1", "mods/Bar"),
            "../Foo/Other.ps1"
        );
    }

    #[test]
    fn identity_emitted_name() {
        let p = vec!["int".to_string(), "string".to_string()];
        assert_eq!(emitted_name("M", &p, true), "M(int,string)");
        assert_eq!(emitted_name("M", &[], true), "M()");
        assert_eq!(emitted_name("M", &p, false), "M");
        assert_eq!(emitted_name("M", &[], false), "M");
    }

    #[test]
    fn identity_record_key() {
        assert_eq!(record_key("", "build.ps1"), ".build.ps1");
        assert_eq!(record_key(".build.ps1", "F"), ".build.ps1.F");
        assert_eq!(record_key("build", "x.ps1"), "build.x.ps1");
        assert_ne!(record_key("", "build.ps1"), "build");
    }

    #[test]
    fn identity_render() {
        let p = vec!["int".to_string(), "string".to_string()];
        assert_eq!(render("scope", "F", &[], false), "scope.F");
        assert_eq!(render("scope", "M", &p, true), "scope.M(int,string)");
        assert_eq!(render("scope", "M", &[], true), "scope.M()");
        assert_eq!(render("scope", "M", &p, false), "scope.M");
        assert_eq!(render(".a.ps1", "F", &[], false), ".a.ps1.F");
    }

    #[test]
    fn identity_is_disambiguated() {
        let paths: std::collections::BTreeSet<String> =
            ["dir/a.ps1", "x.ps1"].map(String::from).into();
        for f in [
            "m.s.F//dir/a.ps1:3",
            "m.s.F//x.ps1:12",
            "m.s.C.M(int)//dir/a.ps1:3",
        ] {
            assert!(is_disambiguated(f, &paths), "{f:?}");
        }
        for f in [
            "m.s.F//dir/a.ps1:",
            "m.s.F//dir/a.ps1:3x",
            "m.s.F//other.ps1:3",
            "m.s.F/dir/a.ps1:3",
            "m.s.C.M(int)",
            "m.s.F",
        ] {
            assert!(!is_disambiguated(f, &paths), "{f:?}");
        }
        let odd: std::collections::BTreeSet<String> = ["d.(x)/a.ps1".to_string()].into();
        assert!(is_disambiguated("m.s.F//d.(x)/a.ps1:7", &odd));
        assert!(!is_disambiguated("m.s.F//(x)/a.ps1:7", &odd));
    }

    #[test]
    fn resolve_import_arg_kind() {
        assert_eq!(import_arg(PathArg::Computed), ImportArg::Computed);
        assert_eq!(
            import_arg(st("Foo.psm1", true)),
            ImportArg::Path(st("Foo.psm1", true))
        );
        // Extension checks ignore ASCII case (Foo.PSD1).
        for t in [
            r"lib\Foo.psm1",
            "lib/Foo",
            r".\Foo",
            "./Foo",
            ".Foo",
            "Foo.psm1",
            "Foo.PSD1",
            "Foo.ps1",
            "Foo.dll",
        ] {
            assert_eq!(
                import_arg(st(t, false)),
                ImportArg::Path(st(t, false)),
                "{t:?}"
            );
        }
        for t in [
            "Foo",
            "Foo.Bar",
            "Microsoft.PowerShell.Utility",
            "PSReadLine",
        ] {
            assert_eq!(import_arg(st(t, false)), ImportArg::Name(s(t)), "{t:?}");
        }
    }

    #[test]
    fn resolve_dep_kind_as_str() {
        assert_eq!(DepKind::DotSource.as_str(), "dot-source");
        assert_eq!(DepKind::ImportModule.as_str(), "import-module");
    }

    // In-box list per upstream PowerShell v7.6.0: src/Modules manifests,
    // PSGalleryModules.csproj and InitialSessionState.cs.
    #[test]
    fn resolve_inbox_modules_and_category() {
        let suffixes = [
            "Core",
            "Management",
            "Utility",
            "Security",
            "Host",
            "Diagnostics",
            "Archive",
            "PSResourceGet",
            "ThreadJob",
        ];
        let want: Vec<String> = suffixes
            .iter()
            .map(|x| format!("Microsoft.PowerShell.{x}"))
            .collect();
        assert_eq!(
            INBOX_MODULES,
            want.iter().map(String::as_str).collect::<Vec<_>>()
        );
        let lower: BTreeSet<String> = INBOX_MODULES.iter().map(|m| m.to_lowercase()).collect();
        assert_eq!(lower.len(), INBOX_MODULES.len());
        // Case-insensitive whole-name match.
        for m in INBOX_MODULES {
            for v in [m.to_string(), m.to_lowercase(), m.to_uppercase()] {
                assert_eq!(module_category(&v), "stdlib", "{v:?}");
            }
        }
        for v in [
            "microsoft.powershell.management",
            "MICROSOFT.POWERSHELL.THREADJOB",
            "Microsoft.PowerShell.psresourceget",
        ] {
            assert_eq!(module_category(v), "stdlib", "{v:?}");
        }
        for v in [
            "Microsoft.PowerShell.SecretManagement",
            "Microsoft.PowerShell.Crescendo",
            "PSReadLine",
            "CimCmdlets",
            "ThreadJob",
            "Pester",
            "Microsoft.PowerShell",
            "Microsoft.PowerShell.Utility.Extra",
            "Microsoft.PowerShellX",
            "MicrosoftXPowerShell.Utility",
        ] {
            assert_eq!(module_category(v), "external", "{v:?}");
        }
    }

    #[test]
    fn resolve_nameless_key() {
        assert_eq!(nameless_key("dir/a.ps1", 17), "<nameless@dir/a.ps1:17>");
        assert_eq!(nameless_key("a.ps1", 0), "<nameless@a.ps1:0>");
    }

    #[test]
    fn resolve_select_member_by_name_and_arity() {
        let f = class_file();
        assert_eq!(select_member(&f, 1, "M", 1, false), Some(3));
        assert_eq!(select_member(&f, 1, "m", 0, false), Some(2));
        assert_eq!(select_member(&f, 1, "M", 3, false), None);
        assert_eq!(select_member(&f, 1, "X", 0, false), None);
        assert_eq!(select_member(&f, 1, "new", 1, true), Some(7));
        assert_eq!(select_member(&f, 1, "new", 2, true), None);
        // A method is never selected for a constructor call: M(int,string) has
        // two parameters, M(int) one (that slot goes to new(int)).
        assert_eq!(select_member(&f, 1, "M", 2, true), None);
        assert_eq!(select_member(&f, 1, "M", 1, true), Some(7));
        // Two members with the same name and parameter count are ambiguous.
        let mut dup = class_file();
        dup.units
            .push(member(UnitKind::Method, "M", 1, &["string"]));
        assert_eq!(select_member(&dup, 1, "M", 1, false), None);
    }

    #[test]
    fn resolve_split_qualified() {
        assert_eq!(
            split_qualified(r"Microsoft.PowerShell.Utility\Write-Output"),
            Some(("Microsoft.PowerShell.Utility", "Write-Output"))
        );
        for n in ["Get-Foo", r"\x", r"x\", r"cd\"] {
            assert_eq!(split_qualified(n), None, "{n:?}");
        }
    }

    #[test]
    fn resolve_command_target_keeps_qualified_name() {
        let q = r"M\Foo";
        let one = CommandRes::One {
            path: s("p.ps1"),
            index: 2,
        };
        assert_eq!(
            command_target(one, q),
            Target::Unit {
                path: s("p.ps1"),
                index: 2
            }
        );
        for (res, category) in [
            (CommandRes::Many, "unknown"),
            (CommandRes::Builtin, "stdlib"),
            (CommandRes::External, "external"),
        ] {
            assert_eq!(
                command_target(res, q),
                Target::Unresolved {
                    name: s(q),
                    category
                },
                "{category}"
            );
        }
    }

    // Sample names per PowerShell v7.6.0 InitialSessionState.cs, the src/Modules
    // manifests and PSGalleryModules.csproj, TypeResolver.cs at v7.6.0, and the
    // Learn powershell-7.6 module and about_Type_Accelerators pages.
    #[test]
    fn builtins_table_invariants() {
        assert_eq!(COMMANDS.len(), 319);
        assert_eq!(ALIASES.len(), 148);
        assert_eq!(ACCELERATORS.len(), 107);
        let mut per_module = std::collections::BTreeMap::new();
        for (m, _) in COMMANDS {
            *per_module.entry(*m).or_insert(0) += 1;
        }
        let want: std::collections::BTreeMap<&str, usize> = [
            ("", 37),
            ("Microsoft.PowerShell.Core", 61),
            ("Microsoft.PowerShell.Management", 61),
            ("Microsoft.PowerShell.Utility", 118),
            ("Microsoft.PowerShell.Security", 15),
            ("Microsoft.PowerShell.Host", 2),
            ("Microsoft.PowerShell.Diagnostics", 3),
            ("Microsoft.PowerShell.Archive", 2),
            ("Microsoft.PowerShell.PSResourceGet", 19),
            ("Microsoft.PowerShell.ThreadJob", 1),
        ]
        .into_iter()
        .collect();
        assert_eq!(per_module, want);
        // Strictly increasing lowercased keys: sorted, no case-insensitive duplicate.
        let cmd_keys: Vec<_> = COMMANDS
            .iter()
            .map(|(m, c)| (c.to_lowercase(), m.to_lowercase()))
            .collect();
        assert!(cmd_keys.windows(2).all(|w| w[0] < w[1]), "COMMANDS order");
        for table in [ALIASES, ACCELERATORS] {
            let keys: Vec<_> = table.iter().map(|(k, _)| k.to_lowercase()).collect();
            assert!(keys.windows(2).all(|w| w[0] < w[1]), "{:?} order", table[0]);
        }
        for (a, t) in ALIASES {
            assert!(COMMANDS.iter().any(|(_, c)| c == t), "{a} -> {t}");
        }
        let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        for (m, _) in COMMANDS.iter().filter(|(m, _)| !m.is_empty()) {
            assert!(INBOX_MODULES.iter().any(|i| eq(i, m)), "{m}");
        }
        for i in INBOX_MODULES {
            assert!(COMMANDS.iter().any(|(m, _)| eq(m, i)), "{i}");
        }
        let non_system: Vec<_> = ACCELERATORS
            .iter()
            .filter(|(_, t)| !t.starts_with("System."))
            .collect();
        assert_eq!(non_system.len(), 6, "{non_system:?}");
    }

    #[test]
    fn builtins_is_builtin() {
        for n in [
            "Get-ChildItem",
            "Compress-Archive",
            "Clear-Host",
            "Switch-Process",
            "ls",
            "gci",
            "gcb",
            "cd\\",
            "Microsoft.PowerShell.Management\\Get-ChildItem",
            "Microsoft.PowerShell.Utility\\Write-Output",
            "Microsoft.PowerShell.Archive\\Compress-Archive",
            "Microsoft.PowerShell.ThreadJob\\Start-ThreadJob",
            "MICROSOFT.POWERSHELL.ARCHIVE\\compress-archive",
        ] {
            assert!(is_builtin(n), "{n:?}");
        }
        for (m, c) in COMMANDS {
            for v in [c.to_string(), c.to_uppercase(), c.to_lowercase()] {
                assert!(is_builtin(&v), "{v:?}");
            }
            if !m.is_empty() {
                for v in [
                    format!("{}\\{}", m.to_uppercase(), c.to_lowercase()),
                    format!("{}\\{}", m.to_lowercase(), c.to_uppercase()),
                ] {
                    assert!(is_builtin(&v), "{v:?}");
                }
            }
        }
        for (a, _) in ALIASES {
            for v in [a.to_string(), a.to_uppercase(), a.to_lowercase()] {
                assert!(is_builtin(&v), "{v:?}");
            }
        }
        for n in [
            "Get-CimInstance",
            "Set-PSReadLineOption",
            "CimCmdlets\\Get-CimInstance",
            "ThreadJob\\Start-ThreadJob",
            "git",
            "git.exe",
            "Get-Foo",
            "Import-Modules",
            "",
            "Contoso.Tools\\Get-Item",
            "Microsoft.PowerShell.Utility\\Get-ChildItem",
            "Microsoft.PowerShell.Core\\ls",
            "Microsoft.PowerShell.Core\\Clear-Host",
        ] {
            assert!(!is_builtin(n), "{n:?}");
        }
    }

    #[test]
    fn builtins_is_system_type() {
        for n in [
            "string",
            "STRING",
            "hashtable",
            "pscustomobject",
            "ordered",
            "adsi",
            "System",
            "system",
            "System.IO.File",
            "SYSTEM.io.path",
            "System.Collections.Generic.List[string]",
        ] {
            assert!(is_system_type(n), "{n:?}");
        }
        for n in [
            "ciminstance",
            "CimSession",
            "pspropertyexpression",
            "math",
            "object",
            "console",
            "Systems.X",
            "MySystem.String",
            "Foo",
            "",
        ] {
            assert!(!is_system_type(n), "{n:?}");
        }
    }
}

mod int {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn diag_error_regions_over_parsed_sources() {
        let syntax = |l, c| (l, c, s("syntax error"));
        assert!(regions("Get-A\n$x = 1\n").is_empty());
        // one broken statement, one region at its start
        assert_eq!(regions("Get-A\n$x = )"), [syntax(2, 4)]);
        // an outer ERROR holding nested ones is reported once
        let nested = "Get-A }} {{ ]] = (1 + 2";
        assert_eq!(errors(nested).len(), 3, "{:?}", errors(nested));
        assert_eq!(regions(nested), [syntax(1, 1)]);
        // an unclosed construct
        assert_eq!(regions("(1 + 2"), [(1, 7, s("missing )"))]);
        // two separate regions, in source order
        assert_eq!(
            regions("Get-A\n$x = )\nGet-B\n(1 + 2"),
            [syntax(2, 6), (4, 7, s("missing )"))]
        );
        // columns count characters, not bytes
        assert_eq!(regions("'é'; $x = )"), [syntax(1, 9)]);
        // the whole-file region: one diagnostic where the ERROR root starts
        assert_error_root(NESTED_OPEN_AT_EOF);
        let root = parse(NESTED_OPEN_AT_EOF).root_node().start_byte();
        let (l, c) = position(NESTED_OPEN_AT_EOF, root);
        assert_eq!(regions(NESTED_OPEN_AT_EOF), [syntax(l, c)]);
    }

    #[test]
    fn scan_file_decodes_every_encoding() {
        let text = "Get-A\n$x = 'é😀'\nGet-B\n";
        let n = line_count(text);
        for bytes in [
            text.as_bytes().to_vec(),
            utf8_bom(text),
            utf16_bytes(text, false),
            utf16_bytes(text, true),
        ] {
            let mut e = Emitter::new(Path::new("/r"), "n");
            assert!(e.scan_file("/r/a.ps1", &bytes).is_empty());
            let files: Vec<_> = e
                .records()
                .iter()
                .filter(|r| matches!(r, Rec::File { .. }))
                .collect();
            assert_eq!(files, [&file("/r/a.ps1", "", n)], "{bytes:?}");
        }
    }

    #[test]
    fn scan_file_modules_per_directory() {
        let mut e = Emitter::new(Path::new("/r"), "n");
        e.scan_file("/r/a.ps1", b"Get-A\n");
        e.scan_file("/r/sub/dir/b.ps1", b"Get-B\nGet-C\n");
        e.scan_file("/r/sub/dir/c.ps1", b"Get-D\n");
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s("") },
                file("/r/a.ps1", "", 1),
                Rec::Module { fqn: s("sub/dir") },
                file("/r/sub/dir/b.ps1", "sub/dir", 2),
                file("/r/sub/dir/c.ps1", "sub/dir", 1),
            ]
        );
    }

    #[test]
    fn scan_file_recovers_from_syntax_errors() {
        let mut e = Emitter::new(Path::new("/r"), "n");
        let broken = e.scan_file("/r/a.ps1", b"Get-A\n$x = )");
        assert_eq!(broken.len(), 1);
        assert_eq!(
            (broken[0].file.as_str(), broken[0].line, broken[0].col),
            ("/r/a.ps1", 2, 4)
        );
        assert!(e.scan_file("/r/b.ps1", b"Get-B\n").is_empty());
        let whole = e.scan_file("/r/c.ps1", NESTED_OPEN_AT_EOF.as_bytes());
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].message, "syntax error");
        assert!(e.scan_file("/r/d.ps1", b"Get-D\n").is_empty());
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s("") },
                file("/r/a.ps1", "", 2),
                file("/r/b.ps1", "", 1),
                file("/r/c.ps1", "", line_count(NESTED_OPEN_AT_EOF)),
                file("/r/d.ps1", "", 1),
            ]
        );
        assert_eq!(e.error_summary(), "ps: 2 files had syntax errors");
    }

    #[test]
    fn retain_targets_keeps_target_files_and_their_modules() {
        let mut e = Emitter::new(Path::new("/r"), "n");
        for p in ["/r/a.ps1", "/r/sub/b.ps1", "/r/other/c.ps1"] {
            e.scan_file(p, b"Get-A\n");
        }
        let targets: HashSet<String> = [s("/r/a.ps1"), s("/r/other/c.ps1")].into();
        e.retain_targets(&targets);
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s("") },
                file("/r/a.ps1", "", 1),
                Rec::Module { fqn: s("other") },
                file("/r/other/c.ps1", "other", 1),
            ]
        );
    }

    #[test]
    fn write_jsonl_one_object_per_line() {
        let mut e = Emitter::new(Path::new("/r"), "n");
        e.scan_file("/r/a.ps1", b"Get-A\n");
        e.scan_file("/r/sub/b.ps1", b"Get-B\nGet-C");
        let mut out = Vec::new();
        e.write_jsonl(&mut out).unwrap();
        let lines: Vec<serde_json::Value> = std::str::from_utf8(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                serde_json::json!({"type": "module", "fqn": ""}),
                serde_json::json!({"type": "file", "path": "/r/a.ps1", "parent": "", "start_line": 1, "end_line": 1}),
                serde_json::json!({"type": "module", "fqn": "sub"}),
                serde_json::json!({"type": "file", "path": "/r/sub/b.ps1", "parent": "sub", "start_line": 1, "end_line": 2}),
            ]
        );
        assert!(out.ends_with(b"\n"));
    }

    #[test]
    fn paths_path_arg_over_invocation_operands() {
        for (operand_src, want) in operand_cells() {
            for op in [".", "&"] {
                let src = format!("{op} {operand_src}\n");
                assert_eq!(arg_of(&src, |r| operand(r, op)), want, "{src:?}");
            }
            // nested in a function body, not a direct child of the root
            let src = format!("function F {{\n    . {operand_src}\n}}\n");
            assert_eq!(arg_of(&src, |r| operand(r, ".")), want, "{src:?}");
        }
    }

    #[test]
    fn paths_path_arg_over_import_module_arguments() {
        for (operand_src, want) in operand_cells() {
            for src in [
                format!("Import-Module {operand_src}\n"),
                format!("Import-Module -Name {operand_src}\n"),
            ] {
                let got = arg_of(&src, |r| first_arg(r, &src, "Import-Module"));
                assert_eq!(got, want, "{src:?}");
            }
        }
    }

    #[test]
    fn paths_path_arg_over_manifest_values() {
        for src in [
            "@{ RootModule = 'Foo.psm1' }\n",
            "# manifest\n@{ RootModule = 'Foo.psm1' }\n",
            MANIFEST,
        ] {
            let got = arg_of(src, |r| manifest_value(r, src, "RootModule"));
            assert_eq!(got, st("Foo.psm1", false), "{src:?}");
        }
    }

    #[test]
    fn paths_path_arg_resolves_per_anchoring_form() {
        let dir = "mods/Foo";
        for (src, want) in [
            (". ./lib.ps1\n", "mods/Foo/lib.ps1"),
            (". $PSScriptRoot\\sub\\lib.ps1\n", "mods/Foo/sub/lib.ps1"),
            (". \"$PSScriptRoot/../Bar/lib.ps1\"\n", "mods/Bar/lib.ps1"),
            (". ${PSScriptRoot}\\x.ps1\n", "mods/Foo/x.ps1"),
            ("@{ RootModule = 'Foo.psm1' }\n", "mods/Foo/Foo.psm1"),
        ] {
            let got = match arg_of(src, |r| {
                operand(r, ".").or_else(|| manifest_value(r, src, "RootModule"))
            }) {
                PathArg::Static { text, .. } => resolve(dir, &text),
                PathArg::Computed => None,
            };
            assert_eq!(got.as_deref(), Some(want), "{src:?}");
        }
    }

    #[test]
    fn manifest_read_manifest_over_parsed_trees() {
        let all = || ExportSpec::All;
        let foo = || Some(st("Foo.psm1", false));
        let ab = || vec![st("a.ps1", false), st("b.ps1", false)];
        let cells: Vec<(&str, Option<ManifestKeys>)> = vec![
            ("@{ RootModule = 'Foo.psm1' }\n", mk(foo(), vec![], all())),
            (
                "@{ NestedModules = 'a.ps1' }\n",
                mk(None, vec![st("a.ps1", false)], all()),
            ),
            (
                "@{ NestedModules = @('a.ps1', 'b.ps1') }\n",
                mk(None, ab(), all()),
            ),
            (
                "@{ NestedModules = 'a.ps1', 'b.ps1' }\n",
                mk(None, ab(), all()),
            ),
            (
                "@{ FunctionsToExport = 'Get-A' }\n",
                mk(None, vec![], pats(&["Get-A"])),
            ),
            (
                "@{ FunctionsToExport = @('Get-A', 'Set-*') }\n",
                mk(None, vec![], pats(&["Get-A", "Set-*"])),
            ),
            (
                "@{ FunctionsToExport = '*' }\n",
                mk(None, vec![], pats(&["*"])),
            ),
            (
                "@{ FunctionsToExport = @() }\n",
                mk(None, vec![], pats(&[])),
            ),
            (
                "@{ rootmodule = 'Foo.psm1'; 'NESTEDMODULES' = 'a.ps1'; \"functionsToExport\" = 'Get-A' }\n",
                mk(foo(), vec![st("a.ps1", false)], pats(&["Get-A"])),
            ),
            // a repeated key: the first wins
            (
                "@{ 'RootModule' = 'Foo.psm1'; rootmodule = 'x' }\n",
                mk(foo(), vec![], all()),
            ),
            (
                "@{ NestedModules = @('a.ps1', $x) }\n",
                mk(None, vec![st("a.ps1", false), PathArg::Computed], all()),
            ),
            ("@{ Setting = 1 }\n", None),
            ("Get-A\n$x = 1\n", None),
        ];
        for (src, want) in cells {
            assert_source_root(src);
            assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
            assert_eq!(keys_of(src), want, "{src:?}");
        }

        // (1a) local regions: a syntax error outside the hashtable ...
        assert_source_root(MANIFEST_BROKEN_ELSEWHERE);
        assert!(!errors(MANIFEST_BROKEN_ELSEWHERE).is_empty());
        let full = mk(
            foo(),
            vec![st("lib\\Helpers.ps1", false)],
            pats(&["Get-Foo"]),
        );
        assert_eq!(keys_of(MANIFEST_BROKEN_ELSEWHERE), full);
        assert_eq!(keys_of(MANIFEST), full);
        // ... and an entry inside a local ERROR node, skipped with its value
        assert_source_root(MANIFEST_BROKEN_ENTRY);
        let errs = errors(MANIFEST_BROKEN_ENTRY);
        assert!(
            errs.iter().any(|e| e.starts_with("ERROR")
                && e.contains("NestedModules")
                && e.contains("a.ps1")),
            "{errs:?}"
        );
        assert_eq!(
            keys_of(MANIFEST_BROKEN_ENTRY),
            mk(foo(), vec![], pats(&["Get-Foo"]))
        );

        // (1b) the whole-file region: no manifest
        assert_error_root(MANIFEST_OPEN_AT_EOF);
        assert_eq!(keys_of(MANIFEST_OPEN_AT_EOF), None);
    }

    #[test]
    fn manifest_assign_over_path_lists() {
        assert_eq!(errors(FOO_PSD1), Vec::<String>::new());
        let map = module_fixture();
        let foo = map.manifest_of("mods/Foo/Foo.psd1").expect("Foo manifest");
        assert_eq!(
            (foo.path.as_str(), foo.base.as_str()),
            ("mods/Foo/Foo.psd1", "mods/Foo")
        );
        assert_eq!(foo.exports, pats(&["Get-Foo"]));
        // entry order; a bare name, .dll, .psd1, missing file, path above the
        // root, computed value and the Bar-claimed ../both.ps1 claim nothing
        assert_eq!(
            foo.members,
            [
                "mods/Foo/Foo.psm1",
                "mods/Foo/lib/Helpers.ps1",
                "mods/Foo/Other.ps1",
                "mods/Foo/lib/case.ps1",
                "mods/Shared/s.ps1",
                "mods/Foo/anchored.ps1",
            ]
        );
        let bar = map.manifest_of("mods/Bar/Bar.psd1").expect("Bar manifest");
        assert_eq!(bar.members, ["mods/both.ps1"]);
        let at = |p: &str| (map.module(p), map.base(p));
        let foo_mod = (s("mods/Foo/Foo.psd1"), s("mods/Foo"));
        for p in [
            "mods/Foo/Foo.psd1",
            "mods/Foo/Foo.psm1",
            "mods/Foo/lib/Helpers.ps1",
            "mods/Foo/Other.ps1",
            "mods/Foo/lib/case.ps1",
            "mods/Foo/anchored.ps1",
            // reached through ..\: the manifest's base, not its own directory
            "mods/Shared/s.ps1",
        ] {
            assert_eq!(at(p), foo_mod, "{p}");
        }
        assert_eq!(at("mods/both.ps1"), (s("mods/Bar/Bar.psd1"), s("mods/Bar")));
        for p in [
            "mods/Foo/loose.ps1",
            "mods/Foo/Other.psd1",
            "mods/Foo/Broken.psd1",
            "mods/Foo/x.dll",
        ] {
            assert_eq!(at(p), (s("mods/Foo"), s("mods/Foo")), "{p}");
        }
        assert_eq!(at("run.ps1"), (s(""), s("")));
    }

    #[test]
    fn manifest_module_map_queries() {
        let map = module_fixture();
        let holder = |p| map.manifest_of(p).map(|m| m.path.as_str());
        assert_eq!(
            holder("mods/Foo/lib/Helpers.ps1"),
            Some("mods/Foo/Foo.psd1")
        );
        assert_eq!(holder("mods/Foo/Foo.psd1"), Some("mods/Foo/Foo.psd1"));
        assert_eq!(holder("mods/Foo/loose.ps1"), None);
        assert_eq!(holder("run.ps1"), None);
        assert_eq!(
            map.modules(),
            ["", "mods/Bar/Bar.psd1", "mods/Foo", "mods/Foo/Foo.psd1"]
        );
        assert_eq!(map.module("elsewhere/deep/x.ps1"), "elsewhere/deep");
        assert_eq!(map.module("x.ps1"), "");
    }

    #[test]
    fn scan_file_parents_files_to_manifest_and_directory_modules() {
        let psd1 = MODULE_FILES.map(|p| (p, module_source(p)));
        let map = module_map(&MODULE_FILES, &psd1);
        let e = module_emitter(Some(map), &MODULE_FILES);
        let foo = "mods/Foo/Foo.psd1";
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s(foo) },
                module_file("mods/Foo/Foo.psd1", foo),
                module_file("mods/Foo/Foo.psm1", foo),
                module_file("mods/Foo/lib/Helpers.ps1", foo),
                Rec::Module { fqn: s("mods/Foo") },
                module_file("mods/Foo/loose.ps1", "mods/Foo"),
                module_file("mods/Foo/Data.psd1", "mods/Foo"),
                Rec::Module { fqn: s("") },
                module_file("root.ps1", ""),
            ]
        );
        // every File once, under exactly one emitted Module
        let modules: Vec<&str> = e
            .records()
            .iter()
            .filter_map(|r| match r {
                Rec::Module { fqn } => Some(fqn.as_str()),
                _ => None,
            })
            .collect();
        let files: Vec<&str> = e
            .records()
            .iter()
            .filter_map(|r| match r {
                Rec::File { path, parent, .. } => {
                    assert_eq!(modules.iter().filter(|m| **m == parent).count(), 1);
                    Some(path.as_str())
                }
                _ => None,
            })
            .collect();
        let want: Vec<String> = MODULE_FILES.iter().map(|p| format!("/r/{p}")).collect();
        assert_eq!(files, want);
    }

    #[test]
    fn scan_file_parents_a_doubly_claimed_member_to_the_earlier_manifest() {
        let alt = "@{ RootModule = 'Foo.psm1' }\n";
        let files = [
            "mods/Foo/Foo.psd1",
            "mods/Foo/Alt.psd1",
            "mods/Foo/Foo.psm1",
        ];
        // listed after Foo.psd1, but sorting first
        let map = module_map(
            &files,
            &[("mods/Foo/Foo.psd1", MANIFEST), ("mods/Foo/Alt.psd1", alt)],
        );
        let e = module_emitter(Some(map), &["mods/Foo/Foo.psm1"]);
        assert_eq!(
            e.records(),
            [
                Rec::Module {
                    fqn: s("mods/Foo/Alt.psd1")
                },
                module_file("mods/Foo/Foo.psm1", "mods/Foo/Alt.psd1"),
            ]
        );
    }

    #[test]
    fn retain_targets_keeps_a_target_files_manifest_module() {
        let psd1 = MODULE_FILES.map(|p| (p, module_source(p)));
        let mut e = module_emitter(Some(module_map(&MODULE_FILES, &psd1)), &MODULE_FILES);
        e.retain_targets(&[s("/r/mods/Foo/lib/Helpers.ps1")].into());
        let foo = "mods/Foo/Foo.psd1";
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s(foo) },
                module_file("mods/Foo/lib/Helpers.ps1", foo),
            ]
        );
    }

    #[test]
    fn scan_file_without_set_modules_keeps_directory_parents() {
        let e = module_emitter(None, &MODULE_FILES);
        let foo = "mods/Foo";
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s(foo) },
                module_file("mods/Foo/Foo.psd1", foo),
                module_file("mods/Foo/Foo.psm1", foo),
                Rec::Module {
                    fqn: s("mods/Foo/lib")
                },
                module_file("mods/Foo/lib/Helpers.ps1", "mods/Foo/lib"),
                module_file("mods/Foo/loose.ps1", foo),
                module_file("mods/Foo/Data.psd1", foo),
                Rec::Module { fqn: s("") },
                module_file("root.ps1", ""),
            ]
        );
    }

    #[test]
    fn units_scan_script_body_and_functions() {
        use UnitKind::*;
        assert_eq!(errors(SCRIPT_UNITS), Vec::<String>::new());
        let fu = units_of(SCRIPT_UNITS.as_bytes());
        let whole = Span {
            start: 0,
            end: SCRIPT_UNITS.len(),
            start_line: 1,
            end_line: line_count(SCRIPT_UNITS),
        };
        assert_eq!(fu.path, "f.ps1");
        assert_eq!(fu.file_span, whole);
        assert_eq!(fu.units[0].span, whole);
        assert_eq!(
            unit_rows(&fu),
            [
                (ScriptBody, "<script>", 1),
                (Function, "Foo", 2),
                (Function, "Nested", 3),
                (Function, "Bar", 4),
                (Function, "Baz", 5),
            ]
        );
        assert!(fu
            .units
            .iter()
            .all(|u| u.owner.is_none() && u.params.is_empty() && !u.overloaded));
    }

    #[test]
    fn units_scan_bare_names_and_first_spelling() {
        let src = "function global:Get-A { }\nfunction script:B { }\nfunction Local:C { }\nfunction private:D { }\nfunction Foo { }\nfunction foo { }\n";
        assert_eq!(errors(src), Vec::<String>::new());
        let f = UnitKind::Function;
        assert_eq!(
            unit_rows(&units_of(src.as_bytes())),
            [
                (UnitKind::ScriptBody, "<script>", 1),
                (f, "Get-A", 2),
                (f, "B", 3),
                (f, "C", 4),
                (f, "D", 5),
                (f, "Foo", 6),
            ]
        );
    }

    #[test]
    fn units_scan_class_members_and_enum() {
        use UnitKind::*;
        assert_eq!(errors(CLASS_UNITS), Vec::<String>::new());
        let fu = units_of(CLASS_UNITS.as_bytes());
        // the repeated m([Int]) signature and the later class c are dropped
        assert_eq!(
            unit_rows(&fu),
            [
                (ScriptBody, "<script>", 1),
                (Class, "C", 2),
                (Method, "M", 3),
                (Method, "M", 4),
                (Method, "M", 5),
                (Constructor, "new", 6),
                (Method, "N", 7),
                (Enum, "E", 8),
            ]
        );
        let members: Vec<(Vec<String>, bool, Option<usize>)> = fu.units[2..7]
            .iter()
            .map(|u| (u.params.clone(), u.overloaded, u.owner))
            .collect();
        let p = |ps: &[&str]| ps.iter().map(|t| s(t)).collect::<Vec<_>>();
        assert_eq!(
            members,
            [
                (p(&[]), true, Some(1)),
                (p(&["int"]), true, Some(1)),
                (p(&["object"]), true, Some(1)),
                (p(&["int"]), false, Some(1)),
                (p(&["string"]), false, Some(1)),
            ]
        );
        assert_eq!(fu.units[1].property_types, ["D", "string"]);
        assert_eq!((fu.units[1].owner, fu.units[7].owner), (None, None));

        // a method explicitly named `new` joins the constructor's overload group
        let src = "class W {\n    W() { }\n    [int] new([int]$i) { return 1 }\n}\n";
        assert_eq!(errors(src), Vec::<String>::new());
        let fu = units_of(src.as_bytes());
        assert_eq!(
            unit_rows(&fu),
            [
                (ScriptBody, "<script>", 1),
                (Class, "W", 2),
                (Constructor, "new", 3),
                (Method, "new", 4),
            ]
        );
        assert!(fu.units[2].overloaded && fu.units[3].overloaded);
        assert_eq!(fu.units[3].params, ["int"]);
    }

    #[test]
    fn units_scan_ordinals_follow_start_offsets() {
        for src in [SCRIPT_UNITS, CLASS_UNITS] {
            let fu = units_of(src.as_bytes());
            let mut by_start: Vec<&Unit> = fu.units.iter().collect();
            by_start.sort_by_key(|u| (u.kind != UnitKind::ScriptBody, u.span.start));
            let ords: Vec<usize> = by_start.iter().map(|u| u.ordinal).collect();
            assert_eq!(ords, (1..=fu.units.len()).collect::<Vec<_>>(), "{src:?}");
        }
    }

    #[test]
    fn units_scan_spans_are_raw_offsets_across_encodings() {
        let text = "function Foo { 'é😀' }\nGet-A\nclass C {\n    [int] M() { return 1 }\n}\n";
        let mut lines = Vec::new();
        for (bytes, be, bom) in [
            (text.as_bytes().to_vec(), None, 0),
            (utf8_bom(text), None, 3),
            (utf16_bytes(text, false), Some(false), 2),
            (utf16_bytes(text, true), Some(true), 2),
        ] {
            let fu = units_of(&bytes);
            // the whole file in raw bytes, the BOM included
            let whole = Span {
                start: 0,
                end: bytes.len(),
                start_line: 1,
                end_line: line_count(text),
            };
            assert_eq!(fu.file_span, whole, "{bom}");
            assert_eq!(fu.units[0].span, whole, "{bom}");
            assert_eq!(fu.units[1].span.start, bom, "Foo starts past the BOM");
            let src = decode_mapped(&bytes);
            let tree = parse(&src.text);
            let mut got = Vec::new();
            for name in ["Foo", "C"] {
                let node = decls(tree.root_node(), &src.text, name)[0];
                let u = fu.units.iter().find(|u| u.name == name).expect(name);
                assert_eq!(u.span, span(&src, node.start_byte(), node.end_byte()));
                let raw = raw_text(&bytes, be, u.span.start, u.span.end);
                assert_eq!(raw, &src.text[node.byte_range()], "{name} {bom}");
                got.push((u.span.start_line, u.span.end_line));
            }
            lines.push(got);
        }
        assert_eq!(lines[0], [(1, 1), (3, 5)]);
        assert!(lines.iter().all(|l| *l == lines[0]), "{lines:?}");
    }

    #[test]
    fn units_scan_using_and_dsc_yield_no_unit() {
        for src in [USING_MODULE, DSC_CONFIG] {
            let fu = units_of(src.as_bytes());
            assert_eq!(
                unit_rows(&fu),
                [(UnitKind::ScriptBody, "<script>", 1)],
                "{src:?}"
            );
        }
    }

    #[test]
    fn units_scan_local_error_regions_keep_outside_units() {
        use UnitKind::*;
        // a broken statement between the declarations
        let src = "function F { }\nGet-A\n$x = )\nGet-B\nclass C { }\n";
        assert_source_root(src);
        assert!(!errors(src).is_empty());
        for name in ["F", "C"] {
            assert_eq!(decl_errors(src, name), [(false, false)], "{name}");
        }
        let rows = [
            (ScriptBody, "<script>", 1),
            (Function, "F", 2),
            (Class, "C", 3),
        ];
        assert_eq!(unit_rows(&units_of(src.as_bytes())), rows);

        // an ERROR inside G's body, H nested in that body outside it
        let src = "function G { function H { }\n$x = (1 + \n}\n";
        assert_source_root(src);
        assert_eq!(decl_errors(src, "G"), [(false, true)]);
        assert_eq!(decl_errors(src, "H"), [(false, false)]);
        let fu = units_of(src.as_bytes());
        let rows = [
            (ScriptBody, "<script>", 1),
            (Function, "G", 2),
            (Function, "H", 3),
        ];
        assert_eq!(unit_rows(&fu), rows);
        let tree = parse(src);
        let g = decls(tree.root_node(), src, "G")[0];
        let d = decode_mapped(src.as_bytes());
        assert_eq!(fu.units[1].span, span(&d, g.start_byte(), g.end_byte()));

        // an ERROR inside K's class body, the method Go outside it
        let src = "class K { [int] Go() { return 1 }\n$x = (1 + \n}\n";
        assert_source_root(src);
        assert_eq!(decl_errors(src, "K"), [(false, true)]);
        assert_eq!(decl_errors(src, "Go"), [(false, false)]);
        let fu = units_of(src.as_bytes());
        let rows = [
            (ScriptBody, "<script>", 1),
            (Class, "K", 2),
            (Method, "Go", 3),
        ];
        assert_eq!(unit_rows(&fu), rows);
        assert_eq!(fu.units[2].owner, Some(1));
    }

    #[test]
    fn units_scan_skips_declarations_inside_error_nodes() {
        use UnitKind::*;
        // the stray `}` wraps the broken statement, declarations included, in an ERROR
        let broken = "$z = 1 + function Inside-F { } class InsideK { [int] Go() { return 1 } } }\nfunction Inside-F { }\nfunction After { }\n";
        assert_source_root(broken);
        assert_eq!(
            decl_errors(broken, "Inside-F"),
            [(true, false), (false, false)]
        );
        for name in ["InsideK", "Go"] {
            assert_eq!(decl_errors(broken, name), [(true, false)], "{name}");
        }
        assert_eq!(decl_errors(broken, "After"), [(false, false)]);
        let fu = units_of(broken.as_bytes());
        let rows = [
            (ScriptBody, "<script>", 1),
            (Function, "Inside-F", 2),
            (Function, "After", 3),
        ];
        assert_eq!(unit_rows(&fu), rows);
        // the ordinals of the same fixture with the broken statement deleted
        let clean = "function Inside-F { }\nfunction After { }\n";
        assert_eq!(unit_rows(&units_of(clean.as_bytes())), rows);
        // the kept Inside-F is the later one, outside the region
        let tree = parse(broken);
        let later = decls(tree.root_node(), broken, "Inside-F")[1];
        let d = decode_mapped(broken.as_bytes());
        let later = span(&d, later.start_byte(), later.end_byte());
        assert_eq!(fu.units[1].span, later);

        // a function nested in G's body inside that body's ERROR node
        let src = "function G { function Inner { } +=\nfunction H { }\n}\n";
        assert_source_root(src);
        assert_eq!(decl_errors(src, "G"), [(false, true)]);
        assert_eq!(decl_errors(src, "Inner"), [(true, false)]);
        assert_eq!(decl_errors(src, "H"), [(false, false)]);
        let rows = [
            (ScriptBody, "<script>", 1),
            (Function, "G", 2),
            (Function, "H", 3),
        ];
        assert_eq!(unit_rows(&units_of(src.as_bytes())), rows);

        // a method inside the class body's ERROR node
        let src = "class K { [int] Bad() { return 2 } else\n[int] Ok() { return 3 }\n}\n";
        assert_source_root(src);
        assert_eq!(decl_errors(src, "K"), [(false, true)]);
        assert_eq!(decl_errors(src, "Bad"), [(true, false)]);
        assert_eq!(decl_errors(src, "Ok"), [(false, false)]);
        let fu = units_of(src.as_bytes());
        let rows = [
            (ScriptBody, "<script>", 1),
            (Class, "K", 2),
            (Method, "Ok", 3),
        ];
        assert_eq!(unit_rows(&fu), rows);
        assert_eq!(
            (fu.units[2].overloaded, fu.units[2].owner),
            (false, Some(1))
        );
    }

    #[test]
    fn units_scan_whole_file_error_yields_only_script() {
        let src = "function Keep { }\nGet-A\nfunction F { Get-B\n$x = (1 + \n";
        assert_error_root(src);
        let fu = units_of(src.as_bytes());
        let whole = Span {
            start: 0,
            end: src.len(),
            start_line: 1,
            end_line: line_count(src),
        };
        assert_eq!(fu.file_span, whole);
        let mut script = unit_at(UnitKind::ScriptBody, "<script>", 0, 0);
        script.span = whole;
        script.ordinal = 1;
        assert_eq!(fu.units, [script]);
    }

    #[test]
    fn units_scan_names_feed_export_matcher() {
        let fu = units_of(b"function global:Get-A { }\nfunction Set-B { }\n");
        let names = fu
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Function)
            .map(|u| u.name.as_str());
        let spec = ExportSpec::Patterns(vec![s("Get-*")]);
        assert_eq!(spec.exported(names), ["Get-A"]);
    }

    #[test]
    fn identity_resolve_script_names() {
        let foo = "@{ NestedModules = @('lib\\Helpers.ps1', '..\\Shared\\s.ps1') }\n";
        let (_, _, id) = resolved(&[
            ("mods/x/a.ps1", "Get-A\n"),
            ("mods/Foo/Foo.psd1", foo),
            ("mods/Foo/lib/Helpers.ps1", "Get-A\n"),
            ("mods/Shared/s.ps1", "Get-A\n"),
        ]);
        let keys = id.keys();
        let at = |p| (id.script_name(p), id.script_parent(p));
        assert_eq!(at("mods/x/a.ps1"), (Some("a.ps1"), s("mods/x")));
        assert!(keys.contains(&s("mods/x.a.ps1")));
        let foo_mod = s("mods/Foo/Foo.psd1");
        assert_eq!(
            at("mods/Foo/lib/Helpers.ps1"),
            (Some("lib/Helpers.ps1"), foo_mod.clone())
        );
        assert!(keys.contains(&s("mods/Foo/Foo.psd1.lib/Helpers.ps1")));
        // reached through ..\: named by its .. path from the manifest's base
        assert_eq!(at("mods/Shared/s.ps1"), (Some("../Shared/s.ps1"), foo_mod));
        assert!(keys.contains(&s("mods/Foo/Foo.psd1.../Shared/s.ps1")));
        assert_eq!(id.script_name("mods/Foo/Foo.psd1"), None);
    }

    #[test]
    fn identity_resolve_unique_script_names() {
        // root x.a.ps1 and .x/a.ps1 share .x.a.ps1; an unrelated .PSM1 is plain
        let (_, _, id) = resolved(&[
            ("x.a.ps1", "Get-A\n"),
            (".x/a.ps1", "Get-A\n"),
            ("z/U.PSM1", "Get-A\n"),
        ]);
        assert_eq!(id.script_name("x.a.ps1"), Some("x.a.ps1//x.a.ps1"));
        assert_eq!(id.script_name(".x/a.ps1"), Some("a.ps1//.x/a.ps1"));
        assert_eq!(id.script_name("z/U.PSM1"), Some("U.PSM1"));
        // a.b.c.ps1 twice; A.b.c.ps1 differs only in case and stays plain
        let (_, _, id) = resolved(&[
            ("a.b/c.ps1", "Get-A\n"),
            ("a/b.c.ps1", "Get-A\n"),
            ("A/b.c.ps1", "Get-A\n"),
        ]);
        assert_eq!(id.script_name("a.b/c.ps1"), Some("c.ps1//a.b/c.ps1"));
        assert_eq!(id.script_name("a/b.c.ps1"), Some("b.c.ps1//a/b.c.ps1"));
        assert_eq!(id.script_name("A/b.c.ps1"), Some("b.c.ps1"));
        // a script keyed like a directory module yields; the Module does not
        let (map, _, id) = resolved(&[("a/b.ps1", "Get-A\n"), ("a.b.ps1/f.ps1", "Get-A\n")]);
        assert_eq!(id.script_name("a/b.ps1"), Some("b.ps1//a/b.ps1"));
        assert_eq!(id.script_name("a.b.ps1/f.ps1"), Some("f.ps1"));
        assert_eq!(map.modules(), ["a", "a.b.ps1"]);
        assert!(id.keys().contains(&s("a.b.ps1")));
    }

    #[test]
    fn identity_resolve_root_script_beside_same_named_directory() {
        let (map, _, id) = resolved(&[
            ("build.ps1", "function F {}\n"),
            ("build/x.ps1", "function F {}\n"),
        ]);
        assert_eq!(
            (map.module("build.ps1"), map.base("build.ps1")),
            (s(""), s(""))
        );
        assert_eq!(
            (map.module("build/x.ps1"), map.base("build/x.ps1")),
            (s("build"), s("build"))
        );
        assert_eq!(map.modules(), ["", "build"]);
        let keys: BTreeSet<String> = id.keys().into_iter().collect();
        let want = [
            "",
            "build",
            ".build.ps1",
            ".build.ps1.<script>",
            ".build.ps1.F",
            "build.x.ps1",
            "build.x.ps1.<script>",
            "build.x.ps1.F",
        ];
        assert_eq!(keys, want.map(s).into_iter().collect());
        let at = |p| (id.script_name(p), id.script_parent(p));
        assert_eq!(at("build.ps1"), (Some("build.ps1"), s("")));
        assert_eq!(at("build/x.ps1"), (Some("x.ps1"), s("build")));
        for (p, scope) in [("build.ps1", ".build.ps1"), ("build/x.ps1", "build.x.ps1")] {
            for (i, name) in [(0, "<script>"), (1, "F")] {
                assert_eq!(id.unit_parent(p, i), scope, "{p} {i}");
                assert_eq!(id.unit_name(p, i), name, "{p} {i}");
            }
        }
    }

    #[test]
    fn identity_resolve_type_names() {
        // class key m.a.ps1.B equals a directory module's; a unique class is plain
        let (map, files, id) = resolved(&[
            ("m/a.ps1", "class B { }\n"),
            ("m.a.ps1.B/f.ps1", "Get-A\n"),
            ("m/u.ps1", "class U { }\n"),
        ]);
        assert_eq!(
            unit_names(&id, &files, "m/a.ps1"),
            ["<script>", "B//m/a.ps1:2"]
        );
        assert_eq!(unit_names(&id, &files, "m/u.ps1"), ["<script>", "U"]);
        assert!(map.modules().contains(&s("m.a.ps1.B")));
        assert!(id.keys().contains(&s("m.a.ps1.B")));
        // class ps1 keyed like script m/a.ps1.ps1: the type yields, the script not
        let (_, files, id) =
            resolved(&[("m/a.ps1.ps1", "Get-A\n"), ("m/a.ps1", "class ps1 { }\n")]);
        assert_eq!(id.script_name("m/a.ps1.ps1"), Some("a.ps1.ps1"));
        assert_eq!(id.script_name("m/a.ps1"), Some("a.ps1"));
        assert_eq!(
            unit_names(&id, &files, "m/a.ps1"),
            ["<script>", "ps1//m/a.ps1:2"]
        );
        // a class whose key already reads //<scanned path>:<digits>
        let (map, mut files, paths) = identity_fixture(&[("m/c.ps1", "class C { }\n")]);
        files[0].units[1].name = s("C//m/c.ps1:9");
        let id = identity::resolve(&map, &files, &paths);
        assert_identity(&map, &files, &id);
        assert_eq!(id.unit_name("m/c.ps1", 1), "C//m/c.ps1:9//m/c.ps1:2");
    }

    #[test]
    fn identity_resolve_function_names() {
        // function C.M and method C.M share m.s.ps1.C.M: both suffixed
        let lone = "function C.M { }\nclass C {\n    M() { }\n}\n";
        let (_, files, id) = resolved(&[("m/s.ps1", lone)]);
        assert_eq!(
            unit_names(&id, &files, "m/s.ps1"),
            ["<script>", "C.M//m/s.ps1:2", "C", "M//m/s.ps1:4"]
        );
        assert_eq!(id.unit_parent("m/s.ps1", 1), "m.s.ps1");
        assert_eq!(id.unit_parent("m/s.ps1", 3), "m.s.ps1.C");
        // functions keyed like a script Struct and a Module yield; those do not
        let (map, files, id) = resolved(&[
            (
                "p/q.ps1",
                "function r.ps1 { }\nfunction mod { }\nfunction ok { }\n",
            ),
            ("p/q.ps1.r.ps1", "Get-A\n"),
            ("p.q.ps1.mod/f.ps1", "Get-A\n"),
        ]);
        assert_eq!(
            unit_names(&id, &files, "p/q.ps1"),
            ["<script>", "r.ps1//p/q.ps1:2", "mod//p/q.ps1:3", "ok"]
        );
        assert_eq!(id.script_name("p/q.ps1.r.ps1"), Some("q.ps1.r.ps1"));
        assert!(map.modules().contains(&s("p.q.ps1.mod")));
        // overloads carry their signature and miss function C.M
        let over = "function C.M { }\nclass C {\n    M() { }\n    M([int]$x) { }\n}\n";
        let (_, files, id) = resolved(&[("m/s.ps1", over)]);
        assert_eq!(
            unit_names(&id, &files, "m/s.ps1"),
            ["<script>", "C.M", "C", "M()", "M(int)"]
        );
        assert!(id.keys().contains(&s("m.s.ps1.C.M(int)")));
        // M(int) keyed like a root directory module: signature kept, then suffix
        let (map, files, id) = resolved(&[
            ("s.ps1", "class C {\n    M() { }\n    M([int]$x) { }\n}\n"),
            (".s.ps1.C.M(int)/f.ps1", "Get-A\n"),
        ]);
        assert_eq!(
            unit_names(&id, &files, "s.ps1"),
            ["<script>", "C", "M()", "M(int)//s.ps1:4"]
        );
        assert!(map.modules().contains(&s(".s.ps1.C.M(int)")));
        // renamed scripts scope their functions; same names across scripts stay
        let (_, _, id) = resolved(&[
            ("x.a.ps1", "function G { }\n"),
            (".x/a.ps1", "function G { }\n"),
        ]);
        assert_eq!(id.unit_parent("x.a.ps1", 1), ".x.a.ps1//x.a.ps1");
        assert_eq!(id.unit_parent(".x/a.ps1", 1), ".x.a.ps1//.x/a.ps1");
        assert_eq!(id.unit_name("x.a.ps1", 1), "G");
        assert_eq!(id.unit_name(".x/a.ps1", 1), "G");
    }

    #[test]
    fn finish_emits_script_and_unit_records_in_ordinal_order() {
        let manifest = "@{ NestedModules = @('a.ps1') }\n";
        let e = assert_emitted(&[("m/a.ps1", EMIT_SCRIPT), ("m/M.psd1", manifest)]);
        let (path, md) = ("/r/m/a.ps1", "m/M.psd1");
        let (sk, ck) = ("m/M.psd1.a.ps1", "m/M.psd1.a.ps1.C");
        let n = line_count(EMIT_SCRIPT);
        let whole = ((0, EMIT_SCRIPT.len()), (1, n));
        let at = |decl: &str| {
            let start = EMIT_SCRIPT.find(decl).expect(decl);
            (start, start + decl.len())
        };
        let class = &EMIT_SCRIPT[at("class C").0..at("\nenum").0];
        type Loc = ((usize, usize), (usize, usize));
        let st = |id: &str, parent: &str, name: &str, ((start, end), (l1, l2)): Loc| Rec::Struct {
            id: s(id),
            parent: s(parent),
            name: s(name),
            path: s(path),
            start,
            end,
            start_line: l1,
            end_line: l2,
        };
        let func =
            |id: &str, parent: &str, name: &str, params: &[&str], ((start, end), (l1, l2)): Loc| {
                Rec::Function {
                    id: s(id),
                    parent: s(parent),
                    name: s(name),
                    params: params.iter().map(|p| s(p)).collect(),
                    file: s(path),
                    path: s(path),
                    start,
                    end,
                    start_line: l1,
                    end_line: l2,
                }
            };
        let has = |from: &str, to: &str| Rec::Contains {
            from: s(from),
            to: s(to),
        };
        // the .psd1 manifest yields its File only; M([Int]$y) repeats M(int) and is dropped
        assert_eq!(
            e.records(),
            [
                Rec::Module { fqn: s(md) },
                file(path, md, n),
                file("/r/m/M.psd1", md, 1),
                st("n1", md, "a.ps1", whole),
                func("n2", sk, "<script>", &[], whole),
                has("n1", "n2"),
                st("n3", sk, "C", (at(class), (2, 8))),
                has("n1", "n3"),
                func("n4", ck, "new", &[], (at("C() { }"), (4, 4))),
                has("n3", "n4"),
                func("n5", ck, "M()", &[], (at("M() { }"), (5, 5))),
                has("n3", "n5"),
                func("n6", ck, "M(int)", &["int"], (at("M([int]$x) { }"), (6, 6))),
                has("n3", "n6"),
                st("n7", sk, "E", (at("enum E { A; B }"), (9, 9))),
                has("n1", "n7"),
                func("n8", sk, "F", &[], (at("function F { }"), (10, 10))),
                has("n1", "n8"),
            ]
        );
        // each overload is a group of one, keyed by its signed name
        let recs = e.records();
        assert_eq!(
            rendered_of(recs, &format!("{ck}.M()")),
            "ps.m/M.psd1.a.ps1.C.M()"
        );
        assert_eq!(
            rendered_of(recs, &format!("{ck}.M(int)")),
            "ps.m/M.psd1.a.ps1.C.M(int)"
        );
    }

    #[test]
    fn finish_reports_raw_offsets_of_a_utf16_script() {
        let narrow = emit_tree(&[("a.ps1", EMIT_SCRIPT)]);
        let bytes = utf16_bytes(EMIT_SCRIPT, false);
        let wide = assert_emitted(&[("a.ps1", &bytes)]);
        let located = |e: &Emitter| -> Vec<(String, String, usize, usize, usize, usize)> {
            e.records()
                .iter()
                .filter_map(|r| match r {
                    Rec::Struct {
                        parent,
                        name,
                        start,
                        end,
                        start_line,
                        end_line,
                        ..
                    }
                    | Rec::Function {
                        parent,
                        name,
                        start,
                        end,
                        start_line,
                        end_line,
                        ..
                    } => Some((
                        parent.clone(),
                        name.clone(),
                        *start,
                        *end,
                        *start_line,
                        *end_line,
                    )),
                    _ => None,
                })
                .collect()
        };
        let (w, n) = (located(&wide), located(&narrow));
        assert_eq!(w.len(), 8);
        // the script Struct and <script>: the whole file, the BOM included
        for row in &w[..2] {
            let span = (row.2, row.3, row.4, row.5);
            assert_eq!(
                span,
                (0, bytes.len(), 1, line_count(EMIT_SCRIPT)),
                "{}",
                row.1
            );
        }
        for (w, n) in w.iter().zip(&n) {
            assert_eq!((&w.0, &w.1, w.4, w.5), (&n.0, &n.1, n.4, n.5));
        }
        for (w, n) in w.iter().zip(&n).skip(2) {
            let raw = raw_text(&bytes, Some(false), w.2, w.3);
            assert_eq!(raw, &EMIT_SCRIPT[n.2..n.3], "{}", w.1);
        }
    }

    #[test]
    fn finish_keys_are_unique_across_the_scan() {
        let e = assert_emitted(&UNIQUE_TREE);
        let recs = e.records();
        let mut suffixed: Vec<(&str, &str)> = recs
            .iter()
            .filter_map(|r| match r {
                Rec::Struct { path, name, .. } | Rec::Function { path, name, .. }
                    if name.contains("//") =>
                {
                    Some((path.as_str(), name.as_str()))
                }
                _ => None,
            })
            .collect();
        suffixed.sort();
        // scripts by their path, units by //<path>:<ordinal>; the build pair never
        assert_eq!(
            suffixed,
            [
                ("/r/.x/a.ps1", "a.ps1//.x/a.ps1"),
                ("/r/a.b/c.ps1", "c.ps1//a.b/c.ps1"),
                ("/r/a/b.c.ps1", "b.c.ps1//a/b.c.ps1"),
                ("/r/a/b.ps1", "b.ps1//a/b.ps1"),
                ("/r/m/a.ps1", "B//m/a.ps1:2"),
                ("/r/m/s.ps1", "C.M//m/s.ps1:2"),
                ("/r/m/s.ps1", "M//m/s.ps1:4"),
                ("/r/s.ps1", "M(int)//s.ps1:4"),
                ("/r/x.a.ps1", "x.a.ps1//x.a.ps1"),
            ]
        );
        let struct_parent = |n: &str| {
            recs.iter()
                .find_map(|r| match r {
                    Rec::Struct { parent, name, .. } if name == n => Some(parent.as_str()),
                    _ => None,
                })
                .expect(n)
        };
        let functions = |p: &str| -> Vec<&str> {
            recs.iter()
                .filter_map(|r| match r {
                    Rec::Function { parent, name, .. } if parent == p => Some(name.as_str()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(struct_parent("x.a.ps1//x.a.ps1"), "");
        assert_eq!(struct_parent("a.ps1//.x/a.ps1"), ".x");
        let keys: BTreeSet<String> = ps_rendered(recs).into_iter().map(|(k, _)| k).collect();
        assert!(keys.contains(".s.ps1.C.M()") && keys.contains(".s.ps1.C.M(int)//s.ps1:4"));
        assert!(!keys.contains(".s.ps1.C.M"));
        // the build pair: a leading-dot root scope beside the build module
        for m in ["", "build"] {
            assert!(recs.contains(&Rec::Module { fqn: s(m) }), "{m:?}");
        }
        assert_eq!(struct_parent("build.ps1"), "");
        assert_eq!(rendered_of(recs, ".build.ps1"), "ps..build.ps1");
        assert_eq!(functions(".build.ps1"), ["<script>", "F"]);
        assert_eq!(rendered_of(recs, ".build.ps1.F"), "ps..build.ps1.F");
        assert_eq!(struct_parent("x.ps1"), "build");
        assert_eq!(rendered_of(recs, "build.x.ps1"), "ps.build.x.ps1");
        assert_eq!(functions("build.x.ps1"), ["<script>", "F"]);
        assert_eq!(rendered_of(recs, "build.x.ps1.F"), "ps.build.x.ps1.F");
        // same-named functions in two files stay plain
        assert_eq!(functions("q.one.ps1"), ["<script>", "G"]);
        assert_eq!(functions("q.two.ps1"), ["<script>", "G"]);
    }

    #[test]
    fn retain_targets_keeps_one_files_records_as_the_full_run_emits_them() {
        let full = emit_tree(&UNIQUE_TREE);
        for (p, _) in UNIQUE_TREE {
            let target = format!("/r/{p}");
            let mut e = emit_tree(&UNIQUE_TREE);
            e.retain_targets(&[target.clone()].into());
            let module = full
                .records()
                .iter()
                .find_map(|r| match r {
                    Rec::File { path, parent, .. } if *path == target => Some(parent),
                    _ => None,
                })
                .expect(p);
            let ids: std::collections::HashSet<&String> = full
                .records()
                .iter()
                .filter_map(|r| match r {
                    Rec::Struct { id, path, .. } | Rec::Function { id, path, .. }
                        if *path == target =>
                    {
                        Some(id)
                    }
                    _ => None,
                })
                .collect();
            // an edge stays only when both ends are kept
            let want: Vec<Rec> = full
                .records()
                .iter()
                .filter(|r| match r {
                    Rec::Module { fqn } => fqn == module,
                    Rec::File { path, .. }
                    | Rec::Struct { path, .. }
                    | Rec::Function { path, .. } => *path == target,
                    Rec::Contains { from, to } => ids.contains(from) && ids.contains(to),
                    Rec::Uses { from, .. } | Rec::UnresolvedUse { from, .. } => {
                        ids.contains(from)
                    }
                    Rec::Unresolved { fqn, .. } => full.records().iter().any(|u| {
                        matches!(u, Rec::UnresolvedUse { from, to } if ids.contains(from) && to == fqn)
                    }),
                })
                .cloned()
                .collect();
            let edges = want
                .iter()
                .filter(|r| matches!(r, Rec::Contains { .. }))
                .count();
            assert!(ids.len() >= 2, "{p}");
            assert_eq!(edges, ids.len() - 1, "{p}: one edge per unit");
            assert_eq!(e.records(), want, "{p}");
        }
    }

    #[test]
    fn resolve_manifest_stems_over_assigned_manifests() {
        let (_, _, stems) = resolve_fixture();
        let foo = StemMatch::One(s("mods/Foo/Foo.psd1"));
        assert_eq!(stems.find("foo"), foo);
        assert_eq!(stems.find("FOO"), foo);
        // Bar.psd1 and Bar.PSD1 share the stem case-insensitively
        assert_eq!(stems.find("bar"), StemMatch::Many);
        // a data .psd1 and a non-manifest file are no stems
        assert_eq!(stems.find("Data"), StemMatch::None);
        assert_eq!(stems.find("Helpers"), StemMatch::None);
    }

    #[test]
    fn resolve_site_dot_source() {
        let (_, index, stems) = resolve_fixture();
        let dep = |arg: PathArg| {
            let site = DepSite {
                kind: SiteKind::DotSource(arg),
                raw_offset: 7,
            };
            resolve_site(&site, "scripts/a.ps1", &index, &stems)
        };
        let file = |p: &str| Dep::File {
            path: s(p),
            kind: DepKind::DotSource,
        };
        let nameless = Dep::Unresolved {
            name: s("<nameless@scripts/a.ps1:7>"),
            category: "unknown",
        };
        // exact case preferred among case variants
        assert_eq!(dep(st("b.ps1", false)), file("scripts/b.ps1"));
        // several matches all differing in case
        assert_eq!(dep(st("B.PS1", false)), nameless);
        // a single case-insensitive match
        assert_eq!(
            dep(st("..\\mods\\Foo\\lib\\helpers.ps1", false)),
            file("mods/Foo/lib/Helpers.ps1")
        );
        assert_eq!(
            dep(st("..\\mods\\Foo\\Foo.psm1", true)),
            file("mods/Foo/Foo.psm1")
        );
        assert_eq!(
            dep(st("..\\mods\\Foo\\Data.psd1", false)),
            file("mods/Foo/Data.psd1")
        );
        for text in ["missing.ps1", "..\\..\\x.ps1", "C:\\x.ps1"] {
            assert_eq!(dep(st(text, false)), nameless, "{text}");
        }
        assert_eq!(dep(PathArg::Computed), nameless);
    }

    #[test]
    fn resolve_site_import_module_path() {
        let (_, index, stems) = resolve_fixture();
        let dep = |arg: ImportArg| {
            let site = DepSite {
                kind: SiteKind::Import(arg),
                raw_offset: 11,
            };
            resolve_site(&site, "scripts/a.ps1", &index, &stems)
        };
        let path = |t: &str| ImportArg::Path(st(t, false));
        let file = |p: &str| Dep::File {
            path: s(p),
            kind: DepKind::ImportModule,
        };
        let nameless = Dep::Unresolved {
            name: s("<nameless@scripts/a.ps1:11>"),
            category: "unknown",
        };
        assert_eq!(
            dep(path("..\\mods\\Foo\\Foo.psm1")),
            file("mods/Foo/Foo.psm1")
        );
        assert_eq!(
            dep(path("..\\mods\\Foo\\Foo.psd1")),
            file("mods/Foo/Foo.psd1")
        );
        // any scanned .psd1, a data file included
        assert_eq!(
            dep(path("..\\mods\\Foo\\Data.psd1")),
            file("mods/Foo/Data.psd1")
        );
        for t in [
            "b.ps1",
            "..\\tools\\x.dll",
            "..\\mods\\Foo\\Missing.psm1",
            "..\\..\\Foo.psm1",
            "C:\\mods\\Foo\\Foo.psm1",
        ] {
            assert_eq!(dep(path(t)), nameless, "{t}");
        }
        assert_eq!(dep(ImportArg::Path(PathArg::Computed)), nameless);
        assert_eq!(dep(ImportArg::Computed), nameless);
    }

    #[test]
    fn resolve_site_import_module_name() {
        let (_, index, stems) = resolve_fixture();
        let dep = |n: &str| {
            let site = DepSite {
                kind: SiteKind::Import(ImportArg::Name(s(n))),
                raw_offset: 3,
            };
            resolve_site(&site, "scripts/a.ps1", &index, &stems)
        };
        let unresolved = |n: &str, category| Dep::Unresolved {
            name: s(n),
            category,
        };
        assert_eq!(
            dep("foo"),
            Dep::File {
                path: s("mods/Foo/Foo.psd1"),
                kind: DepKind::ImportModule,
            }
        );
        assert_eq!(dep("Bar"), unresolved("Bar", "unknown"));
        // in-box, the name kept verbatim in any letter case
        for n in [
            "Microsoft.PowerShell.Utility",
            "microsoft.powershell.archive",
        ] {
            assert_eq!(dep(n), unresolved(n, "stdlib"), "{n}");
        }
        for n in [
            "Microsoft.PowerShell.SecretManagement",
            "PSReadLine",
            "CimCmdlets",
            "Helpers",
        ] {
            assert_eq!(dep(n), unresolved(n, "external"), "{n}");
        }
    }

    #[test]
    fn resolve_file_deps_dot_sources() {
        let dot = |a: PathArg| vec![SiteKind::DotSource(a)];
        let cases: Vec<(&str, Vec<SiteKind>)> = vec![
            (". ./lib.ps1", dot(st("./lib.ps1", false))),
            (". 'lib.ps1'", dot(st("lib.ps1", false))),
            (r". $PSScriptRoot\lib.ps1", dot(st("lib.ps1", true))),
            (". $path", dot(PathArg::Computed)),
            (". (Join-Path $PSScriptRoot x.ps1)", dot(PathArg::Computed)),
            ("function F { . ./f.ps1 }", dot(st("./f.ps1", false))),
            ("class C { M() { . ./m.ps1 } }", dot(st("./m.ps1", false))),
            (". { Get-Date }", vec![]),
            // the call operator is phase 6's
            ("& ./lib.ps1", vec![]),
        ];
        for (src, want) in cases {
            assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
            // a lone site starts at its `.` operator
            let at = src.find('.').unwrap();
            let want: Vec<DepSite> = want
                .into_iter()
                .map(|kind| DepSite {
                    kind,
                    raw_offset: at,
                })
                .collect();
            assert_eq!(deps_of(src.as_bytes()), want, "{src:?}");
        }
    }

    #[test]
    fn resolve_file_deps_import_module() {
        let name = |n: &str| Some(ImportArg::Name(s(n)));
        let path = |p: &str, anchored| Some(ImportArg::Path(st(p, anchored)));
        let computed = Some(ImportArg::Computed);
        let cases: Vec<(&str, Option<ImportArg>)> =
            vec![
            ("Import-Module Foo", name("Foo")),
            ("Import-Module -Name Foo", name("Foo")),
            ("import-module -name Foo", name("Foo")),
            ("ipmo Foo", name("Foo")),
            ("Microsoft.PowerShell.Core\\Import-Module Foo", name("Foo")),
            ("Import-Module -Force Foo", name("Foo")),
            ("Import-Module Foo -Global -PassThru", name("Foo")),
            ("Import-Module -Prefix P Foo", name("Foo")),
            ("Import-Module .\\Foo.psm1", path(".\\Foo.psm1", false)),
            ("Import-Module $PSScriptRoot\\Foo.psd1", path("Foo.psd1", true)),
            ("Import-Module lib/Foo", path("lib/Foo", false)),
            ("Import-Module Foo.dll", path("Foo.dll", false)),
            ("Import-Module $m", computed.clone()),
            ("Import-Module \"$x\\Foo.psm1\"", computed.clone()),
            // a comma list is one computed site, never its first name
            ("Import-Module A, B", computed.clone()),
            ("Import-Module -Name A, B", computed.clone()),
            ("Import-Module -Force", None),
            // common switches
            ("Import-Module -Verbose Foo", name("Foo")),
            ("Import-Module -Debug Foo", name("Foo")),
            ("Import-Module -WhatIf -Confirm Foo", name("Foo")),
            // colon-bound parameters consume nothing further
            ("Import-Module -Force:$true Foo", name("Foo")),
            ("Import-Module -Force:$false Foo", name("Foo")),
            ("Import-Module -Prefix:P Foo", name("Foo")),
            ("Import-Module -Name:Foo", name("Foo")),
            ("Import-Module -name:'./x.psm1'", path("./x.psm1", false)),
            ("Import-Module -Name:$m", computed.clone()),
            // -FullyQualifiedName
            ("Import-Module -FullyQualifiedName Foo", name("Foo")),
            (
                "Import-Module -FullyQualifiedName './Foo.psd1'",
                path("./Foo.psd1", false),
            ),
            (
                "Import-Module -FullyQualifiedName @{ModuleName='Foo'; ModuleVersion='1.0'}",
                computed.clone(),
            ),
            (
                "Import-Module -FullyQualifiedName:@{ModuleName='Foo'}",
                computed.clone(),
            ),
            ("Import-Module -ModuleInfo $mi", computed.clone()),
            (
                "Import-Module -Verbose -Force:$true -Prefix:P -Debug -Name:Foo -Global",
                name("Foo"),
            ),
            // abbreviations are not recognised: -Na consumes Foo
            ("Import-Module -Na Foo", None),
            ("Get-Module Foo", None),
            ("using module Foo", None),
            (
                "configuration C { Import-DscResource -ModuleName PSDesiredStateConfiguration }",
                None,
            ),
            // a value-taking parameter followed by a parameter binds nothing
            ("Import-Module -Prefix -Force X", name("X")),
            // a parameter given twice: the first bound wins
            ("Import-Module -Name A -Name B", name("A")),
            // -ModuleInfo and -Assembly share a rank: the first in source wins
            (
                "Import-Module -Assembly x.dll -ModuleInfo $mi",
                path("x.dll", false),
            ),
            ("Import-Module -ModuleInfo $mi -Assembly x.dll", computed.clone()),
        ];
        for (src, want) in cases {
            assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
            let want: Vec<DepSite> = want
                .into_iter()
                .map(|a| DepSite {
                    kind: SiteKind::Import(a),
                    raw_offset: 0,
                })
                .collect();
            assert_eq!(deps_of(src.as_bytes()), want, "{src:?}");
        }
    }

    #[test]
    fn resolve_file_deps_dsc_body_in_source_order() {
        let src = "configuration C { Node x { . ./lib.ps1; Import-Module Foo } }";
        assert_eq!(errors(src), Vec::<String>::new());
        let want = [
            DepSite {
                kind: SiteKind::DotSource(st("./lib.ps1", false)),
                raw_offset: src.find(". ./").unwrap(),
            },
            DepSite {
                kind: SiteKind::Import(ImportArg::Name(s("Foo"))),
                raw_offset: src.find("Import-Module").unwrap(),
            },
        ];
        assert_eq!(deps_of(src.as_bytes()), want);
    }

    #[test]
    fn resolve_file_deps_raw_offsets_across_encodings() {
        let text = "# é😀 comment\n. ./lib.ps1\nImport-Module Foo\n";
        let mut offsets = Vec::new();
        for (bytes, be, width) in [
            (text.as_bytes().to_vec(), None, 1),
            (utf8_bom(text), None, 1),
            (utf16_bytes(text, false), Some(false), 2),
            (utf16_bytes(text, true), Some(true), 2),
        ] {
            let deps = deps_of(&bytes);
            let got: Vec<usize> = deps.iter().map(|d| d.raw_offset).collect();
            assert_eq!(got.len(), 2, "{deps:?}");
            for (off, first) in got.iter().zip([".", "I"]) {
                assert_eq!(raw_text(&bytes, be, *off, off + width), first);
            }
            offsets.push(got);
        }
        // UTF-8, UTF-8 with BOM and UTF-16 differ; UTF-16LE and BE share a layout
        assert_eq!(offsets[0], [17, 29]);
        assert_eq!(offsets[1], [20, 32]);
        assert_eq!(offsets[2], [30, 54]);
        assert_eq!(offsets[3], offsets[2]);
    }

    /// requirements.requirement.ps-error-recovery: sites outside an ERROR region survive.
    #[test]
    fn resolve_file_deps_error_recovery() {
        let dot = |p: &str, at: usize| DepSite {
            kind: SiteKind::DotSource(st(p, false)),
            raw_offset: at,
        };
        // (A) a broken statement between two dot-sources and an Import-Module
        let src = ". ./a.ps1\n$x = )\n. ./b.ps1\nImport-Module Foo\n";
        assert_source_root(src);
        assert!(!errors(src).is_empty());
        let foo = DepSite {
            kind: SiteKind::Import(ImportArg::Name(s("Foo"))),
            raw_offset: 27,
        };
        assert_eq!(
            deps_of(src.as_bytes()),
            [dot("./a.ps1", 0), dot("./b.ps1", 17), foo]
        );

        // (B) both inside sites' command nodes lie in a local ERROR, the outside one not
        let broken =
            "function G { function I { . ./inside.ps1; Import-Module InsideMod } +=\n. ./out.ps1\n}\n";
        assert_source_root(broken);
        assert_eq!(
            site_ancestry(broken),
            [
                (". ./inside.ps1", true),
                ("Import-Module InsideMod", true),
                (". ./out.ps1", false),
            ]
        );
        let out = broken.find(". ./out").unwrap();
        assert_eq!(deps_of(broken.as_bytes()), [dot("./out.ps1", out)]);
        // without the stray `+=` every site is kept
        let clean = broken.replacen(" +=", "", 1);
        assert_eq!(errors(&clean), Vec::<String>::new());
        assert_eq!(deps_of(clean.as_bytes()).len(), 3);

        // (C) a whole-file ERROR root yields no site
        let open = ". ./a.ps1\nImport-Module Foo\nfunction F { Get-B\n$x = (1 + \n";
        assert_error_root(open);
        assert_eq!(deps_of(open.as_bytes()), []);
    }

    #[test]
    fn resolve_file_deps_feed_resolve_site() {
        let (_, index, stems) = resolve_fixture();
        let bytes = utf16_bytes(". ./b.ps1\n. $path\n", false);
        let deps = deps_of(&bytes);
        assert_eq!(deps.len(), 2, "{deps:?}");
        let got: Vec<Dep> = deps
            .iter()
            .map(|d| resolve_site(d, "scripts/a.ps1", &index, &stems))
            .collect();
        // `. $path` starts after the BOM and ten UTF-16 code units
        assert_eq!(deps[1].raw_offset, 22);
        assert_eq!(
            got,
            [
                Dep::File {
                    path: s("scripts/b.ps1"),
                    kind: DepKind::DotSource,
                },
                Dep::Unresolved {
                    name: s("<nameless@scripts/a.ps1:22>"),
                    category: "unknown",
                },
            ]
        );
    }

    #[test]
    fn finish_emits_file_dependency_records() {
        let e = emit_tree(&dep_tree(DEP_MAIN.into()));
        let recs = e.records();
        let main = script_id(recs, "/r/main.ps1");
        let nameless = |site: &str| {
            let at = DEP_MAIN.find(site).expect(site);
            format!("<nameless@main.ps1:{at}>")
        };
        let unresolved = |fqn: String, category: &str| {
            [
                Rec::Unresolved {
                    fqn: fqn.clone(),
                    category: s(category),
                },
                Rec::UnresolvedUse {
                    from: main.clone(),
                    to: fqn,
                },
            ]
        };
        // util.ps1's three dot-sources and PSReadLine's two imports each yield one edge;
        // `. { 1 }` yields nothing
        let mut want = vec![uses(&main, "/r/lib/util.ps1", "dot-source")];
        want.extend(unresolved(nameless(". $x"), "unknown"));
        want.push(uses(&main, "/r/mods/Foo/Foo.psm1", "import-module"));
        want.push(uses(&main, "/r/mods/Foo/Foo.psd1", "import-module"));
        for (name, category) in [
            ("Bar", "unknown"),
            ("Microsoft.PowerShell.Utility", "stdlib"),
            ("PSReadLine", "external"),
        ] {
            want.extend(unresolved(s(name), category));
        }
        // a .ps1 path is not importable
        want.extend(unresolved(nameless("Import-Module ./lib"), "unknown"));
        want.extend(unresolved(nameless("Import-Module $m"), "unknown"));
        assert_eq!(dep_recs(recs), want);
        let files: BTreeSet<&String> = recs
            .iter()
            .filter_map(|r| match r {
                Rec::File { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        let mut fqns = Vec::new();
        for r in recs {
            match r {
                Rec::Uses { to, .. } => assert!(files.contains(to), "{to}"),
                Rec::Unresolved { fqn, .. } => fqns.push(fqn),
                _ => {}
            }
        }
        let distinct: BTreeSet<&&String> = fqns.iter().collect();
        assert_eq!(distinct.len(), fqns.len(), "{fqns:?}");
        // the .psd1 sites yield nothing: no script Struct carries them
        assert!(
            files.contains(&s("/r/data/cfg.psd1")) && files.contains(&s("/r/mods/Foo/Foo.psd1"))
        );
        assert!(!recs
            .iter()
            .any(|r| matches!(r, Rec::Struct { path, .. } if path.ends_with(".psd1"))));
        assert!(!fqns.iter().any(|f| *f == "ManifestOnly" || *f == "CfgOnly"));
        assert_eq!(e.dot_sourced("main.ps1"), ["lib/util.ps1"]);
        assert!(e.dot_sourced("lib/util.ps1").is_empty());
    }

    #[test]
    fn finish_keys_nameless_dependencies_at_raw_utf16_offsets() {
        let nameless = |main: Vec<u8>| -> Vec<String> {
            emit_tree(&dep_tree(main))
                .records()
                .iter()
                .filter_map(|r| match r {
                    Rec::Unresolved { fqn, .. } if fqn.starts_with("<nameless@") => {
                        Some(fqn.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let sites = [". $x", "Import-Module ./lib", "Import-Module $m"];
        let key = |width: usize, bom: usize| -> Vec<String> {
            sites
                .iter()
                .map(|site| {
                    let units = DEP_MAIN[..DEP_MAIN.find(site).expect(site)]
                        .encode_utf16()
                        .count();
                    format!("<nameless@main.ps1:{}>", bom + width * units)
                })
                .collect()
        };
        let (narrow, wide) = (
            nameless(DEP_MAIN.into()),
            nameless(utf16_bytes(DEP_MAIN, false)),
        );
        assert_eq!(narrow, key(1, 0));
        // the BOM plus two bytes per code unit
        assert_eq!(wide, key(2, 2));
        for (w, n) in wide.iter().zip(&narrow) {
            assert_ne!(w, n);
        }
    }

    #[test]
    fn retain_targets_keeps_a_kept_scripts_dependency_records() {
        let tree = dep_tree(DEP_MAIN.into());
        let full = emit_tree(&tree);
        let mut main = emit_tree(&tree);
        main.retain_targets(&[s("/r/main.ps1")].into());
        assert_eq!(dep_recs(main.records()), dep_recs(full.records()));
        // a Uses survives with its source script, its target File dropped
        for to in [
            "/r/lib/util.ps1",
            "/r/mods/Foo/Foo.psm1",
            "/r/mods/Foo/Foo.psd1",
        ] {
            let kept = main.records();
            assert!(!kept
                .iter()
                .any(|r| matches!(r, Rec::File { path, .. } if path == to)));
            assert!(kept
                .iter()
                .any(|r| matches!(r, Rec::Uses { to: t, .. } if t == to)));
        }
        assert_eq!(main.dot_sourced("main.ps1"), ["lib/util.ps1"]);
        // util.ps1 alone: main's edges go, and with them every Unresolved
        let mut util = emit_tree(&tree);
        util.retain_targets(&[s("/r/lib/util.ps1")].into());
        assert!(full
            .records()
            .iter()
            .any(|r| matches!(r, Rec::Unresolved { .. })));
        assert_eq!(dep_recs(util.records()), []);
        assert_eq!(util.dot_sourced("main.ps1"), ["lib/util.ps1"]);
    }

    /// requirements.requirement.ps-file-dependency-kind-precedence
    #[test]
    fn finish_dot_source_wins_whatever_the_site_order_and_count() {
        let e = emit_tree(&PRECEDENCE_TREE);
        let recs = e.records();
        let scripts = [
            "dot_then_import.ps1",
            "import_then_dot.ps1",
            "two_imports.ps1",
        ];
        let mut want = Vec::new();
        for p in scripts {
            let id = script_id(recs, &format!("/r/{p}"));
            want.push(uses(&id, "/r/mods/Foo/Foo.psm1", "dot-source"));
            assert_eq!(e.dot_sourced(p), ["mods/Foo/Foo.psm1"], "{p}");
        }
        assert_eq!(dep_recs(recs), want);
    }

    #[test]
    fn units_scan_class_bases() {
        let cases: [(&str, &[&str]); 6] = [
            ("class C : B { }\n", &["B"]),
            ("class C : B, IFoo, IBar { }\n", &["B", "IFoo", "IBar"]),
            ("class C : B, b { }\n", &["B"]),
            (
                "class C : System.IDisposable { }\n",
                &["System.IDisposable"],
            ),
            (
                "class C : System.Collections.Generic.List[int] { }\n",
                &["System.Collections.Generic.List[int]"],
            ),
            ("class C { }\n", &[]),
        ];
        for (src, want) in cases {
            assert_eq!(errors(src), Vec::<String>::new(), "{src:?}");
            let fu = units_of(src.as_bytes());
            assert_eq!(fu.units[1].kind, UnitKind::Class, "{src:?}");
            assert_eq!(fu.units[1].bases, want, "{src:?}");
        }
        // an enum's underlying type is no base; every non-Class unit has none
        let src =
            "enum E : byte { A }\nfunction F { }\nclass K : B {\n    M() { }\n    K() { }\n}\n";
        assert_eq!(errors(src), Vec::<String>::new());
        let fu = units_of(src.as_bytes());
        for u in &fu.units {
            let want: &[&str] = if u.kind == UnitKind::Class {
                &["B"]
            } else {
                &[]
            };
            assert_eq!(u.bases, want, "{u:?}");
        }
        assert_eq!(fu.units.len(), 6);
    }

    #[test]
    fn units_scan_class_bases_around_a_local_error() {
        for (src, err) in [("class C : B, { }\n", ","), ("class C : B, ) { }\n", ", )")] {
            // the ERROR stays inside the class, beside the base list
            let tree = parse(src);
            let shape: Vec<(String, &str)> = descendants(tree.root_node())
                .into_iter()
                .filter(|n| n.is_error() || n.is_missing())
                .map(|n| {
                    (
                        format!("{:?}", &src[n.byte_range()]),
                        n.parent().map_or("", |p| p.kind()),
                    )
                })
                .collect();
            assert_eq!(shape, [(format!("{err:?}"), "class_definition")], "{src:?}");
            let fu = units_of(src.as_bytes());
            assert_eq!(fu.units[1].bases, ["B"], "{src:?}");
        }
    }

    /// requirements.requirement.ps-type-name-resolution
    #[test]
    fn resolve_type_over_scanned_fixtures() {
        let srcs = [
            (
                "a.ps1",
                "class Foo { }\nclass Bar : Foo {\n    [Foo]$p\n    [Color]$c\n}\n",
            ),
            ("b.ps1", "enum Color { Red }\nclass Foo { }\n"),
            ("c.ps1", "class Widget { }\nclass color2 { }\n"),
            ("d.ps1", "class Widget { }\n"),
            ("e.ps1", "Get-A\n"),
        ];
        let files: Vec<FileUnits> = srcs
            .iter()
            .map(|(p, src)| {
                assert_eq!(errors(src), Vec::<String>::new(), "{p}");
                units_at(p, src)
            })
            .collect();
        let bar = &files[0].units[2];
        assert_eq!((bar.name.as_str(), bar.kind), ("Bar", UnitKind::Class));
        assert_eq!(bar.property_types, ["Foo", "Color"]);
        assert_eq!(bar.bases, ["Foo"]);
        assert_eq!(files[1].units[1].kind, UnitKind::Enum);
        let index = TypeIndex::new(&files);
        let dot = dots(&[("a.ps1", &["b.ps1"])]);
        // step 1 beats the dot-sourced b.ps1's Foo
        assert_eq!(
            index.resolve_type(&bar.property_types[0], "a.ps1", &dot),
            one("a.ps1", 1)
        );
        // step 2: an enum is a property's target
        assert_eq!(
            index.resolve_type(&bar.property_types[1], "a.ps1", &dot),
            one("b.ps1", 1)
        );
        assert_eq!(
            index.resolve_type(&bar.bases[0], "a.ps1", &dot),
            one("a.ps1", 1)
        );
        // step 3 from a file with no dot-sources
        let widget = index.resolve_type("Widget", "e.ps1", &dot);
        assert_eq!(widget, TypeRes::Many);
        assert_eq!(
            type_target(widget, "Widget"),
            Target::Unresolved {
                name: s("Widget"),
                category: "unknown"
            }
        );
        // step 3 with one (color2 is no match): an enum is a base's target
        let color = index.resolve_type("Color", "e.ps1", &dot);
        assert_eq!(color, one("b.ps1", 1));
        assert_eq!(
            type_target(color, "Color"),
            Target::Unit {
                path: s("b.ps1"),
                index: 1
            }
        );
        // a static type literal's text
        assert_eq!(
            index.resolve_type("[Foo[]]", "a.ps1", &dot),
            one("a.ps1", 1)
        );
    }

    /// requirements.requirement.ps-system-type
    #[test]
    fn resolve_type_outside_the_scan() {
        let src = "class W : System.IDisposable, IComparable {\n    [System.String]$a\n    [string]$b\n    [hashtable]$c\n    [pscustomobject]$d\n    [System.IO.FileInfo]$e\n    [ciminstance]$f\n    [math]$g\n    [object]$h\n    [Contoso.Widget]$i\n}\n";
        assert_eq!(errors(src), Vec::<String>::new());
        let files = [units_at("w.ps1", src)];
        let w = &files[0].units[1];
        let written: Vec<&String> = w.bases.iter().chain(&w.property_types).collect();
        assert_eq!(
            written,
            [
                "System.IDisposable",
                "IComparable",
                "System.String",
                "string",
                "hashtable",
                "pscustomobject",
                "System.IO.FileInfo",
                "ciminstance",
                "math",
                "object",
                "Contoso.Widget",
            ]
        );
        let index = TypeIndex::new(&files);
        let dot = DotSourceMap::new();
        for name in written {
            let res = index.resolve_type(name, "w.ps1", &dot);
            assert_eq!(res, TypeRes::Outside, "{name}");
            let stdlib = crate::resolve::is_system_type(&type_name(name));
            let category = if stdlib { "stdlib" } else { "external" };
            let want = [
                "System.IDisposable",
                "System.String",
                "string",
                "hashtable",
                "pscustomobject",
                "System.IO.FileInfo",
            ]
            .contains(&name.as_str());
            assert_eq!(stdlib, want, "{name}");
            assert_eq!(
                type_target(res, name),
                Target::Unresolved {
                    name: name.clone(),
                    category
                },
                "{name}"
            );
        }
    }

    /// requirements.requirement.ps-generic-type-name
    #[test]
    fn resolve_type_name_strips_brackets_arrays_and_generics() {
        for (written, want) in [
            ("[Foo]", "Foo"),
            ("Foo[]", "Foo"),
            ("[Foo[][]]", "Foo"),
            ("[Foo[]]", "Foo"),
            (" Foo ", "Foo"),
            ("[string[]]", "string"),
            (
                "[System.Collections.Generic.List[string]]",
                "System.Collections.Generic.List",
            ),
            (
                "System.Collections.Generic.List[int]",
                "System.Collections.Generic.List",
            ),
            (
                "System.Collections.Generic.List[int][]",
                "System.Collections.Generic.List",
            ),
            (
                "System.Collections.Generic.List[System.Collections.Generic.List[int]]",
                "System.Collections.Generic.List",
            ),
            (
                "System.Collections.Generic.Dictionary[string,int]",
                "System.Collections.Generic.Dictionary",
            ),
            ("List[int][]", "List"),
            ("List[List[int]]", "List"),
            ("Dictionary[string,int]", "Dictionary"),
        ] {
            assert_eq!(type_name(written), want, "{written:?}");
        }
    }

    /// requirements.requirement.ps-type-name-resolution
    #[test]
    fn resolve_type_step_1_own_file() {
        use UnitKind::*;
        let index = TypeIndex::new(&[type_file("a.ps1", &[(Function, "F"), (Class, "Foo")])]);
        let dot = DotSourceMap::new();
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), one("a.ps1", 2));
    }

    #[test]
    fn resolve_type_is_case_insensitive() {
        let index = TypeIndex::new(&[type_file("a.ps1", &[(UnitKind::Class, "Foo")])]);
        let dot = DotSourceMap::new();
        for name in ["foo", "FOO"] {
            assert_eq!(
                index.resolve_type(name, "a.ps1", &dot),
                one("a.ps1", 1),
                "{name}"
            );
        }
    }

    #[test]
    fn resolve_type_enum_candidate() {
        let index = TypeIndex::new(&[type_file("a.ps1", &[(UnitKind::Enum, "Color")])]);
        let dot = DotSourceMap::new();
        assert_eq!(index.resolve_type("Color", "a.ps1", &dot), one("a.ps1", 1));
    }

    #[test]
    fn resolve_type_step_2_dot_sourced() {
        let index = TypeIndex::new(&[
            type_file("a.ps1", &[]),
            type_file("b.ps1", &[(UnitKind::Class, "Foo")]),
        ]);
        let dot = dots(&[("a.ps1", &["b.ps1"])]);
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), one("b.ps1", 1));
    }

    #[test]
    fn resolve_type_step_3_any_file() {
        let index = TypeIndex::new(&[
            type_file("a.ps1", &[]),
            type_file("c.ps1", &[(UnitKind::Class, "Foo")]),
        ]);
        let dot = DotSourceMap::new();
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), one("c.ps1", 1));
    }

    #[test]
    fn resolve_type_step_2_beats_several_at_step_3() {
        let foo = [(UnitKind::Class, "Foo")];
        let index = TypeIndex::new(&[
            type_file("a.ps1", &[]),
            type_file("b.ps1", &foo),
            type_file("c.ps1", &foo),
            type_file("f.ps1", &foo),
        ]);
        let dot = dots(&[("a.ps1", &["b.ps1"])]);
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), one("b.ps1", 1));
    }

    /// requirements.requirement.ps-ambiguous-type
    #[test]
    fn resolve_type_several_at_step_2_is_many() {
        let foo = [(UnitKind::Class, "Foo")];
        let index = TypeIndex::new(&[
            type_file("a.ps1", &[]),
            type_file("b.ps1", &foo),
            type_file("d.ps1", &foo),
        ]);
        let dot = dots(&[("a.ps1", &["b.ps1", "d.ps1"])]);
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), TypeRes::Many);
    }

    /// requirements.requirement.ps-ambiguous-type
    #[test]
    fn resolve_type_several_at_step_3_is_many() {
        let index = TypeIndex::new(&[
            type_file("a.ps1", &[]),
            type_file("c.ps1", &[(UnitKind::Class, "Foo")]),
            type_file("e.ps1", &[(UnitKind::Enum, "foo")]),
        ]);
        let dot = DotSourceMap::new();
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), TypeRes::Many);
    }

    #[test]
    fn resolve_type_file_dot_sourced_twice_is_one() {
        let index = TypeIndex::new(&[
            type_file("a.ps1", &[]),
            type_file("b.ps1", &[(UnitKind::Class, "Foo")]),
        ]);
        let dot = dots(&[("a.ps1", &["b.ps1", "b.ps1"])]);
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), one("b.ps1", 1));
    }

    #[test]
    fn resolve_type_scanned_class_beats_accelerator() {
        let index = TypeIndex::new(&[type_file("a.ps1", &[(UnitKind::Class, "string")])]);
        let dot = DotSourceMap::new();
        assert_eq!(index.resolve_type("string", "a.ps1", &dot), one("a.ps1", 1));
    }

    /// requirements.requirement.ps-generic-type-name
    #[test]
    fn resolve_type_generic_reference_finds_its_base() {
        let index = TypeIndex::new(&[type_file("a.ps1", &[(UnitKind::Class, "Foo")])]);
        let dot = DotSourceMap::new();
        assert_eq!(
            index.resolve_type("Foo[int]", "a.ps1", &dot),
            one("a.ps1", 1)
        );
    }

    #[test]
    fn resolve_type_outside() {
        let index = TypeIndex::new(&[type_file("a.ps1", &[(UnitKind::Class, "Bar")])]);
        let dot = DotSourceMap::new();
        assert_eq!(index.resolve_type("Foo", "a.ps1", &dot), TypeRes::Outside);
    }
}
