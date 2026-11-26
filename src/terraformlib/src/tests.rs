//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules (each starting with `use super::*;`), with any
//! shared fixtures and helpers at this root. e2e tests live outside this
//! tree in `tests/terraformlib_e2e.rs`, under `mod e2e`.

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
