//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules (each starting with `use super::*;`), with any
//! shared fixtures and helpers at this root. e2e tests live outside this
//! tree in `tests/biceplib_e2e.rs`, under `mod e2e`.

use crate::{diag, parse, resolve, source, units};
use std::path::Path;

fn parse_text(text: &str) -> parse::File {
    parse::parse(&source::decode_mapped(text.as_bytes()))
}

mod unit {
    use super::*;

    #[test]
    fn is_bicep_source_claims_bicep_and_bicepparam_in_any_case() {
        for name in ["main.bicep", "MAIN.BICEP", "p.bicepparam", "P.BicepParam"] {
            assert!(source::is_bicep_source(Path::new(name)), "{name}");
        }
        for name in [
            "a.json",
            "a.bicep.json",
            "bicep",
            "main.tf",
            "x.bicepparam.bak",
        ] {
            assert!(!source::is_bicep_source(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn decode_strips_bom_and_maps_raw_offsets_past_it() {
        let d = source::decode_mapped(b"\xEF\xBB\xBFab");
        assert_eq!(d.text, "ab");
        assert_eq!(
            (d.raw_offset(0), d.raw_offset(1), d.raw_offset(2)),
            (3, 4, 5)
        );
    }

    #[test]
    fn decode_replaces_invalid_utf8_with_replacement_char() {
        let d = source::decode_mapped(b"a\xFFb");
        assert_eq!(d.text, "a\u{FFFD}b");
        // The replacement char (3 decoded bytes) maps to raw byte 1; `b` to raw byte 2.
        assert_eq!(
            (d.raw_offset(1), d.raw_offset(4), d.raw_offset(5)),
            (1, 2, 3)
        );
    }

    #[test]
    fn line_count_has_minimum_one_and_ignores_trailing_newline() {
        assert_eq!(source::line_count(""), 1);
        assert_eq!(source::line_count("a\nb\n"), 2);
        assert_eq!(source::line_count("a\n\nb"), 3);
    }

    #[test]
    fn span_gives_one_based_inclusive_lines() {
        let d = source::decode_mapped(b"param a int\nvar b = 1\n");
        // "param a int\n" is bytes 0..12; its line break stays on line 1.
        let span = |start, end, start_line, end_line| source::Span {
            start,
            end,
            start_line,
            end_line,
        };
        assert_eq!(source::span(&d, 0, 12), span(0, 12, 1, 1));
        assert_eq!(source::span(&d, 12, 21), span(12, 21, 2, 2));
        assert_eq!(source::span(&d, 0, 22), span(0, 22, 1, 2));
    }

    #[test]
    fn span_reports_raw_offsets_after_a_bom() {
        let d = source::decode_mapped(b"\xEF\xBB\xBFvar a = 1\n");
        let span = source::span(&d, 0, 9);
        assert_eq!((span.start, span.end, span.start_line), (3, 12, 1));
    }

    #[test]
    fn diagnostic_line_has_bicep_prefix_and_position() {
        let mut out = Vec::new();
        let err = parse::ParseError {
            line: 3,
            column: 7,
            message: "expected a string".into(),
        };
        diag::write_parse_error(&mut out, "/repo/a.bicep", &err).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "bicep: /repo/a.bicep:3:7: expected a string\n"
        );
    }

    #[test]
    fn error_summary_pluralises_the_file_count() {
        assert_eq!(diag::error_summary(0), "bicep: 0 files had syntax errors");
        assert_eq!(diag::error_summary(1), "bicep: 1 file had syntax errors");
        assert_eq!(diag::error_summary(2), "bicep: 2 files had syntax errors");
    }

    #[test]
    fn each_declaration_kind_maps_to_its_kind() {
        let f = parse_text(
            "param p string\nvar v = 1\noutput o string = v\ntype t = string\n\
             func f(x int) string => x\n\
             resource r 'Microsoft.Storage/storageAccounts@2023-01-01' = {\n}\n\
             module m './m.bicep' = {\n}\n",
        );
        let kinds: Vec<(parse::Kind, Option<&str>)> = f
            .decls
            .iter()
            .map(|d| (d.kind, d.name.as_ref().map(|n| n.text.as_str())))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (parse::Kind::Param, Some("p")),
                (parse::Kind::Var, Some("v")),
                (parse::Kind::Output, Some("o")),
                (parse::Kind::Type, Some("t")),
                (parse::Kind::Func, Some("f")),
                (parse::Kind::Resource, Some("r")),
                (parse::Kind::Module, Some("m")),
            ]
        );
        assert!(f.errors.is_empty());
    }

    #[test]
    fn string_interpolation_draws_refs_and_has_no_literal_value() {
        let f = parse_text("resource s 'Microsoft.Storage/${kind}Account@2023-01-01' = {\n}\n");
        let d = &f.decls[0];
        assert_eq!(d.literal.as_ref().unwrap().value, None);
        let names: Vec<&str> = d.refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["kind"]);
        assert!(f.errors.is_empty());
    }

    #[test]
    fn triple_quoted_and_escaped_literals_decode_their_text() {
        let f = parse_text("module m '''./net.bicep''' = {\n}\nmodule q 'a\\'b' = {\n}\n");
        let values: Vec<Option<String>> = f
            .decls
            .iter()
            .map(|d| d.literal.as_ref().unwrap().value.clone())
            .collect();
        assert_eq!(
            values,
            vec![Some("./net.bicep".to_string()), Some("a'b".to_string())]
        );
    }

    #[test]
    fn error_recovery_keeps_later_declarations() {
        let f = parse_text("param good string\nresource bad\nvar later = 2\n");
        let names: Vec<&str> = f
            .decls
            .iter()
            .filter_map(|d| d.name.as_ref().map(|n| n.text.as_str()))
            .collect();
        assert_eq!(names, vec!["good", "later"]);
        assert_eq!(
            f.errors,
            vec![parse::ParseError {
                line: 2,
                column: 13,
                message: "expected a string".into(),
            }]
        );
    }

    #[test]
    fn newline_ends_a_declaration_and_same_line_tokens_are_an_error() {
        let ok = parse_text("var a = 1\nvar b = 2\n");
        assert_eq!(ok.decls.len(), 2);
        assert!(ok.errors.is_empty());

        let same = parse_text("var a = 1 var b = 2\n");
        assert!(same.decls.is_empty());
        assert_eq!(
            same.errors,
            vec![parse::ParseError {
                line: 1,
                column: 11,
                message: "unexpected token after declaration".into(),
            }]
        );
    }

    #[test]
    fn metadata_and_test_statements_are_skipped_silently() {
        let f = parse_text("metadata desc = 'x'\ntest t 'a' = {\n}\nparam p string\n");
        let names: Vec<&str> = f
            .decls
            .iter()
            .filter_map(|d| d.name.as_ref().map(|n| n.text.as_str()))
            .collect();
        assert_eq!(names, vec!["p"]);
        assert!(f.errors.is_empty());
    }

    #[test]
    fn lambda_and_for_variables_are_marked_local_refs() {
        let f = parse_text("var xs = [for x in items: x]\nvar f = (a) => a + outer\n");
        let refs = |i: usize| {
            f.decls[i]
                .refs
                .iter()
                .map(|r| (r.name.as_str(), r.local))
                .collect::<Vec<_>>()
        };
        assert_eq!(refs(0), vec![("items", false), ("x", true)]);
        assert_eq!(refs(1), vec![("a", true), ("outer", false)]);
    }

    #[test]
    fn func_parameters_bind_only_in_the_body() {
        let f = parse_text("func g(p int) int => p + q\n");
        let refs: Vec<(&str, bool)> = f.decls[0]
            .refs
            .iter()
            .map(|r| (r.name.as_str(), r.local))
            .collect();
        assert_eq!(
            refs,
            vec![("int", false), ("int", false), ("p", true), ("q", false)]
        );
    }

    #[test]
    fn keyword_names_prefix_each_declaration_and_func_is_a_function() {
        let f = parse_text(
            "param p string\nvar v = 1\noutput o string = v\ntype t = string\n\
             func f(x int) string => x\n\
             resource stg 'Microsoft.Storage/storageAccounts@2023-01-01' = {\n}\n\
             module m './m.bicep' = {\n}\n\
             using 'main.bicep'\nextends 'p.bicepparam'\n",
        );
        let u = units::file_units(&f);
        let names: Vec<(&str, bool)> = u
            .units
            .iter()
            .map(|x| (x.name.as_str(), x.function))
            .collect();
        assert_eq!(
            names,
            vec![
                ("param.p", false),
                ("var.v", false),
                ("output.o", false),
                ("type.t", false),
                ("func.f", true),
                ("resource.stg", false),
                ("module.m", false),
                ("using", false),
                ("extends", false),
            ]
        );
        assert!(u.units.iter().all(|x| x.parent.is_none()));
    }

    #[test]
    fn child_resources_take_their_bare_name_under_the_parent() {
        let f = parse_text(
            "resource plan 'Microsoft.Web/serverfarms@2023-01-01' = {\n\
             \x20 name: 'p'\n\
             \x20 resource slot 'Microsoft.Web/sites/slots@2023-01-01' = {\n\
             \x20   name: 'slot'\n\
             \x20 }\n\
             }\n",
        );
        let u = units::file_units(&f);
        let names: Vec<(&str, Option<usize>)> = u
            .units
            .iter()
            .map(|x| (x.name.as_str(), x.parent))
            .collect();
        assert_eq!(names, vec![("resource.plan", None), ("slot", Some(0))]);
        assert_eq!(
            (u.units[1].span.start_line, u.units[1].span.end_line),
            (3, 5)
        );
    }

    #[test]
    fn import_symbols_each_become_a_unit_sharing_the_statement_span() {
        let f = parse_text(
            "import { vnet, subnet as sn } from 'net.bicep'\nimport * as ns from 'x.bicep'\n",
        );
        let u = units::file_units(&f);
        let names: Vec<&str> = u.units.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["import.vnet", "import.sn", "import.ns"]);
        assert_eq!(u.units[0].span, u.units[1].span);
        assert_eq!(u.units[0].span.start_line, 1);
        let sn = u.units[1].import.unwrap();
        assert_eq!(sn.remote.as_ref().unwrap().text, "subnet");
        assert_eq!(sn.local.text, "sn");
        assert!(u.units[2].import.unwrap().remote.is_none());
    }

    #[test]
    fn repeated_name_in_one_scope_is_kept_once_and_reported() {
        let f = parse_text("param p string\nparam p int\n");
        let u = units::file_units(&f);
        assert_eq!(u.units.len(), 1);
        let dups: Vec<(usize, &str)> = u
            .duplicates
            .iter()
            .map(|d| (d.line, d.name.as_str()))
            .collect();
        assert_eq!(dups, vec![(2, "param.p")]);
    }

    #[test]
    fn duplicate_resource_drops_its_child_resources() {
        let f = parse_text(
            "resource a 'x' = {\n  resource c 'y' = {\n  }\n}\n\
             resource a 'x' = {\n  resource d 'z' = {\n  }\n}\n",
        );
        let u = units::file_units(&f);
        let names: Vec<&str> = u.units.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["resource.a", "c"]);
        let dups: Vec<(usize, &str)> = u
            .duplicates
            .iter()
            .map(|d| (d.line, d.name.as_str()))
            .collect();
        assert_eq!(dups, vec![(5, "resource.a")]);
    }

    #[test]
    fn normalise_joins_relative_paths_and_drops_dots() {
        assert_eq!(resolve::normalise("", "main.bicep"), "main.bicep");
        assert_eq!(
            resolve::normalise("net", "./sub//x.bicep"),
            "net/sub/x.bicep"
        );
        assert_eq!(resolve::normalise("a/b", "../c.bicep"), "a/c.bicep");
        assert_eq!(resolve::normalise("a", "../../x.bicep"), "../x.bicep");
        assert_eq!(resolve::normalise("", "../x.bicep"), "../x.bicep");
    }

    #[test]
    fn directory_is_the_part_before_the_last_slash() {
        assert_eq!(resolve::directory("app/net/vnet.bicep"), "app/net");
        assert_eq!(resolve::directory("main.bicep"), "");
    }

    #[test]
    fn registry_references_are_br_and_ts_with_or_without_alias() {
        for s in [
            "br:mcr.microsoft.com/x:1",
            "br/alias:x:1",
            "ts:spec:1",
            "ts/alias:spec",
        ] {
            assert!(resolve::registry(s), "{s}");
        }
        for s in ["./net.bicep", "net.bicep", "brx:x", "bicep:x", "tsx/a:b"] {
            assert!(!resolve::registry(s), "{s}");
        }
    }
}

mod int {}
