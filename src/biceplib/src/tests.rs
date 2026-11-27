//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules (each starting with `use super::*;`), with any
//! shared fixtures and helpers at this root. e2e tests live outside this
//! tree in `tests/biceplib_e2e.rs`, under `mod e2e`.

use crate::{diag, parse, resolve, scan, source, units};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn parse_text(text: &str) -> parse::File {
    parse::parse(&source::decode_mapped(text.as_bytes()))
}

/// One resolved fact as `kind from -> to`, naming units by their keyword names.
fn render(sources: &[resolve::Source], file: usize, fact: &resolve::Fact) -> String {
    let here = |i: usize| sources[file].units.units[i].name.as_str();
    let target =
        |f: usize, i: usize| format!("{}::{}", sources[f].path, sources[f].units.units[i].name);
    match fact {
        resolve::Fact::Uses {
            from,
            file: target_file,
            to,
        } => format!("uses {} -> {}", here(*from), target(*target_file, *to)),
        resolve::Fact::Calls { from, to } => format!("calls {} -> {}", here(*from), here(*to)),
        resolve::Fact::FileUses { from, path, kind } => {
            format!("file {kind} {} -> {path}", here(*from))
        }
        resolve::Fact::Unresolved {
            from,
            name,
            category,
        } => format!("unresolved {category} {} -> {name}", here(*from)),
    }
}

/// Parses every `(repo-relative path, text)` file, then resolves them together.
fn resolved(files: &[(&str, &str)]) -> Vec<Vec<String>> {
    let parsed: Vec<parse::File> = files.iter().map(|(_, text)| parse_text(text)).collect();
    let sources: Vec<resolve::Source> = files
        .iter()
        .zip(&parsed)
        .map(|((path, _), file)| resolve::Source {
            path,
            units: units::file_units(file),
        })
        .collect();
    resolve::resolve(&sources)
        .iter()
        .enumerate()
        .map(|(file, facts)| facts.iter().map(|f| render(&sources, file, f)).collect())
        .collect()
}

/// The facts core over `(repo-relative path, text)` files under the root `/repo`.
fn records(files: &[(&str, &str)]) -> (Vec<scan::Rec>, usize, String) {
    let owned: Vec<(PathBuf, Vec<u8>)> = files
        .iter()
        .map(|(p, t)| (Path::new("/repo").join(p), t.as_bytes().to_vec()))
        .collect();
    let mut err = Vec::new();
    let (recs, failed) = scan::facts(Path::new("/repo"), &owned, "bp", &mut err).unwrap();
    (recs, failed, String::from_utf8(err).unwrap())
}

const MAIN: &str = "param location string
import { skuName as sku, defaultSku, nope } from '../shared/store.bicep'
import * as ts from 'ts:spec/ref:1.0.0'
module net './net/vnet.bicep' = {
  name: 'net'
}
module arm './arm/template.json' = {
  name: 'arm'
}
module reg 'br:mcr.microsoft.com/bicep/avm/res/storage:1.0.0' = {
  name: 'reg'
}
module gone './gone.bicep' = {
  name: 'gone'
}
module esc '../../outside.bicep' = {
  name: 'esc'
}
module comp './${name}.bicep' = {
  name: 'comp'
}
resource stg 'Microsoft.Storage/storageAccounts@2023-01-01' = {
  name: 'stg1'
  location: location
  sku: {
    name: sku
  }
}
resource web 'Microsoft.Web/sites@2023-01-01' = {
  name: 'web'
  properties: {
    serverFarmId: plan::slot
  }
}
resource plan 'Microsoft.Web/serverfarms@2023-01-01' = {
  name: 'plan'
  resource slot 'Microsoft.Web/sites/slots@2023-01-01' = {
    name: 'slot'
  }
}
func double(n int) int => n * 2
func quad(n int) int => double(double(n))
var answer = double(21)
";

const VNET: &str = "param addressPrefix string
resource vnet 'Microsoft.Network/virtualNetworks@2023-04-01' = {
  name: 'vnet1'
  properties: {
    prefix: addressPrefix
  }
}
output vnetId string = vnet.id
";

const VNET_RES: &str = "resource vnet 'Microsoft.Network/virtualNetworks@2023-04-01' = {
  name: 'vnet1'
  resource subnet 'Microsoft.Network/virtualNetworks/subnets@2023-04-01' = {
    name: 'default'
  }
}
";

const STORE: &str = "type skuName = string
@export()
var defaultSku = 'Standard_LRS'
func mk() string => 'x'
resource stgAcc 'Microsoft.Storage/storageAccounts@2023-01-01' = {
  name: 'sa'
  sku: {
    name: defaultSku
  }
}
";

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

mod int {
    use super::*;

    #[test]
    fn resolves_imports_modules_registry_and_unknown_paths_across_files() {
        let got = resolved(&[
            ("app/main.bicep", MAIN),
            ("app/net/vnet.bicep", VNET),
            ("app/arm/template.json", ""),
            ("shared/store.bicep", STORE),
        ]);
        let nameless = format!(
            "unresolved unknown module.comp -> <nameless@app/main.bicep:{}>",
            MAIN.find("'./${").unwrap()
        );
        assert_eq!(
            got[0],
            [
                "file import import.sku -> shared/store.bicep",
                "uses import.sku -> shared/store.bicep::type.skuName",
                "file import import.defaultSku -> shared/store.bicep",
                "uses import.defaultSku -> shared/store.bicep::var.defaultSku",
                "file import import.nope -> shared/store.bicep",
                "unresolved external import.ts -> ts:spec/ref:1.0.0",
                "file module module.net -> app/net/vnet.bicep",
                "file module module.arm -> app/arm/template.json",
                "unresolved external module.reg -> br:mcr.microsoft.com/bicep/avm/res/storage:1.0.0",
                "unresolved unknown module.gone -> app/gone.bicep",
                "unresolved unknown module.esc -> ../outside.bicep",
                nameless.as_str(),
                "uses resource.stg -> app/main.bicep::param.location",
                "uses resource.stg -> app/main.bicep::import.sku",
                "uses resource.web -> app/main.bicep::slot",
                "calls func.quad -> func.double",
            ]
        );
        assert!(got[2].is_empty());
        assert_eq!(
            got[3],
            ["uses resource.stgAcc -> shared/store.bicep::var.defaultSku"]
        );
    }

    #[test]
    fn a_resource_links_to_its_param_and_an_output_to_its_resource() {
        let got = resolved(&[("app/net/vnet.bicep", VNET)]);
        assert_eq!(
            got[0],
            [
                "uses resource.vnet -> app/net/vnet.bicep::param.addressPrefix",
                "uses output.vnetId -> app/net/vnet.bicep::resource.vnet",
            ]
        );
    }

    #[test]
    fn bicepparam_using_and_extends_draw_file_edges_to_their_targets() {
        let got = resolved(&[
            (
                "app/dev.bicepparam",
                "using './main.bicep'\nextends './base.bicepparam'\nparam location = 'westeurope'\n",
            ),
            ("app/base.bicepparam", "param sku = 'Standard_LRS'\n"),
            ("app/main.bicep", "param location string\n"),
        ]);
        assert_eq!(
            got[0],
            [
                "file using using -> app/main.bicep",
                "file extends extends -> app/base.bicepparam",
            ]
        );
    }

    #[test]
    fn a_module_may_not_name_a_bicepparam_file() {
        let got = resolved(&[
            (
                "app/p.bicep",
                "module m './base.bicepparam' = {\n  name: 'x'\n}\n",
            ),
            ("app/base.bicepparam", ""),
        ]);
        assert_eq!(
            got[0],
            ["unresolved unknown module.m -> app/base.bicepparam"]
        );
    }

    #[test]
    fn facts_emit_module_file_struct_contains_and_struct_to_file_uses() {
        let main = "param location string\nmodule net './net/vnet.bicep' = {\n  name: 'net'\n}\n";
        let (recs, failed, err) =
            records(&[("app/main.bicep", main), ("app/net/vnet.bicep", VNET_RES)]);
        assert_eq!((failed, err.as_str()), (0, ""));
        assert_eq!(
            serde_json::to_value(&recs).unwrap(),
            serde_json::json!([
                {"type": "module", "fqn": "app/main.bicep"},
                {"type": "file", "path": "/repo/app/main.bicep", "parent": "app/main.bicep", "start_line": 1, "end_line": 4},
                {"type": "struct", "id": "bp1", "parent": "app/main.bicep", "name": "param.location", "path": "/repo/app/main.bicep", "start": 0, "end": 21, "start_line": 1, "end_line": 1},
                {"type": "struct", "id": "bp2", "parent": "app/main.bicep", "name": "module.net", "path": "/repo/app/main.bicep", "start": main.find("module net").unwrap(), "end": main.rfind('}').unwrap() + 1, "start_line": 2, "end_line": 4},
                {"type": "uses", "from": "bp2", "to": "/repo/app/net/vnet.bicep", "properties": {"kind": "module"}},
                {"type": "module", "fqn": "app/net/vnet.bicep"},
                {"type": "file", "path": "/repo/app/net/vnet.bicep", "parent": "app/net/vnet.bicep", "start_line": 1, "end_line": 6},
                {"type": "struct", "id": "bp3", "parent": "app/net/vnet.bicep", "name": "resource.vnet", "path": "/repo/app/net/vnet.bicep", "start": 0, "end": VNET_RES.trim_end().len(), "start_line": 1, "end_line": 6},
                {"type": "struct", "id": "bp4", "parent": "app/net/vnet.bicep.resource.vnet", "name": "subnet", "path": "/repo/app/net/vnet.bicep", "start": VNET_RES.find("resource subnet").unwrap(), "end": VNET_RES.find("  }\n}").unwrap() + 3, "start_line": 3, "end_line": 5},
                {"type": "contains", "from": "bp3", "to": "bp4"},
            ])
        );
    }

    #[test]
    fn syntax_errors_and_duplicates_are_diagnosed_and_the_scan_continues() {
        let (recs, failed, err) = records(&[(
            "app/dup.bicep",
            "param p string\nparam p int\nresource bad\n",
        )]);
        assert_eq!(failed, 1);
        assert_eq!(
            err,
            "bicep: /repo/app/dup.bicep:3:13: expected a string\n\
             bicep: /repo/app/dup.bicep:2: duplicate declaration param.p\n"
        );
        assert_eq!(
            serde_json::to_value(&recs).unwrap(),
            serde_json::json!([
                {"type": "module", "fqn": "app/dup.bicep"},
                {"type": "file", "path": "/repo/app/dup.bicep", "parent": "app/dup.bicep", "start_line": 1, "end_line": 3},
                {"type": "struct", "id": "bp1", "parent": "app/dup.bicep", "name": "param.p", "path": "/repo/app/dup.bicep", "start": 0, "end": 14, "start_line": 1, "end_line": 1},
            ])
        );
    }

    #[test]
    fn targets_retain_drops_other_files_and_keeps_file_paths_unrewritten() {
        let main = "param location string\nmodule net './net/vnet.bicep' = {\n  name: 'net'\n}\n";
        let (recs, _, _) = records(&[("app/main.bicep", main), ("app/net/vnet.bicep", VNET_RES)]);
        let targets = HashSet::from(["/repo/app/main.bicep".to_string()]);
        assert_eq!(
            serde_json::to_value(scan::retain(recs, &targets)).unwrap(),
            serde_json::json!([
                {"type": "module", "fqn": "app/main.bicep"},
                {"type": "file", "path": "/repo/app/main.bicep", "parent": "app/main.bicep", "start_line": 1, "end_line": 4},
                {"type": "struct", "id": "bp1", "parent": "app/main.bicep", "name": "param.location", "path": "/repo/app/main.bicep", "start": 0, "end": 21, "start_line": 1, "end_line": 1},
                {"type": "struct", "id": "bp2", "parent": "app/main.bicep", "name": "module.net", "path": "/repo/app/main.bicep", "start": main.find("module net").unwrap(), "end": main.rfind('}').unwrap() + 1, "start_line": 2, "end_line": 4},
                {"type": "uses", "from": "bp2", "to": "/repo/app/net/vnet.bicep", "properties": {"kind": "module"}},
            ])
        );
    }

    #[test]
    fn retain_retargets_a_uses_into_a_dropped_file_to_its_struct_key() {
        let import = "import { skuName } from '../shared/store.bicep'\n";
        let store = "type skuName = string\n";
        let (recs, _, _) = records(&[("app/main.bicep", import), ("shared/store.bicep", store)]);
        let targets = HashSet::from(["/repo/app/main.bicep".to_string()]);
        assert_eq!(
            serde_json::to_value(scan::retain(recs, &targets)).unwrap(),
            serde_json::json!([
                {"type": "module", "fqn": "app/main.bicep"},
                {"type": "file", "path": "/repo/app/main.bicep", "parent": "app/main.bicep", "start_line": 1, "end_line": 1},
                {"type": "struct", "id": "bp1", "parent": "app/main.bicep", "name": "import.skuName", "path": "/repo/app/main.bicep", "start": 0, "end": import.trim_end().len(), "start_line": 1, "end_line": 1},
                {"type": "uses", "from": "bp1", "to": "/repo/shared/store.bicep", "properties": {"kind": "import"}},
                {"type": "uses", "from": "bp1", "to": "shared/store.bicep.type.skuName"},
            ])
        );
    }
}
