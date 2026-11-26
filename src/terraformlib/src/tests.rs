//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules (each starting with `use super::*;`), with any
//! shared fixtures and helpers at this root. e2e tests live outside this
//! tree in `tests/terraformlib_e2e.rs`, under `mod e2e`.

use crate::{module, parse, resolve, source, units};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

mod unit {
    use crate::{diag, module, parse, source};
    use std::path::{Path, PathBuf};

    fn decoded(text: &str) -> source::Decoded {
        source::decode_mapped(text.as_bytes())
    }

    #[test]
    fn source_claims_terraform_suffixes_in_any_case() {
        for name in [
            "main.tf",
            "MAIN.TF",
            "a.tf.json",
            "a.TF.JSON",
            "vars.tfvars",
            "vars.tfvars.json",
            "m.tofu",
            "m.tofu.json",
            "t.tftest.hcl",
            "t.tofutest.hcl",
            ".terraform.lock.hcl",
        ] {
            assert!(source::is_terraform_source(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn source_rejects_plain_hcl_json_and_near_misses() {
        for name in ["x.hcl", "x.json", "x.tf.bak", "x.tfstate", "tf"] {
            assert!(!source::is_terraform_source(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn decode_strips_bom_and_maps_raw_offsets_past_it() {
        let d = source::decode_mapped(&[0xEF, 0xBB, 0xBF, b'a', b'\n', b'b']);
        assert_eq!(d.text, "a\nb");
        assert_eq!(d.raw_offset(0), 3);
        assert_eq!(d.raw_offset(2), 5);
        assert_eq!(d.raw_offset(99), 6, "past the end clamps to the raw length");
    }

    #[test]
    fn decode_replaces_invalid_utf8_with_replacement_char() {
        let d = source::decode_mapped(&[b'a', 0xFF, b'b']);
        assert_eq!(d.text, "a\u{FFFD}b");
        assert_eq!(d.raw_offset(1), 1, "replacement starts at the bad byte");
        assert_eq!(d.raw_offset(4), 2, "the trailing `b` maps back to raw 2");
        assert_eq!(d.raw_offset(5), 3);
    }

    #[test]
    fn line_count_has_minimum_one_and_ignores_trailing_newline() {
        assert_eq!(source::line_count(""), 1);
        assert_eq!(source::line_count("a\nb\n"), 2);
        assert_eq!(source::line_count("a\r\nb"), 2);
    }

    #[test]
    fn span_gives_one_based_inclusive_lines() {
        let d = decoded("ab\ncd\n");
        assert_eq!(
            source::span(&d, 0, 3),
            source::Span {
                start: 0,
                end: 3,
                start_line: 1,
                end_line: 1,
            }
        );
        assert_eq!(
            source::span(&d, 3, 6),
            source::Span {
                start: 3,
                end: 6,
                start_line: 2,
                end_line: 2,
            }
        );
    }

    #[test]
    fn span_reports_raw_offsets_after_a_bom() {
        let d = source::decode_mapped(b"\xEF\xBB\xBFab");
        let s = source::span(&d, 0, 2);
        assert_eq!((s.start, s.end), (3, 5));
    }

    #[test]
    fn diagnostic_line_has_terraform_prefix_and_position() {
        let mut out = Vec::new();
        diag::write_diagnostic(&mut out, "/r/a.tf", 3, 5, "boom").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "terraform: /r/a.tf:3:5: boom\n"
        );
    }

    #[test]
    fn error_summary_pluralises_the_file_count() {
        assert_eq!(
            diag::error_summary(0),
            "terraform: 0 files had syntax errors"
        );
        assert_eq!(
            diag::error_summary(1),
            "terraform: 1 file had syntax errors"
        );
        assert_eq!(
            diag::error_summary(2),
            "terraform: 2 files had syntax errors"
        );
    }

    #[test]
    fn rel_is_slash_separated_and_empty_at_the_root() {
        let root = Path::new("/r");
        assert_eq!(module::rel(root, Path::new("/r")), "");
        assert_eq!(module::rel(root, Path::new("/r/a/b.tf")), "a/b.tf");
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn modules_group_by_directory_and_test_tests_dir_belongs_to_parent() {
        let mods = module::modules(
            Path::new("/r"),
            &paths(&["/r/main.tf", "/r/mod/a.tf", "/r/mod/tests/x.tftest.hcl"]),
        );
        assert_eq!(mods[""].base, vec!["main.tf"]);
        assert_eq!(mods["mod"].base, vec!["mod/a.tf"]);
        assert_eq!(mods["mod"].tests, vec!["mod/tests/x.tftest.hcl"]);
        assert!(!mods.contains_key("mod/tests"));
    }

    #[test]
    fn modules_match_suffixes_case_insensitively_and_bucket_kinds() {
        let mods = module::modules(
            Path::new("/r"),
            &paths(&[
                "/r/Main.TF",
                "/r/override.tf",
                "/r/a_override.tf",
                "/r/prod.tfvars.json",
                "/r/.terraform.lock.hcl",
            ]),
        );
        let m = &mods[""];
        assert_eq!(m.base, vec!["Main.TF"]);
        assert_eq!(m.overrides, vec!["a_override.tf", "override.tf"]);
        assert_eq!(m.tfvars, vec!["prod.tfvars.json"]);
        assert_eq!(m.locks, vec![".terraform.lock.hcl"]);
    }

    #[test]
    fn modules_tofu_shadows_same_base_name_tf() {
        let mods = module::modules(Path::new("/r"), &paths(&["/r/main.tf", "/r/main.tofu"]));
        let m = &mods[""];
        assert_eq!(m.shadowed, vec!["main.tf"]);
        assert_eq!(m.base, vec!["main.tofu"]);
    }

    #[test]
    fn modules_tofutest_shadows_tftest() {
        let mods = module::modules(
            Path::new("/r"),
            &paths(&["/r/x.tftest.hcl", "/r/x.tofutest.hcl"]),
        );
        let m = &mods[""];
        assert_eq!(m.shadowed, vec!["x.tftest.hcl"]);
        assert_eq!(m.tests, vec!["x.tofutest.hcl"]);
    }

    #[test]
    fn modules_skip_unclaimed_files() {
        let mods = module::modules(Path::new("/r"), &paths(&["/r/x.hcl", "/r/y.json"]));
        assert!(mods.is_empty());
    }

    fn native(text: &str) -> parse::Body {
        parse::parse(Path::new("main.tf"), text).unwrap()
    }

    #[test]
    fn native_block_header_labels_and_literal_attribute() {
        let body = native("resource \"aws_vpc\" \"main\" {\n  name = \"plain\"\n  p = aws\n}\n");
        let b = &body.blocks[0];
        assert_eq!(b.kind, "resource");
        assert_eq!(b.labels, vec!["aws_vpc", "main"]);
        assert_eq!(b.body.attributes[0].name, "name");
        assert_eq!(b.body.attributes[0].literal.as_deref(), Some("plain"));
        assert_eq!(b.body.attributes[1].variable.as_deref(), Some("aws"));
    }

    #[test]
    fn native_interpolated_string_is_not_a_literal() {
        let body = native("x = \"a-${var.y}\"\n");
        assert_eq!(body.attributes[0].literal, None);
        assert_eq!(body.attributes[0].refs.len(), 1);
    }

    #[test]
    fn native_traversal_is_collected_as_a_reference() {
        let body = native("cidr = var.cidr\n");
        let refs = &body.attributes[0].refs;
        assert_eq!(refs.len(), 1);
        assert!(matches!(refs[0].target, parse::Target::Traversal(_)));
    }

    #[test]
    fn native_for_expr_records_collection_but_not_iterator() {
        let body = native("y = [for k in local.a : k.name]\n");
        let refs = &body.attributes[0].refs;
        assert_eq!(
            refs.len(),
            1,
            "only local.a; k.name is rooted at the iterator"
        );
        assert!(matches!(refs[0].target, parse::Target::Traversal(_)));
    }

    #[test]
    fn native_for_template_records_collection_but_not_iterator() {
        let body = native("t = \"%{ for x in var.items }${x.id}%{ endfor }\"\n");
        let refs = &body.attributes[0].refs;
        assert_eq!(
            refs.len(),
            1,
            "only var.items; x.id is rooted at the iterator"
        );
    }

    #[test]
    fn native_parse_error_reports_the_failing_line() {
        let err = parse::parse(Path::new("main.tf"), "a = 1\nb = = 2\n").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.column >= 1);
    }

    fn json(name: &str, text: &str) -> parse::Body {
        parse::parse(Path::new(name), text).unwrap()
    }

    #[test]
    fn json_labels_nest_one_level_per_label_with_block_span() {
        let text = r#"{"resource":{"aws_vpc":{"main":{"cidr_block":"10.0.0.0/16"}}}}"#;
        let body = json("a.tf.json", text);
        let b = &body.blocks[0];
        assert_eq!(b.kind, "resource");
        assert_eq!(b.labels, vec!["aws_vpc", "main"]);
        assert_eq!(b.body.attributes[0].literal.as_deref(), Some("10.0.0.0/16"));
        assert_eq!(b.span.start, text.find("\"main\"").unwrap());
    }

    #[test]
    fn json_keeps_duplicate_keys_as_separate_blocks() {
        let body = json("a.tf.json", r#"{"variable":{"a":{},"a":{}}}"#);
        assert_eq!(body.blocks.len(), 2);
        assert!(body.blocks.iter().all(|b| b.labels == vec!["a"]));
    }

    #[test]
    fn json_skips_comment_properties() {
        let body = json("a.tf.json", r#"{"//":"note","variable":{"x":{}}}"#);
        assert_eq!(body.blocks.len(), 1);
        assert_eq!(body.blocks[0].labels, vec!["x"]);
        assert!(body.attributes.is_empty());
    }

    #[test]
    fn json_string_escapes_decode_to_their_characters() {
        let body = json("a.tf.json", r#"{"output":{"o":{"v":"a\"b\u00e9\n"}}}"#);
        assert_eq!(
            body.blocks[0].body.attributes[0].literal.as_deref(),
            Some("a\"b\u{e9}\n")
        );
    }

    #[test]
    fn json_error_reports_line_and_column_of_the_bad_byte() {
        let err = parse::parse(Path::new("a.tf.json"), "{\n  \"a\" 1}").unwrap_err();
        assert_eq!((err.line, err.column), (2, 7));
        assert_eq!(err.message, "expected `:` after key");
    }

    #[test]
    fn json_root_must_be_an_object() {
        let err = parse::parse(Path::new("a.tf.json"), "[1]").unwrap_err();
        assert_eq!((err.line, err.column), (1, 1));
        assert_eq!(err.message, "expected a JSON object at the root");
    }

    #[test]
    fn tfvars_json_root_keys_are_attributes_not_blocks() {
        let body = json(
            "prod.tfvars.json",
            r#"{"region":"us-east-1","vars":{"a":1},"//":"c"}"#,
        );
        assert!(body.blocks.is_empty());
        let names: Vec<&str> = body.attributes.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["region", "vars"]);
    }

    #[test]
    fn tfvars_json_suffix_is_matched_in_any_case() {
        let body = json("PROD.TFVARS.JSON", r#"{"vars":{}}"#);
        assert!(body.blocks.is_empty());
        assert_eq!(body.attributes[0].name, "vars");
    }

    #[test]
    fn json_template_reference_maps_to_raw_bytes() {
        let text = r#"{"resource":{"a":{"b":{"x":"${var.y}"}}}}"#;
        let body = json("a.tf.json", text);
        let refs = &body.blocks[0].body.attributes[0].refs;
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].span.start, text.find("var.y").unwrap());
    }
}

/// One wired run over in-memory `(repo-relative path, text)` files, with every
/// module's facts keyed by qualified Struct name (`<dir>::<name>`, bare in the root).
#[derive(Default)]
struct Wired {
    structs: Vec<String>,
    uses: Vec<(String, String)>,
    files: Vec<(String, String, &'static str)>,
    unresolved: Vec<(String, String, &'static str)>,
    diags: Vec<String>,
}

fn qual(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}::{name}")
    }
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

fn tri(a: &str, b: &str, c: &'static str) -> (String, String, &'static str) {
    (a.to_string(), b.to_string(), c)
}

/// Maps `files` to modules, parses them (shadowed files skipped), scans each module's
/// blocks and resolves its references: pure in-memory, no disk access.
fn wire(files: &[(&str, &str)]) -> Wired {
    let root = Path::new("/r");
    let paths: Vec<PathBuf> = files.iter().map(|&(p, _)| root.join(p)).collect();
    let modules = module::modules(root, &paths);
    let shadowed: HashSet<&str> = modules
        .values()
        .flat_map(|m| m.shadowed.iter().map(String::as_str))
        .collect();
    let parsed: Vec<units::Parsed> = files
        .iter()
        .filter(|&&(p, _)| !shadowed.contains(p))
        .map(|&(p, text)| {
            let text = source::decode_mapped(text.as_bytes());
            let body = parse::parse(Path::new(p), &text.text)
                .unwrap_or_else(|e| panic!("{p}: {}", e.message));
            units::Parsed {
                path: p.to_string(),
                text,
                body,
            }
        })
        .collect();
    let mut w = Wired::default();
    for (dir, m) in &modules {
        let found = units::module_units(dir, m, &parsed);
        let names: Vec<String> = found.structs.iter().map(|u| qual(dir, &u.name)).collect();
        w.diags.extend(
            found
                .diagnostics
                .iter()
                .map(|d| format!("{}:{}: {}", d.file, d.line, d.message)),
        );
        for fact in resolve::resolve(dir, &modules, &found, &parsed) {
            match fact {
                resolve::Fact::Uses { from, to } => {
                    w.uses.push((names[from].clone(), names[to].clone()));
                }
                resolve::Fact::FileUses { from, path, kind } => {
                    w.files.push((names[from].clone(), path, kind));
                }
                resolve::Fact::Unresolved {
                    from,
                    name,
                    category,
                } => w.unresolved.push((names[from].clone(), name, category)),
            }
        }
        w.structs.extend(names);
    }
    w
}

mod int {
    use super::*;

    #[test]
    fn block_facts_name_every_kind_of_block() {
        let w = wire(&[(
            "main.tf",
            r#"
resource "aws_vpc" "main" {}
data "aws_ami" "ubuntu" {}
variable "region" {}
output "id" {}
module "net" {
  source = "hashicorp/vpc/aws"
}
provider "aws" {
  region = "us-east-1"
}
provider "aws" {
  alias = "west"
}
locals {
  a = 1
  b = 2
}
"#,
        )]);
        assert_eq!(
            w.structs,
            [
                "aws_vpc.main",
                "data.aws_ami.ubuntu",
                "var.region",
                "output.id",
                "module.net",
                "provider.aws",
                "provider.aws.west",
                "local.a",
                "local.b",
            ]
        );
        assert_eq!(
            w.unresolved,
            [tri("module.net", "hashicorp/vpc/aws", "external")]
        );
        assert!(w.uses.is_empty() && w.diags.is_empty());
    }

    #[test]
    fn lock_and_test_files_yield_their_structs() {
        let w = wire(&[
            (
                ".terraform.lock.hcl",
                "provider \"registry.terraform.io/hashicorp/aws\" {\n  version = \"5.0.0\"\n}\n",
            ),
            ("tests/x.tftest.hcl", "run \"smoke\" {}\nrun \"plan\" {}\n"),
        ]);
        assert_eq!(
            w.structs,
            [
                "test.tests/x.tftest.hcl",
                "run.smoke",
                "run.plan",
                "lock.registry.terraform.io/hashicorp/aws",
            ]
        );
    }

    #[test]
    fn duplicate_base_declaration_keeps_the_first_and_reports_it() {
        let w = wire(&[
            ("a.tf", "variable \"x\" {}\n"),
            ("b.tf", "variable \"x\" {}\n"),
        ]);
        assert_eq!(w.structs, ["var.x"]);
        assert_eq!(
            w.diags,
            ["b.tf:1: duplicate declaration of `var.x`, first declared at a.tf:1"]
        );
    }

    #[test]
    fn override_block_without_a_base_match_is_dropped_and_reported() {
        let w = wire(&[
            ("main.tf", "resource \"a\" \"b\" {}\n"),
            ("zz_override.tf", "resource \"a\" \"c\" {}\n"),
        ]);
        assert_eq!(w.structs, ["a.b"]);
        assert_eq!(
            w.diags,
            ["zz_override.tf:1: override block `a.c` matches no base declaration"]
        );
    }

    #[test]
    fn same_module_references_resolve_and_context_values_draw_nothing() {
        let w = wire(&[(
            "main.tf",
            r#"
variable "region" {}
resource "aws_vpc" "main" {
  cidr = var.region
  name = lower(var.region)
  az   = count.index
  id   = self.id
  tf   = terraform.workspace
  p    = path.module
  e    = each.key
  zone = aws_subnet.missing.id
}
resource "aws_subnet" "s" {
  vpc = aws_vpc.main.id
}
"#,
        )]);
        assert_eq!(w.structs, ["var.region", "aws_vpc.main", "aws_subnet.s"]);
        assert_eq!(
            w.uses,
            [
                pair("aws_vpc.main", "var.region"),
                pair("aws_subnet.s", "aws_vpc.main"),
            ]
        );
        assert_eq!(
            w.unresolved,
            [tri("aws_vpc.main", "aws_subnet.missing", "unknown")]
        );
    }

    #[test]
    fn dynamic_iterators_by_label_and_by_attribute_draw_no_edges() {
        let w = wire(&[(
            "main.tf",
            r#"
variable "rules" {}
resource "aws_security_group" "sg" {
  dynamic "ingress" {
    for_each = var.rules
    content {
      port = ingress.value
    }
  }
  dynamic "egress" {
    for_each = var.rules
    iterator = it
    content {
      port = it.value
    }
  }
}
"#,
        )]);
        assert_eq!(w.structs, ["var.rules", "aws_security_group.sg"]);
        assert_eq!(w.uses, [pair("aws_security_group.sg", "var.rules")]);
        assert!(w.unresolved.is_empty());
    }

    #[test]
    fn meta_argument_and_provider_references_resolve_to_their_structs() {
        let w = wire(&[(
            "main.tf",
            r#"
provider "aws" {
  region = "us-east-1"
}
provider "aws" {
  alias = "west"
}
resource "aws_vpc" "main" {
  provider = aws
}
resource "aws_instance" "web" {
  provider   = aws.west
  depends_on = [aws_vpc.main]
  lifecycle {
    replace_triggered_by = [aws_vpc.main]
  }
}
"#,
        )]);
        assert_eq!(
            w.structs,
            [
                "provider.aws",
                "provider.aws.west",
                "aws_vpc.main",
                "aws_instance.web",
            ]
        );
        assert_eq!(
            w.uses,
            [
                pair("aws_vpc.main", "provider.aws"),
                pair("aws_instance.web", "provider.aws.west"),
                pair("aws_instance.web", "aws_vpc.main"),
            ]
        );
        assert!(w.unresolved.is_empty());
    }

    #[test]
    fn undeclared_provider_is_an_unknown_unresolved_target() {
        let w = wire(&[("main.tf", "resource \"x\" \"y\" {\n  provider = aws\n}\n")]);
        assert_eq!(w.unresolved, [tri("x.y", "provider.aws", "unknown")]);
    }

    #[test]
    fn module_providers_map_resolves_to_the_aliased_provider() {
        let w = wire(&[(
            "main.tf",
            r#"
provider "aws" {
  alias = "west"
}
module "vpc" {
  source    = "hashicorp/vpc/aws"
  providers = { aws = aws.west }
}
"#,
        )]);
        assert_eq!(w.structs, ["provider.aws.west", "module.vpc"]);
        assert_eq!(w.uses, [pair("module.vpc", "provider.aws.west")]);
        assert_eq!(
            w.unresolved,
            [tri("module.vpc", "hashicorp/vpc/aws", "external")]
        );
    }

    #[test]
    fn override_references_leave_the_base_struct_with_one_override_file_edge() {
        let w = wire(&[
            (
                "main.tf",
                "variable \"size\" {}\nlocals {\n  base = 1\n}\nresource \"aws_instance\" \"web\" {\n  ami = \"ami-1\"\n}\n",
            ),
            (
                "override.tf",
                "resource \"aws_instance\" \"web\" {\n  instance_type = var.size\n  ami           = local.base\n}\n",
            ),
        ]);
        assert_eq!(w.structs, ["var.size", "local.base", "aws_instance.web"]);
        assert_eq!(
            w.uses,
            [
                pair("aws_instance.web", "var.size"),
                pair("aws_instance.web", "local.base"),
            ]
        );
        assert_eq!(
            w.files,
            [(
                "aws_instance.web".to_string(),
                "override.tf".to_string(),
                "override"
            )]
        );
    }

    #[test]
    fn tfvars_keys_bind_to_variables_and_unknown_keys_are_unresolved() {
        let w = wire(&[
            ("main.tf", "variable \"region\" {}\n"),
            ("prod.tfvars", "region = \"us-east-1\"\nbogus = 1\n"),
        ]);
        assert_eq!(w.structs, ["var.region", "tfvars.prod.tfvars"]);
        assert_eq!(w.uses, [pair("tfvars.prod.tfvars", "var.region")]);
        assert_eq!(
            w.unresolved,
            [tri("tfvars.prod.tfvars", "var.bogus", "unknown")]
        );
    }

    #[test]
    fn local_module_source_links_every_base_and_override_file_of_the_target() {
        let w = wire(&[
            ("main.tf", "module \"net\" {\n  source = \"./net\"\n}\n"),
            ("net/main.tf", "variable \"cidr\" {}\n"),
            (
                "net/override.tf",
                "variable \"cidr\" {\n  default = \"10\"\n}\n",
            ),
        ]);
        let mut sources: Vec<_> = w
            .files
            .into_iter()
            .filter(|f| f.2 == "module-source")
            .collect();
        sources.sort();
        assert_eq!(
            sources,
            [
                (
                    "module.net".to_string(),
                    "net/main.tf".to_string(),
                    "module-source"
                ),
                (
                    "module.net".to_string(),
                    "net/override.tf".to_string(),
                    "module-source"
                ),
            ]
        );
    }

    #[test]
    fn parent_relative_module_source_normalises_across_directories() {
        let w = wire(&[
            ("a/main.tf", "module \"b\" {\n  source = \"../x\"\n}\n"),
            ("x/main.tf", "variable \"v\" {}\n"),
        ]);
        assert_eq!(
            w.files,
            [(
                "a::module.b".to_string(),
                "x/main.tf".to_string(),
                "module-source"
            )]
        );
        assert!(w.unresolved.is_empty());
    }

    #[test]
    fn missing_or_escaping_local_module_source_is_unknown_by_normalised_path() {
        let w = wire(&[(
            "main.tf",
            r#"
module "gone" {
  source = "./missing"
}
module "up" {
  source = "../../up"
}
"#,
        )]);
        assert_eq!(w.structs, ["module.gone", "module.up"]);
        assert_eq!(
            w.unresolved,
            [
                tri("module.gone", "missing", "unknown"),
                tri("module.up", "../../up", "unknown"),
            ]
        );
    }

    #[test]
    fn registry_git_and_absolute_module_sources_are_external() {
        let w = wire(&[(
            "main.tf",
            r#"
module "reg" {
  source = "hashicorp/consul/aws"
}
module "git" {
  source = "git::https://example.com/m.git"
}
module "abs" {
  source = "/abs/x"
}
"#,
        )]);
        assert_eq!(
            w.unresolved,
            [
                tri("module.reg", "hashicorp/consul/aws", "external"),
                tri("module.git", "git::https://example.com/m.git", "external"),
                tri("module.abs", "/abs/x", "external"),
            ]
        );
    }

    #[test]
    fn computed_module_source_is_a_nameless_unknown_at_its_byte_offset() {
        let text = "module \"m\" {\n  source = var.src\n}\n";
        let w = wire(&[("main.tf", text)]);
        let off = text.find("var.src").unwrap();
        assert_eq!(
            w.unresolved,
            [tri(
                "module.m",
                &format!("<nameless@main.tf:{off}>"),
                "unknown"
            )]
        );
    }

    #[test]
    fn test_file_runs_resolve_against_their_module_and_carry_module_sources() {
        let w = wire(&[
            ("main.tf", "variable \"region\" {}\n"),
            ("net/main.tf", "variable \"cidr\" {}\n"),
            (
                "tests/plan.tftest.hcl",
                r#"
run "plan" {
  variables {
    r = var.region
  }
}
run "net" {
  module {
    source = "./net"
  }
}
"#,
            ),
        ]);
        assert_eq!(
            w.structs,
            [
                "var.region",
                "test.tests/plan.tftest.hcl",
                "run.plan",
                "run.net",
                "net::var.cidr",
            ]
        );
        assert_eq!(w.uses, [pair("run.plan", "var.region")]);
        assert_eq!(
            w.files,
            [(
                "run.net".to_string(),
                "net/main.tf".to_string(),
                "module-source"
            )]
        );
    }

    #[test]
    fn shadowed_tf_contributes_no_structs_to_the_tofu_module() {
        let w = wire(&[
            ("main.tf", "variable \"x\" {}\n"),
            ("main.tofu", "variable \"y\" {}\n"),
        ]);
        assert_eq!(w.structs, ["var.y"]);
        assert!(w.diags.is_empty());
    }

    #[test]
    fn a_reference_does_not_reach_another_modules_declarations() {
        let w = wire(&[
            ("main.tf", "variable \"region\" {}\n"),
            (
                "net/main.tf",
                "resource \"a\" \"b\" {\n  x = var.region\n}\n",
            ),
        ]);
        assert_eq!(w.structs, ["var.region", "net::a.b"]);
        assert!(w.uses.is_empty());
        assert_eq!(w.unresolved, [tri("net::a.b", "var.region", "unknown")]);
    }
}
