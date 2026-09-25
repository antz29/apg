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

    /// apg-0.17.0 phase-01 task-18: with a config present, a STRUCTURAL
    /// record takes the config's structural `code_type` (default `config`)
    /// instead of falling through to `default`; `md` keeps its built-in
    /// `docs` classification; a configured `code_type` overrides the non-md
    /// default; and a code record is unchanged.
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
