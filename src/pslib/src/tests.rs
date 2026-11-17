//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules, with shared fixtures and helpers at this root.

use crate::parse::parse;
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
    ABBREVIATIONS
        .iter()
        .fold(out, |out, (chain, short)| out.replace(chain, short))
}

/// The grammar's single-child precedence chains, abbreviated in `dump` output
/// in this order (exact substrings, so every kind stays pinned).
const ABBREVIATIONS: &[(&str, &str)] = &[
    (
        "logical_expression>bitwise_expression>comparison_expression>additive_expression>multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression",
        "<expr>",
    ),
    (
        "logical_expression>bitwise_expression>comparison_expression>additive_expression>multiplicative_expression>format_expression>range_expression>array_literal_expression",
        "<list>",
    ),
    (
        "argument_expression>logical_argument_expression>bitwise_argument_expression>comparison_argument_expression>additive_argument_expression>multiplicative_argument_expression>format_argument_expression>range_argument_expression>unary_expression",
        "<arg>",
    ),
];

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
                "program>statement_list(pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>path_command_name(variable\"$PSScriptRoot\" path_command_name_token\"\\\\lib.ps1\")) pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>command_name\"./other.ps1\"))",
            ),
            (
                USING_MODULE,
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"using\" command_elements:command_elements(command_argument_sep>' ' generic_token\"module\" command_argument_sep>' ' generic_token\"./Foo.psm1\"))",
            ),
            (
                DSC_CONFIG,
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"configuration\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Web\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Node\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'localhost'\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"File\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Site\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"DestinationPath\" command_elements:command_elements(command_argument_sep>' ' generic_token\"=\" command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'C:\\\\site'\"))))))))))))))",
            ),
            (
                MANIFEST,
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"RootModule\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Foo.psm1'\") hash_entry(key_expression>simple_name\"NestedModules\" '=' pipeline>pipeline_chain><expr>>array_expression('@(' statements:statement_list>pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'lib\\\\Helpers.ps1'\")) hash_entry(key_expression>simple_name\"FunctionsToExport\" '=' pipeline>pipeline_chain><expr>>array_expression('@(' statements:statement_list>pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Get-Foo'\"))))",
            ),
        ]);
    }

    #[test]
    fn a_script_root() {
        check(&[(
            "param($a, [int]$b)\nGet-A\n$x = 1\n",
            "program(param_block('param' parameter_list(script_parameter>variable\"$a\" ',' script_parameter(attribute_list>attribute>type_literal>type_spec>type_name>type_identifier\"int\" variable\"$b\"))) statement_list(pipeline>pipeline_chain>command(command_name:command_name\"Get-A\") pipeline>assignment_expression(left_assignment_expression><expr>>variable\"$x\" assignement_operator>'=' value:pipeline>pipeline_chain><expr>>integer_literal>decimal_integer_literal\"1\")))",
        )]);
    }

    #[test]
    fn b_function_filter_workflow() {
        check(&[
            (
                "function F($a) { Get-A }",
                "program>statement_list>function_statement('function' function_name\"F\" function_parameter_declaration>parameter_list>script_parameter>variable\"$a\" script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-A\"))))",
            ),
            (
                "function F { param($a) Get-A }",
                "program>statement_list>function_statement('function' function_name\"F\" script_block(param_block('param' parameter_list>script_parameter>variable\"$a\") ERROR(simple_name\" Get\" '-' simple_name\"A\")))",
            ),
            (
                "function F {\n  param($a)\n  Get-A\n}",
                "program>statement_list>function_statement('function' function_name\"F\" script_block(param_block('param' parameter_list>script_parameter>variable\"$a\") script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-A\"))))",
            ),
            (
                "filter Fl { $_ }",
                "program>statement_list>function_statement('filter' function_name\"Fl\" script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain><expr>>variable>'$_')))",
            ),
            (
                "workflow W { param($a) Get-A }",
                "program>statement_list>function_statement('workflow' function_name\"W\" script_block(param_block('param' parameter_list>script_parameter>variable\"$a\") ERROR(simple_name\" Get\" '-' simple_name\"A\")))",
            ),
            (
                "workflow W {\n  param($a)\n  Get-A\n}",
                "program>statement_list>function_statement('workflow' function_name\"W\" script_block(param_block('param' parameter_list>script_parameter>variable\"$a\") script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-A\"))))",
            ),
            (
                "function Outer { function Inner { } Inner }",
                "program>statement_list>function_statement('function' function_name\"Outer\" script_block(script_block_body:script_block_body(statement_list:statement_list(function_statement('function' function_name\"Inner\") pipeline>pipeline_chain>command(command_name:command_name\"Inner\")))))",
            ),
        ]);
    }

    #[test]
    fn c_scope_prefixed_names() {
        check(&[
            (
                "function global:Foo { }",
                "program>statement_list>function_statement('function' function_name\"global:Foo\")",
            ),
            (
                "function script:Foo { }",
                "program>statement_list>function_statement('function' function_name\"script:Foo\")",
            ),
            (
                "function local:Foo { }",
                "program>statement_list>function_statement('function' function_name\"local:Foo\")",
            ),
            (
                "function private:Foo { }",
                "program>statement_list>function_statement('function' function_name\"private:Foo\")",
            ),
            (
                "script:Foo -X 1",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"script:Foo\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-X\" command_argument_sep>' ' array_literal_expression>unary_expression>integer_literal>decimal_integer_literal\"1\"))",
            ),
        ]);
    }

    #[test]
    fn d_class_and_enum() {
        check(&[
            (
                "class C : B, I {\n  [Foo]$P\n  $U\n  C([int]$x, $y) { }\n  [void] M([int]$x, $y) { }\n  static [int] S() { return 1 }\n}",
                "program>statement_list>class_statement(simple_name\"C\" ':' simple_name\"B\" ',' simple_name\"I\" class_property_definition(type_literal>type_spec>type_name>type_identifier\"Foo\" variable\"$P\") class_property_definition>variable\"$U\" class_method_definition(simple_name\"C\" class_method_parameter_list(class_method_parameter(type_literal>type_spec>type_name>type_identifier\"int\" variable\"$x\") ',' class_method_parameter>variable\"$y\")) class_method_definition(type_literal>type_spec>type_name>type_identifier\"void\" simple_name\"M\" class_method_parameter_list(class_method_parameter(type_literal>type_spec>type_name>type_identifier\"int\" variable\"$x\") ',' class_method_parameter>variable\"$y\")) class_method_definition(class_attribute\"static\" type_literal>type_spec>type_name>type_identifier\"int\" simple_name\"S\" script_block(script_block_body:script_block_body(statement_list:statement_list>flow_control_statement('return' pipeline>pipeline_chain><expr>>integer_literal>decimal_integer_literal\"1\")))))",
            ),
            (
                "enum E { A\n B = 2 }",
                "program>statement_list>enum_statement(simple_name\"E\" enum_member>simple_name\"A\" enum_member(simple_name\"B\" '=' integer_literal>decimal_integer_literal\"2\"))",
            ),
        ]);
    }

    #[test]
    fn e_static_and_instance_invocation() {
        check(&[
            (
                "[T]::M($a)",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(type_literal>type_spec>type_name>type_identifier\"T\" '::' member_name>simple_name\"M\" argument_list(argument_expression_list:argument_expression_list><arg>>variable\"$a\"))",
            ),
            (
                "[T]::new($a)",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(type_literal>type_spec>type_name>type_identifier\"T\" '::' member_name>simple_name\"new\" argument_list(argument_expression_list:argument_expression_list><arg>>variable\"$a\"))",
            ),
            (
                "$this.M($a, $b)",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(variable\"$this\" '.' member_name>simple_name\"M\" argument_list(argument_expression_list:argument_expression_list(<arg>>variable\"$a\" ',' <arg>>variable\"$b\")))",
            ),
        ]);
    }

    #[test]
    fn f_call_and_dot_operands() {
        check(&[
            (
                "& { Get-A }",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-A\"))))",
            ),
            (
                "& $sb",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>path_command_name>variable\"$sb\")",
            ),
            (
                "& ('a' + 'b')",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>parenthesized_expression>pipeline>pipeline_chain>logical_expression>bitwise_expression>comparison_expression>additive_expression(additive_expression>multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'a'\" '+' multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'b'\"))",
            ),
            (
                "& 'C:\\x.ps1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>string_literal>verbatim_string_characters\"'C:\\\\x.ps1'\")",
            ),
            (
                ". { Get-A }",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-A\"))))",
            ),
            (
                ". $sb",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>path_command_name>variable\"$sb\")",
            ),
            (
                ". ('a' + 'b')",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>parenthesized_expression>pipeline>pipeline_chain>logical_expression>bitwise_expression>comparison_expression>additive_expression(additive_expression>multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'a'\" '+' multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'b'\"))",
            ),
            (
                ". 'lib.ps1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>verbatim_string_characters\"'lib.ps1'\")",
            ),
        ]);
    }

    #[test]
    fn g_path_literals() {
        check(&[
            (
                ". ./lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>command_name\"./lib.ps1\")",
            ),
            (
                ". './lib.ps1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>verbatim_string_characters\"'./lib.ps1'\")",
            ),
            (
                ". \"./lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>expandable_string_literal\"\\\"./lib.ps1\\\"\")",
            ),
            (
                ". $PSScriptRoot\\lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>path_command_name(variable\"$PSScriptRoot\" path_command_name_token\"\\\\lib.ps1\"))",
            ),
            (
                ". \"$PSScriptRoot/lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>expandable_string_literal>variable\"$PSScriptRoot\")",
            ),
            (
                "Import-Module ./lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"./lib.ps1\"))",
            ),
            (
                "Import-Module './lib.ps1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'./lib.ps1'\"))",
            ),
            (
                "Import-Module \"./lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>expandable_string_literal\"\\\"./lib.ps1\\\"\"))",
            ),
            (
                "Import-Module $PSScriptRoot\\lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>member_access(variable\"$PSScriptRoot\" ERROR('\\' simple_name\"lib\") '.' member_name>simple_name\"ps1\")))",
            ),
            (
                "Import-Module \"$PSScriptRoot/lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>expandable_string_literal>variable\"$PSScriptRoot\"))",
            ),
            (
                "Import-Module -Name ./lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>' ' generic_token\"./lib.ps1\"))",
            ),
            (
                "Import-Module -Name './lib.ps1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'./lib.ps1'\"))",
            ),
            (
                "Import-Module -Name \"./lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>expandable_string_literal\"\\\"./lib.ps1\\\"\"))",
            ),
            (
                "Import-Module -Name $PSScriptRoot\\lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>' ' array_literal_expression>unary_expression>member_access(variable\"$PSScriptRoot\" ERROR('\\' simple_name\"lib\") '.' member_name>simple_name\"ps1\")))",
            ),
            (
                "Import-Module -Name \"$PSScriptRoot/lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>expandable_string_literal>variable\"$PSScriptRoot\"))",
            ),
        ]);
    }

    #[test]
    fn h_commands_using_dsc_hashtables() {
        check(&[
            (
                "Get-Foo -Name x 'y' $z",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-Foo\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>' ' generic_token\"x\" command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'y'\" command_argument_sep>' ' array_literal_expression>unary_expression>variable\"$z\"))",
            ),
            (
                "using module Foo",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"using\" command_elements:command_elements(command_argument_sep>' ' generic_token\"module\" command_argument_sep>' ' generic_token\"Foo\"))",
            ),
            (
                "using module 'Foo.psm1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"using\" command_elements:command_elements(command_argument_sep>' ' generic_token\"module\" command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'Foo.psm1'\"))",
            ),
            (
                "configuration C { }",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"configuration\" command_elements:command_elements(command_argument_sep>' ' generic_token\"C\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>empty_statement>MISSING ';'))))",
            ),
            (
                "@{ A = 1; 'B' = 'x'; C = @('y') }",
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"A\" '=' pipeline>pipeline_chain><expr>>integer_literal>decimal_integer_literal\"1\") hash_entry(key_expression>unary_expression>string_literal>verbatim_string_characters\"'B'\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'x'\") hash_entry(key_expression>simple_name\"C\" '=' pipeline>pipeline_chain><expr>>array_expression('@(' statements:statement_list>pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'y'\"))))",
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
                vec![
                    "ERROR \"function F { Get-A\\n$x = (1 + \\n\"",
                    "ERROR \"\\n\""
                ],
                vec!["MISSING }"],
                vec!["ERROR \"(1 + 2\""],
                vec!["ERROR \"$x = @(1\""],
                vec!["MISSING }"],
                vec!["MISSING }"],
                vec!["ERROR \"$x = \""],
            ]
        );
        let dumps = [
            "ERROR('function' function_name\"F\" pipeline>pipeline_chain>command(command_name:command_name\"Get-A\") left_assignment_expression><expr>>variable\"$x\" assignement_operator>'=' additive_expression>multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>integer_literal>decimal_integer_literal\"1\" '+' ERROR\"\\n\")",
            "program>statement_list>function_statement('function' function_name\"F\" MISSING '}')",
            "ERROR>bitwise_expression>comparison_expression>additive_expression(additive_expression>multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>integer_literal>decimal_integer_literal\"1\" '+' multiplicative_expression>format_expression>range_expression>array_literal_expression>unary_expression>integer_literal>decimal_integer_literal\"2\")",
            "ERROR(left_assignment_expression><expr>>variable\"$x\" assignement_operator>'=' '@(' statement_list>pipeline>pipeline_chain><expr>>integer_literal>decimal_integer_literal\"1\")",
            "program>statement_list>if_statement('if' condition:pipeline>pipeline_chain><expr>>variable\"$x\" statement_block>MISSING '}')",
            "program>statement_list>class_statement(simple_name\"C\" MISSING '}')",
            "ERROR(left_assignment_expression><expr>>variable\"$x\" assignement_operator>'=')",
        ];
        check(&srcs.into_iter().zip(dumps).collect::<Vec<_>>());
    }

    #[test]
    fn j_colon_bound_parameters() {
        check(&[
            (
                "Import-Module Foo -Force:$true",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\" command_argument_sep>' ' command_parameter\"-Force\" command_argument_sep>':' array_literal_expression>unary_expression>variable\"$true\"))",
            ),
            (
                "Import-Module Foo -Force:$false",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\" command_argument_sep>' ' command_parameter\"-Force\" command_argument_sep>':' array_literal_expression>unary_expression>variable\"$false\"))",
            ),
            (
                "Import-Module Foo -Prefix:P",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\" command_argument_sep>' ' command_parameter\"-Prefix\" command_argument_sep>':' generic_token\"P\"))",
            ),
            (
                "Import-Module -Name:Foo",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>':' generic_token\"Foo\"))",
            ),
            (
                "Import-Module -name:'./x.psm1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-name\" command_argument_sep>':' array_literal_expression>unary_expression>string_literal>verbatim_string_characters\"'./x.psm1'\"))",
            ),
            (
                "Import-Module -Name:$m",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Name\" command_argument_sep>':' array_literal_expression>unary_expression>variable\"$m\"))",
            ),
        ]);
    }

    #[test]
    fn k_hashtable_arguments() {
        check(&[
            (
                "Import-Module -FullyQualifiedName @{ModuleName='Foo'; ModuleVersion='1.0'}",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-FullyQualifiedName\" command_argument_sep>' ' array_literal_expression>unary_expression>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"ModuleName\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Foo'\") hash_entry(key_expression>simple_name\"ModuleVersion\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'1.0'\")))))",
            ),
            (
                "Import-Module -FullyQualifiedName:@{ModuleName='Foo'}",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-FullyQualifiedName\" command_argument_sep>':' array_literal_expression>unary_expression>hash_literal_expression('@{' hash_literal_body>hash_entry(key_expression>simple_name\"ModuleName\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Foo'\"))))",
            ),
        ]);
    }

    #[test]
    fn l_import_module_variants() {
        check(&[
            (
                "ipmo Foo",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"ipmo\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\"))",
            ),
            (
                "import-module Foo",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"import-module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\"))",
            ),
            (
                "Microsoft.PowerShell.Core\\Import-Module Foo",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Microsoft.PowerShell.Core\\\\Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\"))",
            ),
            (
                "Import-Module Foo -Force -Global -PassThru -Verbose -Debug -WhatIf -Confirm -Na Bar",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\" command_argument_sep>' ' command_parameter\"-Force\" command_argument_sep>' ' command_parameter\"-Global\" command_argument_sep>' ' command_parameter\"-PassThru\" command_argument_sep>' ' command_parameter\"-Verbose\" command_argument_sep>' ' command_parameter\"-Debug\" command_argument_sep>' ' command_parameter\"-WhatIf\" command_argument_sep>' ' command_parameter\"-Confirm\" command_argument_sep>' ' command_parameter\"-Na\" command_argument_sep>' ' generic_token\"Bar\"))",
            ),
            (
                "Import-Module Foo -Prefix P",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"Foo\" command_argument_sep>' ' command_parameter\"-Prefix\" command_argument_sep>' ' generic_token\"P\"))",
            ),
            (
                "Import-Module -ModuleInfo $mi",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-ModuleInfo\" command_argument_sep>' ' array_literal_expression>unary_expression>variable\"$mi\"))",
            ),
            (
                "Import-Module A, B",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"A\" ',' generic_token\"B\"))",
            ),
            (
                "Import-Module $m",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>variable\"$m\"))",
            ),
            (
                "Import-Module \"$x\\Foo.psm1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>string_literal>expandable_string_literal>variable\"$x\"))",
            ),
            (
                "Import-Module (Join-Path $PSScriptRoot x.ps1)",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>parenthesized_expression>pipeline>pipeline_chain>command(command_name:command_name\"Join-Path\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>variable\"$PSScriptRoot\" command_argument_sep>' ' generic_token\"x.ps1\"))))",
            ),
            (
                "Import-Module -Force",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-Force\"))",
            ),
        ]);
    }

    #[test]
    fn m_expandable_string_operands() {
        check(&[
            (
                ". \"$psscriptroot\\lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>expandable_string_literal>variable\"$psscriptroot\")",
            ),
            (
                ". \"$env:X\\lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>expandable_string_literal>variable\"$env:X\")",
            ),
            (
                ". \"$PSScriptRoot$x.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>expandable_string_literal(variable\"$PSScriptRoot\" variable\"$x\"))",
            ),
            (
                ". \"$($x)\\lib.ps1\"",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>expandable_string_literal>sub_expression('$(' statements:statement_list>pipeline>pipeline_chain><expr>>variable\"$x\"))",
            ),
            (
                ". '$PSScriptRoot\\lib.ps1'",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>string_literal>verbatim_string_characters\"'$PSScriptRoot\\\\lib.ps1'\")",
            ),
        ]);
    }

    #[test]
    fn n_manifest_hashtables() {
        check(&[
            (
                "# manifest\n@{ RootModule = 'Foo.psm1' }\n",
                "program(comment\"# manifest\" statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body>hash_entry(key_expression>simple_name\"RootModule\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Foo.psm1'\")))",
            ),
            (
                "@{ 'RootModule' = 'Foo.psm1'; rootmodule = 'x' }",
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>unary_expression>string_literal>verbatim_string_characters\"'RootModule'\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Foo.psm1'\") hash_entry(key_expression>simple_name\"rootmodule\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'x'\")))",
            ),
            (
                "@{ NestedModules = 'a.ps1'; FunctionsToExport = 'Get-A' }",
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"NestedModules\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'a.ps1'\") hash_entry(key_expression>simple_name\"FunctionsToExport\" '=' pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Get-A'\")))",
            ),
            (
                "@{ NestedModules = @('a.ps1', 'b.ps1'); FunctionsToExport = @('Get-A') }",
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"NestedModules\" '=' pipeline>pipeline_chain><expr>>array_expression('@(' statements:statement_list>pipeline>pipeline_chain><list>(unary_expression>string_literal>verbatim_string_characters\"'a.ps1'\" ',' unary_expression>string_literal>verbatim_string_characters\"'b.ps1'\"))) hash_entry(key_expression>simple_name\"FunctionsToExport\" '=' pipeline>pipeline_chain><expr>>array_expression('@(' statements:statement_list>pipeline>pipeline_chain><expr>>string_literal>verbatim_string_characters\"'Get-A'\"))))",
            ),
            (
                "@{ NestedModules = 'a.ps1', 'b.ps1'; FunctionsToExport = 'A', 'B' }",
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"NestedModules\" '=' pipeline>pipeline_chain><list>(unary_expression>string_literal>verbatim_string_characters\"'a.ps1'\" ',' unary_expression>string_literal>verbatim_string_characters\"'b.ps1'\")) hash_entry(key_expression>simple_name\"FunctionsToExport\" '=' pipeline>pipeline_chain><list>(unary_expression>string_literal>verbatim_string_characters\"'A'\" ',' unary_expression>string_literal>verbatim_string_characters\"'B'\"))))",
            ),
            (
                "@{ NestedModules = @(); FunctionsToExport = @() }",
                "program>statement_list>pipeline>pipeline_chain><expr>>hash_literal_expression('@{' hash_literal_body(hash_entry(key_expression>simple_name\"NestedModules\" '=' pipeline>pipeline_chain><expr>>array_expression>'@(') hash_entry(key_expression>simple_name\"FunctionsToExport\" '=' pipeline>pipeline_chain><expr>>array_expression>'@(')))",
            ),
        ]);
    }

    #[test]
    fn o_name_and_type_variants() {
        check(&[
            (
                "function C.M { }",
                "program>statement_list>function_statement('function' function_name\"C.M\")",
            ),
            (
                "class C { [D[]]$Q }",
                "program>statement_list>class_statement(simple_name\"C\" class_property_definition(type_literal>type_spec>array_type_name>type_name>type_identifier\"D\" variable\"$Q\"))",
            ),
            (
                "class C { M([System.Collections.Generic.List[string]]$l) { } }",
                "program>statement_list>class_statement(simple_name\"C\" class_method_definition(simple_name\"M\" class_method_parameter_list>class_method_parameter(type_literal>type_spec(generic_type_name>type_name(type_name(type_name(type_name>type_identifier\"System\" '.' type_identifier\"Collections\") '.' type_identifier\"Generic\") '.' type_identifier\"List\") generic_type_arguments>type_spec>type_name>type_identifier\"string\") variable\"$l\")))",
            ),
        ]);
    }

    #[test]
    fn p_nested_sites() {
        check(&[
            (
                "function F { . ./a.ps1; Import-Module B }",
                "program>statement_list>function_statement('function' function_name\"F\" script_block(script_block_body:script_block_body(statement_list:statement_list(pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>command_name\"./a.ps1\") empty_statement\";\" pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"B\"))))))",
            ),
            (
                "class C { M() { . ./a.ps1; Import-Module B } }",
                "program>statement_list>class_statement(simple_name\"C\" class_method_definition(simple_name\"M\" script_block(script_block_body:script_block_body(statement_list:statement_list(pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>command_name\"./a.ps1\") empty_statement\";\" pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"B\")))))))",
            ),
            (
                "$s = { . ./a.ps1; Import-Module B }",
                "program>statement_list>pipeline>assignment_expression(left_assignment_expression><expr>>variable\"$s\" assignement_operator>'=' value:pipeline>pipeline_chain><expr>>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list(pipeline>pipeline_chain>command(command_invokation_operator>'.' command_name:command_name_expr>command_name\"./a.ps1\") empty_statement\";\" pipeline>pipeline_chain>command(command_name:command_name\"Import-Module\" command_elements:command_elements(command_argument_sep>' ' generic_token\"B\"))))))",
            ),
            (
                "configuration C { Import-DscResource -ModuleName PSDesiredStateConfiguration }",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"configuration\" command_elements:command_elements(command_argument_sep>' ' generic_token\"C\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Import-DscResource\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-ModuleName\" command_argument_sep>' ' generic_token\"PSDesiredStateConfiguration\"))))))",
            ),
        ]);
    }

    #[test]
    fn q_command_name_variants() {
        check(&[
            (
                "% { $_ }",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"%\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain><expr>>variable>'$_'))))",
            ),
            (
                "? { $_ }",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"?\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain><expr>>variable>'$_'))))",
            ),
            (
                "ls",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"ls\")",
            ),
            (
                "Microsoft.PowerShell.Utility\\Write-Output x",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Microsoft.PowerShell.Utility\\\\Write-Output\" command_elements:command_elements(command_argument_sep>' ' generic_token\"x\"))",
            ),
            (
                "./x.ps1 a",
                "ERROR(command_invokation_operator>'.' '/' simple_name\"x\" '.' simple_name\"ps1\" simple_name\" a\")",
            ),
            (
                "git.exe status",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"git.exe\" command_elements:command_elements(command_argument_sep>' ' generic_token\"status\"))",
            ),
        ]);
    }

    #[test]
    fn r_call_operand_variants() {
        check(&[
            (
                "& Get-Foo -X 1",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>path_command_name>path_command_name_token\"Get-Foo\" command_elements:command_elements(command_argument_sep>' ' command_parameter\"-X\" command_argument_sep>' ' array_literal_expression>unary_expression>integer_literal>decimal_integer_literal\"1\"))",
            ),
            (
                "& $PSScriptRoot\\lib.ps1",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>path_command_name(variable\"$PSScriptRoot\" path_command_name_token\"\\\\lib.ps1\"))",
            ),
            (
                "& $function:Foo",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>path_command_name>variable\"$function:Foo\")",
            ),
            (
                "& (Get-X)",
                "program>statement_list>pipeline>pipeline_chain>command(command_invokation_operator>'&' command_name:command_name_expr>parenthesized_expression>pipeline>pipeline_chain>command(command_name:command_name\"Get-X\"))",
            ),
        ]);
    }

    #[test]
    fn s_pipelines_and_nested_commands() {
        check(&[
            (
                "Get-A | Get-B | Get-C",
                "program>statement_list>pipeline>pipeline_chain(command(command_name:command_name\"Get-A\" command_elements:command_elements>command_argument_sep>' ') '|' command(command_name:command_name\"Get-B\" command_elements:command_elements>command_argument_sep>' ') '|' command(command_name:command_name\"Get-C\"))",
            ),
            (
                "Get-A { Get-B }",
                "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-A\" command_elements:command_elements(command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Get-B\")))))",
            ),
        ]);
    }

    #[test]
    fn t_this_invocation_variants() {
        check(&[
            (
                "$this.M()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(variable\"$this\" '.' member_name>simple_name\"M\" argument_list\"()\")",
            ),
            (
                "$this.M(@(1, 2), [Foo]::N(3, 4))",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(variable\"$this\" '.' member_name>simple_name\"M\" argument_list(argument_expression_list:argument_expression_list(<arg>>array_expression('@(' statements:statement_list>pipeline>pipeline_chain><list>(unary_expression>integer_literal>decimal_integer_literal\"1\" ',' unary_expression>integer_literal>decimal_integer_literal\"2\")) ',' <arg>>invokation_expression(type_literal>type_spec>type_name>type_identifier\"Foo\" '::' member_name>simple_name\"N\" argument_list(argument_expression_list:argument_expression_list(<arg>>integer_literal>decimal_integer_literal\"3\" ',' <arg>>integer_literal>decimal_integer_literal\"4\"))))))",
            ),
            (
                "$THIS.m(1)",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(variable\"$THIS\" '.' member_name>simple_name\"m\" argument_list(argument_expression_list:argument_expression_list><arg>>integer_literal>decimal_integer_literal\"1\"))",
            ),
        ]);
    }

    #[test]
    fn u_static_member_variants() {
        check(&[
            (
                "[T]::new()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(type_literal>type_spec>type_name>type_identifier\"T\" '::' member_name>simple_name\"new\" argument_list\"()\")",
            ),
            (
                "[T]::M()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(type_literal>type_spec>type_name>type_identifier\"T\" '::' member_name>simple_name\"M\" argument_list\"()\")",
            ),
            (
                "[T]::M(1, 2, 3)",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(type_literal>type_spec>type_name>type_identifier\"T\" '::' member_name>simple_name\"M\" argument_list(argument_expression_list:argument_expression_list(<arg>>integer_literal>decimal_integer_literal\"1\" ',' <arg>>integer_literal>decimal_integer_literal\"2\" ',' <arg>>integer_literal>decimal_integer_literal\"3\")))",
            ),
            (
                "[T]::P",
                "program>statement_list>pipeline>pipeline_chain><expr>>member_access(type_literal>type_spec>type_name>type_identifier\"T\" '::' member_name>simple_name\"P\")",
            ),
            (
                "[System.Collections.Generic.List[int]]::new()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(type_literal>type_spec(generic_type_name>type_name(type_name(type_name(type_name>type_identifier\"System\" '.' type_identifier\"Collections\") '.' type_identifier\"Generic\") '.' type_identifier\"List\") generic_type_arguments>type_spec>type_name>type_identifier\"int\") '::' member_name>simple_name\"new\" argument_list\"()\")",
            ),
        ]);
    }

    #[test]
    fn v_instance_member_variants() {
        check(&[
            (
                "$x.M()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(variable\"$x\" '.' member_name>simple_name\"M\" argument_list\"()\")",
            ),
            (
                "(Get-X).M()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(parenthesized_expression>pipeline>pipeline_chain>command(command_name:command_name\"Get-X\") '.' member_name>simple_name\"M\" argument_list\"()\")",
            ),
            (
                "$this.P.Q()",
                "program>statement_list>pipeline>pipeline_chain><expr>>invokation_expression(member_access(variable\"$this\" '.' member_name>simple_name\"P\") '.' member_name>simple_name\"Q\" argument_list\"()\")",
            ),
            (
                "$x.P",
                "program>statement_list>pipeline>pipeline_chain><expr>>member_access(variable\"$x\" '.' member_name>simple_name\"P\")",
            ),
        ]);
    }

    #[test]
    fn w_class_and_enum_variants() {
        check(&[
            (
                "class C : System.Collections.Generic.List[int] { }",
                "program>statement_list>class_statement(simple_name\"C\" ':' ERROR(simple_name\"System\" '.' simple_name\"Collections\" '.' simple_name\"Generic\" '.' simple_name\"List\") simple_name\"int\" ERROR\"]\")",
            ),
            (
                "enum E : byte { A }",
                "program>statement_list>enum_statement(ERROR(simple_name\"E\" ':') simple_name\" byte\" enum_member>simple_name\"A\")",
            ),
            (
                "class C { [int]$P = (Get-Count) }",
                "program>statement_list>class_statement(simple_name\"C\" class_property_definition(type_literal>type_spec>type_name>type_identifier\"int\" variable\"$P\" '=' <expr>>parenthesized_expression>pipeline>pipeline_chain>command(command_name:command_name\"Get-Count\")))",
            ),
        ]);
    }

    #[test]
    fn x_dsc_resource_blocks() {
        let src = "configuration C { Node x { File f { } } }";
        // An empty `{ }` script-block argument recovers with a MISSING `;`.
        assert_eq!(errors(src), ["MISSING ;"]);
        check(&[(
            src,
            "program>statement_list>pipeline>pipeline_chain>command(command_name:command_name\"configuration\" command_elements:command_elements(command_argument_sep>' ' generic_token\"C\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"Node\" command_elements:command_elements(command_argument_sep>' ' generic_token\"x\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>pipeline>pipeline_chain>command(command_name:command_name\"File\" command_elements:command_elements(command_argument_sep>' ' generic_token\"f\" command_argument_sep>' ' array_literal_expression>unary_expression>script_block_expression>script_block(script_block_body:script_block_body(statement_list:statement_list>empty_statement>MISSING ';'))))))))))))",
        )]);
    }
}
