//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules, with shared fixtures and helpers at this root.

use crate::diag::{error_summary, write_diagnostic, Diagnostic};
use crate::parse::parse;
use crate::source::{decode, decode_mapped, line_count, position, span};
use tree_sitter::Node;

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
}
