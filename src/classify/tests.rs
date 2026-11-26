use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// fix-module-identity phase-03 task-6: the shared build-output
    /// exclusion predicate rejects every `.git/**` and `target/**` path
    /// (the observed self-ingestion offenders: the Go build cache under
    /// `.git/apg/facts` and the TS/staged frontends under `target/`) and
    /// accepts ordinary source — `.gitignore`, `targets/` and a
    /// `target_file` name are NOT build-output trees.
    #[test]
    fn build_output_predicate_rejects_git_and_target_trees() {
        for p in [
            ".git/apg/facts/go/key/53/abc-d",
            "/abs/repo/.git/index",
            "target/debug/frontends/tsfrontend/scanner.mjs",
            "/abs/repo/target/debug/x.rs",
            "nested/target/out.rs",
            "src\\.git\\x.go",
        ] {
            assert!(is_build_output_path(p), "{p} must be a build-output path");
        }
        for p in [
            "src/main.rs",
            "a/a.go",
            "targets/release.rs",
            "src/target_file.go",
            ".gitignore",
            "docs/.gitkeep",
            "",
        ] {
            assert!(!is_build_output_path(p), "{p} must stay authored source");
        }
    }

    /// cobol-frontend phase-02 task-18: a COBOL file is `test` under a
    /// test/tests segment or a test filename marker (matched
    /// case-insensitively), `generated` under gen/generated, `external` under
    /// vendor/third_party/thirdparty, and `src` otherwise (copybooks included).
    #[test]
    fn cobol_code_type_follows_segments_and_filename_markers() {
        for p in [
            "tests/payroll.cbl",
            "src/test/payroll.cob",
            "src/test_payroll.cbl",
            "src/Test_Payroll.CBL",
            "src/payroll_test.cbl",
            "src/payroll-test.cob",
            "src/payroll_TEST.cbl",
        ] {
            assert_eq!(builtin_code_type(p, "cobol"), "test", "{p} must be test");
        }
        for p in ["gen/payroll.cbl", "src/generated/payroll.cob"] {
            assert_eq!(
                builtin_code_type(p, "cobol"),
                "generated",
                "{p} must be generated"
            );
        }
        for p in [
            "vendor/lib/payroll.cbl",
            "third_party/payroll.cob",
            "thirdparty/payroll.cbl",
        ] {
            assert_eq!(
                builtin_code_type(p, "cobol"),
                "external",
                "{p} must be external"
            );
        }
        for p in [
            "src/payroll.cbl",
            "src/copy/PAYROLL.cpy",
            "src/contest.cbl",
            "src/testing.cbl",
            "src/latest-payroll.cbl",
        ] {
            assert_eq!(builtin_code_type(p, "cobol"), "src", "{p} must be src");
        }
    }

    /// sql-frontend phase-06 task-13: a SQL file's code type follows path
    /// segments only — test/tests → `test`, gen/generated/obj (SSDT build
    /// output) → `generated`, vendor/third_party/thirdparty → `external` —
    /// with no filename marker, and a test segment wins over obj.
    #[test]
    fn sql_code_type_follows_path_segments() {
        for (p, want) in [
            ("db/test/a.sql", "test"),
            ("tests/b.sql", "test"),
            ("Db/obj/test/x.sql", "test"),
            ("gen/c.sql", "generated"),
            ("generated/d.sql", "generated"),
            ("Db/obj/Debug/e.sql", "generated"),
            ("vendor/f.sql", "external"),
            ("third_party/g.sql", "external"),
            ("thirdparty/h.sql", "external"),
            ("test_x.sql", "src"),
            ("x_test.sql", "src"),
            ("Tables/Orders.sql", "src"),
        ] {
            assert_eq!(builtin_code_type(p, "sql"), want, "{p} must be {want}");
            assert_eq!(
                classify_code_type(p, "sql.x", "sql", None),
                want,
                "{p} must classify as {want}"
            );
        }
    }

    /// powershell-frontend phase-07 task-3: a PowerShell file's code type
    /// follows directory segments and the Pester `*.Tests.ps1` filename,
    /// case-insensitively; a segment check never matches the filename itself.
    #[test]
    fn ps_code_type_follows_segments_and_pester_filename() {
        for (p, want) in [
            ("Build.Tests.ps1", "test"),
            ("src/x/Mod.TESTS.PS1", "test"),
            ("test/a.ps1", "test"),
            ("Tests/b.psm1", "test"),
            ("a/TEST/c.psd1", "test"),
            ("gen/a.ps1", "generated"),
            ("Generated/b.psm1", "generated"),
            ("vendor/a.ps1", "external"),
            ("Third_Party/b.ps1", "external"),
            ("thirdparty/c.psd1", "external"),
            ("a.ps1", "src"),
            ("src/Tester.ps1", "src"),
            ("Mod.Tests.psm1", "src"),
            ("contest/a.ps1", "src"),
            ("test.ps1", "src"),
            ("vendor.psm1", "src"),
        ] {
            assert_eq!(builtin_code_type(p, "ps"), want, "{p} must be {want}");
        }
    }

    /// iac-frontends phase-01 task-2: a Terraform file is test under a
    /// test/tests segment or a `.tftest.hcl`/`.tofutest.hcl` filename,
    /// generated under gen/generated, external under vendor/.terraform, and
    /// `src` otherwise — matched case-insensitively.
    #[test]
    fn terraform_code_type_follows_spec() {
        for (p, want) in [
            ("test/main.tf", "test"),
            ("Tests/x.tf", "test"),
            ("tf/test/main.tofu", "test"),
            ("main.tftest.hcl", "test"),
            ("X.TOFUTEST.HCL", "test"),
            ("tests/vars.tfvars", "test"),
            ("gen/outputs.tf", "generated"),
            ("GENERATED/providers.hcl", "generated"),
            ("vendor/modules/main.tf.json", "external"),
            ("VENDOR/main.tf", "external"),
            (".terraform/modules/x/main.tf", "external"),
            ("third_party/stack.tf", "src"),
            ("thirdparty/import.tf", "src"),
            ("main.tf", "src"),
            ("src/variables.tf", "src"),
            ("env/main.tf.json", "src"),
            ("test_main.tf", "src"),
            ("testing/main.tf", "src"),
        ] {
            assert_eq!(
                builtin_code_type(p, "terraform"),
                want,
                "{p} must be {want}"
            );
            assert_eq!(
                classify_code_type(p, &format!("terraform.{p}"), "terraform", None),
                want,
                "{p} must classify as {want}"
            );
        }
    }

    /// iac-frontends phase-01 task-14: a Bicep file is test under a test/tests
    /// segment, generated under gen/generated, external under vendor/.terraform,
    /// and `src` otherwise — matched case-insensitively, no filename marker.
    #[test]
    fn bicep_code_type_follows_spec() {
        for (p, want) in [
            ("test/main.bicep", "test"),
            ("Tests/params.bicepparam", "test"),
            ("gen/module.bicep", "generated"),
            ("GEN/x.bicep", "generated"),
            ("generated/resources.bicepparam", "generated"),
            ("vendor/modules/network.bicep", "external"),
            (".terraform/storage.bicep", "external"),
            ("VENDOR/aks.bicep", "external"),
            ("third_party/storage.bicep", "src"),
            ("thirdparty/aks.bicep", "src"),
            ("main.bicep", "src"),
            ("src/variables.bicepparam", "src"),
            ("test_main.bicep", "src"),
            ("main.tftest.hcl", "src"),
        ] {
            assert_eq!(builtin_code_type(p, "bicep"), want, "{p} must be {want}");
            assert_eq!(
                classify_code_type(p, &format!("bicep.{p}"), "bicep", None),
                want,
                "{p} must classify as {want}"
            );
        }
    }

    /// apg-0.17.0 phase-01 task-18: with a config present, a STRUCTURAL
    /// record takes the config's structural `code_type` (default `config`)
    /// instead of falling through to `default`; `md` keeps its built-in
    /// `docs` classification; a configured `code_type` overrides the non-md
    /// default; and a code record is unchanged. schema-alignment phase-02
    /// task-15: an `agent` definition is a Markdown document, so it takes the
    /// same `docs` classification as `md` under a default config and under a
    /// config whose `structural.code_type` is set.
    #[test]
    fn structural_streams_take_the_configured_code_type() {
        let default_cfg = ApgConfig {
            default: "src".to_string(),
            types: Vec::new(),
            structural: Some(StructuralScope::default()),
        };
        for lang in [
            "sh",
            "yaml",
            "json",
            "toml",
            "xml",
            "dockerfile",
            "makefile",
            "ini",
            "misc",
        ] {
            assert_eq!(
                classify_code_type("a/file", "a/file", lang, Some(&default_cfg)),
                "config",
                "{lang} must default to `config`"
            );
        }
        assert_eq!(
            classify_code_type("AGENTS.md", "md.AGENTS.md", "md", Some(&default_cfg)),
            "docs",
            "Markdown keeps its built-in docs classification"
        );
        assert_eq!(
            classify_code_type(
                ".opencode/agents/implementer.md",
                ".opencode/agents/implementer.md",
                "agent",
                Some(&default_cfg)
            ),
            "docs",
            "an agent definition classifies like Markdown"
        );

        // A configured structural code_type overrides the non-md default,
        // but Markdown still keeps `docs`.
        let custom = ApgConfig {
            default: "src".to_string(),
            types: Vec::new(),
            structural: Some(StructuralScope {
                include: Vec::new(),
                exclude: Vec::new(),
                code_type: Some("cfg".to_string()),
            }),
        };
        assert_eq!(
            classify_code_type("ci.yml", "yaml.ci.yml", "yaml", Some(&custom)),
            "cfg"
        );
        assert_eq!(
            classify_code_type("AGENTS.md", "md.AGENTS.md", "md", Some(&custom)),
            "docs"
        );
        assert_eq!(
            classify_code_type(
                ".opencode/agents/implementer.md",
                ".opencode/agents/implementer.md",
                "agent",
                Some(&custom)
            ),
            "docs",
            "the structural override leaves agent definitions `docs`"
        );

        // A code stream is unchanged: it still falls through to `default`,
        // and with no config the built-in classifier is consulted.
        assert_eq!(
            classify_code_type("src/a.rs", "rust.a", "rust", Some(&custom)),
            "src"
        );
        assert_eq!(
            classify_code_type("src/a.rs", "rust.a", "rust", None),
            "src"
        );
    }
}
