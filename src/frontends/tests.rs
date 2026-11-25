use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// schema-alignment phase-02 task-16: the agent frontend's stream is
    /// merged with every other language's, and each frontend restarts its
    /// opaque-id counter at `n1`, so the agent prefix must be distinct from
    /// every other language's prefix and never the unknown-id fallback `x`.
    #[test]
    fn id_prefix_for_agent_is_distinct_and_non_fallback() {
        let prefix = id_prefix_for("agent");
        assert_ne!(
            prefix,
            id_prefix_for("not-a-language"),
            "the agent stream must not share the unknown-id fallback prefix"
        );
        for language in [
            "go",
            "java",
            "cpp",
            "rust",
            "ts",
            "js",
            "csharp",
            "md",
            "sh",
            "yaml",
            "json",
            "toml",
            "xml",
            "dockerfile",
            "makefile",
            "ini",
            "misc",
            "py",
        ] {
            assert_ne!(
                prefix,
                id_prefix_for(language),
                "the agent stream must not share {language}'s opaque-id prefix"
            );
        }
        assert!(
            !prefix.is_empty() && prefix.len() <= 3,
            "{prefix:?} is not a short opaque-id prefix"
        );
    }

    /// cobol-frontend phase-02 task-22: the cobol stream's opaque-id prefix is
    /// `cb`, distinct from every other language's, so a merged COBOL stream
    /// cannot collide ids; COBOL needs no scan-time tool on PATH.
    #[test]
    fn id_prefix_for_cobol_is_distinct_and_tool_less() {
        let prefix = id_prefix_for("cobol");
        assert_eq!(prefix, "cb");
        for language in [
            "go",
            "java",
            "cpp",
            "rust",
            "ts",
            "js",
            "csharp",
            "md",
            "sh",
            "yaml",
            "json",
            "toml",
            "xml",
            "dockerfile",
            "makefile",
            "ini",
            "misc",
            "py",
            "agent",
        ] {
            assert_ne!(prefix, id_prefix_for(language), "cobol vs {language}");
        }
        assert!(scan_time_tools("cobol").is_empty());
    }

    /// csharp-projects phase-03 task-13: every structural stream id (`sln`
    /// included) is a language root and classified structural; the `sln`
    /// stream's opaque-id prefix collides with no other root's nor the `x`
    /// fallback, and it needs no scan-time tool on PATH.
    #[test]
    fn structural_languages_are_roots_and_sln_is_distinct_and_tool_less() {
        let roots = crate::layers::LANGUAGE_ROOTS;
        for language in STRUCTURAL_LANGUAGES {
            assert!(roots.contains(language), "{language} is not a root");
            assert!(
                classify::is_structural_language(language),
                "{language} is not classified structural"
            );
        }
        let prefix = id_prefix_for("sln");
        assert_ne!(prefix, id_prefix_for("not-a-language"), "sln vs fallback");
        for language in roots.iter().filter(|l| **l != "sln") {
            assert_ne!(prefix, id_prefix_for(language), "sln vs {language}");
        }
        assert!(scan_time_tools("sln").is_empty());
    }

    /// cobol-frontend phase-02 task-22: only the COBOL candidate matches its
    /// extensions in any letter case; every other language stays exact-case,
    /// so `Main.JAVA` is not a java source.
    #[test]
    fn ext_matches_is_case_insensitive_only_when_asked() {
        let cobol = [".cob", ".cbl", ".cobol", ".cpy"];
        assert!(ext_matches("CBL", &cobol, true));
        assert!(ext_matches("Cpy", &cobol, true));
        assert!(!ext_matches("CBL", &cobol, false));
        let java = [".java"];
        assert!(ext_matches("java", &java, false));
        assert!(!ext_matches("JAVA", &java, false));
    }

    /// sql-frontend phase-06 task-19: `sql` is a language root whose `sq`
    /// opaque-id prefix collides with no other root's nor the `x` fallback, it
    /// needs no scan-time tool on PATH, and `.sql` matches in any letter case
    /// only when the candidate asks for it.
    #[test]
    fn sql_registration_is_distinct_tool_less_and_case_insensitive() {
        let roots = crate::layers::LANGUAGE_ROOTS;
        assert!(roots.contains(&"sql"), "sql is not a language root");
        let prefix = id_prefix_for("sql");
        assert_eq!(prefix, "sq");
        assert_ne!(prefix, id_prefix_for("not-a-language"), "sql vs fallback");
        for language in roots.iter().filter(|l| **l != "sql") {
            assert_ne!(prefix, id_prefix_for(language), "sql vs {language}");
        }
        assert!(scan_time_tools("sql").is_empty());
        assert!(ext_matches("SQL", &[".sql"], true));
        assert!(ext_matches("Sql", &[".sql"], true));
        assert!(!ext_matches("SQL", &[".sql"], false));
    }

    /// powershell-frontend phase-07 task-10: `ps` is a language root whose
    /// `ps` opaque-id prefix collides with no other root's (sql's `sq`
    /// included) nor the `x` fallback, it needs no scan-time tool on PATH,
    /// uses the default `--targets` hand-off, and matches `.ps1`/`.psm1`/
    /// `.psd1` in any letter case.
    #[test]
    fn ps_registration_is_distinct_tool_less_and_case_insensitive() {
        let roots = crate::layers::LANGUAGE_ROOTS;
        assert!(roots.contains(&"ps"), "ps is not a language root");
        let prefix = id_prefix_for("ps");
        assert_eq!(prefix, "ps");
        assert_ne!(prefix, id_prefix_for("not-a-language"), "ps vs fallback");
        for language in roots.iter().filter(|l| **l != "ps") {
            assert_ne!(prefix, id_prefix_for(language), "ps vs {language}");
        }
        assert!(prefix.len() <= 3, "{prefix:?} is not a short prefix");
        assert!(scan_time_tools("ps").is_empty());
        assert!(!passes_targets_flag("ps", false));
        let ps = [".ps1", ".psm1", ".psd1"];
        assert!(ext_matches("PS1", &ps, true));
        assert!(ext_matches("Psm1", &ps, true));
        assert!(ext_matches("psd1", &ps, true));
        assert!(!ext_matches("ps", &ps, true));
        assert!(!ext_matches("ps1xml", &ps, true));
    }

    /// sql-frontend phase-06 task-30: `sql` always carries `--targets` (its
    /// list is its emission universe); every other language only when enabled.
    #[test]
    fn passes_targets_flag_is_always_on_for_sql() {
        assert!(passes_targets_flag("sql", false));
        assert!(passes_targets_flag("sql", true));
        assert!(!passes_targets_flag("go", false));
        assert!(!passes_targets_flag("xml", false));
        assert!(!passes_targets_flag("csharp", false));
        assert!(passes_targets_flag("go", true));
    }

    /// sql-frontend phase-06 task-20: any `.sql`/`.sqlproj` change (any case)
    /// widens to every tracked `.sql` path; the sql target list carries `.sql`
    /// and `.sqlproj` paths only, while `.sqlproj` stays in the xml list too.
    #[test]
    fn sql_targets_cover_the_whole_stream() {
        use std::collections::BTreeSet;
        let set =
            |paths: &[&str]| -> BTreeSet<String> { paths.iter().map(|p| p.to_string()).collect() };
        let tracked = set(&[
            "Db/Tables/A.sql",
            "Db/Views/B.SQL",
            "scripts/c.Sql",
            "Db/Db.sqlproj",
            "main.go",
            "App/App.cs",
            "pom.xml",
        ]);
        let all_sql = set(&["Db/Tables/A.sql", "Db/Views/B.SQL", "scripts/c.Sql"]);
        for changed in [
            set(&["Db/Views/B.SQL"]),
            set(&["Db/Db.sqlproj"]),
            set(&["Other/Other.SQLPROJ"]),
        ] {
            assert_eq!(
                whole_language_targets(&changed, &tracked),
                all_sql,
                "{changed:?}"
            );
        }
        let non_sql = set(&["main.go", "App/App.cs", "pom.xml"]);
        assert!(whole_language_targets(&non_sql, &tracked).is_empty());

        let root = Path::new("/root");
        let abs = |rel: &str| root.join(rel).to_string_lossy().into_owned();
        let mut expected: Vec<String> = [
            "Db/Tables/A.sql",
            "Db/Views/B.SQL",
            "scripts/c.Sql",
            "Db/Db.sqlproj",
        ]
        .iter()
        .map(|r| abs(r))
        .collect();
        expected.sort();
        let sql = targets_for_language(&tracked, root, "sql");
        assert_eq!(sql, expected);
        assert!(!sql.contains(&abs("main.go")));
        assert!(!targets_for_language(&set(&["Db/Db.sqlproj"]), root, "sql").is_empty());
        let xml = targets_for_language(&tracked, root, "xml");
        assert!(xml.contains(&abs("Db/Db.sqlproj")));
        let go = targets_for_language(&tracked, root, "go");
        for rel in all_sql.iter() {
            assert!(!go.contains(&abs(rel)), "{rel} reached go");
            assert!(!xml.contains(&abs(rel)), "{rel} reached xml");
        }
    }

    /// sql-frontend phase-06 task-29: the sqlfrontend `--targets` universe is
    /// every manifest `.sql`/`.sqlproj` path (any case), absolute and sorted;
    /// non-sql keys contribute nothing.
    #[test]
    fn sql_stream_targets_list_every_tracked_sql_file() {
        use std::collections::BTreeSet;
        let set =
            |paths: &[&str]| -> BTreeSet<String> { paths.iter().map(|p| p.to_string()).collect() };
        let root = Path::new("/root");
        let keys = set(&[
            "Db/Tables/A.sql",
            "Db/Views/B.SQL",
            "scripts/c.Sql",
            "Db/Db.sqlproj",
            "Legacy/L.SQLPROJ",
            "main.go",
            "Db/x.xml",
        ]);
        let expected: Vec<String> = [
            "Db/Db.sqlproj",
            "Db/Tables/A.sql",
            "Db/Views/B.SQL",
            "Legacy/L.SQLPROJ",
            "scripts/c.Sql",
        ]
        .iter()
        .map(|rel| root.join(rel).to_string_lossy().into_owned())
        .collect();
        assert_eq!(sql_stream_targets(keys.iter(), root), expected);
        assert!(sql_stream_targets(set(&[]).iter(), root).is_empty());
        assert!(sql_stream_targets(set(&["main.go", "Db/x.xml"]).iter(), root).is_empty());
    }

    /// schema-alignment phase-02 task-17: the agent frontend is self-contained
    /// (it reads `.opencode/agents/` itself), so its scan-time preflight set is
    /// empty through the explicit tool-less `agent` arm beside the structural
    /// ids, while a tool-requiring language such as `go` still reports its tool.
    #[test]
    fn scan_time_tools_agent_is_tool_less() {
        assert!(
            scan_time_tools("agent").is_empty(),
            "the agent frontend is self-contained and needs nothing on PATH"
        );
        // The explicit arm sits beside the structural streams' tool-less arm,
        // so the agent stream's exemption matches theirs.
        for structural in [
            "md",
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
                scan_time_tools("agent"),
                scan_time_tools(structural),
                "the agent frontend's preflight set must match {structural}'s"
            );
        }
        assert_eq!(
            scan_time_tools("go"),
            &["go"],
            "a tool-requiring language must still report its scan-time tool"
        );
    }

    /// module-flag-resolution phase-01 task-18: `honours_module_flag` admits a
    /// shipped code frontend, the agent frontend and a structural stream, and
    /// refuses a language the driver ships no frontend for — so a `--module`
    /// selection can never spawn a frontend that would accept and ignore the
    /// flag. The pure `module_flag_error` builder names that language and every
    /// requested value, so the operator can correct the selection.
    #[test]
    fn module_flag_honour_admits_shipped_and_refuses_unshipped() {
        for shipped in ["go", "agent", "md"] {
            assert!(
                honours_module_flag(shipped),
                "the shipped `{shipped}` frontend must honour --module"
            );
        }
        // `ruby` has no frontend in the driver's registry and is not a
        // structural stream id, so it is a genuinely unshipped refusal arm.
        assert!(
            !classify::is_structural_language("ruby"),
            "the refusal arm must not be a structural stream id"
        );
        assert!(
            !honours_module_flag("ruby"),
            "a language with no shipped frontend must refuse --module"
        );

        let message = module_flag_error("ruby", &["pkg/one".to_string(), "pkg/two".to_string()]);
        assert!(message.contains("`ruby`"), "{message}");
        assert!(message.contains("`pkg/one`"), "{message}");
        assert!(message.contains("`pkg/two`"), "{message}");
    }
}
