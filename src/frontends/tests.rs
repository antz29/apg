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
}
