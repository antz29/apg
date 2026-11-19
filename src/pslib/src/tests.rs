//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules, with shared fixtures and helpers at this root.

use crate::diag::{error_regions, error_summary, write_diagnostic, Diagnostic};
use crate::manifest::{assign, read_manifest, wildcard_match, ExportSpec, ManifestKeys, ModuleMap};
use crate::parse::parse;
use crate::paths::{normalise, path_arg, resolve, FileIndex, PathArg};
use crate::scan::{is_ps_source, parse_args, Emitter, Rec};
use crate::source::{decode, decode_mapped, line_count, position, span};
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
}
