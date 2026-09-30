use super::*;
/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// Phase-7 task-7 (gate): the `apg --help` text documents the strict
    /// `add|update|rm` surface for node/edge/plan, no longer names the retired
    /// `plan init`/`plan link` verbs, and leaves the `apg review` line
    /// unchanged.
    #[test]
    fn help_text_documents_strict_surface_and_preserves_review() {
        let help = help_text();
        // node/edge/plan all document add|update|rm.
        assert!(help.contains("apg node <sub>"), "node block present");
        assert!(help.contains("apg edge <sub>"), "edge block present");
        assert!(help.contains("apg session <sub>"), "session block present");
        assert!(
            help.contains("single-writer coordinator"),
            "the session block documents the coordinator"
        );
        // phase-04 task-11: the session block pins all four lifecycle entries —
        // start, save, abort, end — so the save/abort additions travel with the
        // pre-existing start/end documentation.
        let session_block = help
            .lines()
            .skip_while(|l| !l.contains("apg session <sub>"))
            .take_while(|l| !l.contains("apg --version"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            session_block.contains("start — own db.lbug exclusively"),
            "the session block documents `start`: {session_block}"
        );
        assert!(
            session_block.contains("save — make the buffered node-file set durable"),
            "the session block documents `save`: {session_block}"
        );
        assert!(
            session_block.contains("abort — discard the buffered node-file set"),
            "the session block documents `abort`: {session_block}"
        );
        assert!(
            session_block.contains("end — signal the live session"),
            "the session block documents `end`: {session_block}"
        );
        assert!(help.contains("apg plan <sub>"), "plan block present");
        assert!(
            help.contains("add/update/rm/done/undone/note/complete/render/verify"),
            "the plan subcommand surface names add/update/rm progress verbs"
        );
        assert!(
            help.contains("add/update/rm (type-as-argument"),
            "the node surface names add/update/rm"
        );
        assert!(
            help.contains("add/update/rm (kind/from/to"),
            "the edge surface names add/update/rm"
        );
        // The retired plan verbs are gone.
        assert!(!help.contains("plan init"), "plan init is retired: {help}");
        assert!(!help.contains("plan link"), "plan link is retired: {help}");
        // The review block is untouched: the same header + the exact five-verb
        // dispatch line.
        let review_block = help
            .lines()
            .skip_while(|l| !l.contains("apg review <sub>"))
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            review_block.contains("Writer↔reviewer feedback cycle:"),
            "the apg review header must be unchanged: {review_block}"
        );
        assert!(
            review_block.contains("add/action/resolve/reject/list"),
            "the apg review verbs must be unchanged: {review_block}"
        );
    }

    /// Phase-03 task-5: the per-language spawn verdict. On the incremental
    /// path, in the PARTIAL case (the scan has other changed languages) a
    /// language whose target set is empty is SKIPPED entirely (no process
    /// spawn), while a language with targets spawns. When the whole target set
    /// is empty phase-02's unfiltered path runs every language, and on a full
    /// scan every language spawns. The verdict is derived from the SAME
    /// phase-2 `targets_rel` set that drives the win-C DB splice, via
    /// `targets_for_language`, never a fresh detection walk.
    #[test]
    fn spawn_verdict_skips_unchanged_languages_only_on_the_incremental_path() {
        // A partial (win-C) delta: go changed, ts untouched.
        let mut rel = BTreeSet::new();
        rel.insert("a/a.go".to_string());
        let go_targets = targets_for_language(&rel, Path::new("/root"), "go");
        let ts_targets = targets_for_language(&rel, Path::new("/root"), "ts");
        let any = !rel.is_empty();

        // Incremental PARTIAL case: the changed language spawns; the unchanged
        // one is skipped entirely (not merely emission-filtered).
        assert!(should_spawn_language(
            false,
            go_targets.is_empty(),
            any,
            false
        ));
        assert!(!should_spawn_language(
            false,
            ts_targets.is_empty(),
            any,
            false
        ));

        // Incremental with NO targets anywhere: phase-02's unfiltered path runs
        // every language (an empty target file means "no filter"), so the
        // frontends still emit their global module scaffolding + full universe.
        assert!(should_spawn_language(false, true, false, false));
        assert!(should_spawn_language(false, false, false, false));

        // Full scan: every detected/requested language still spawns, even with
        // an empty target set (no emission filter is passed on this path).
        assert!(should_spawn_language(true, true, false, false));
        assert!(should_spawn_language(
            true,
            go_targets.is_empty(),
            any,
            false
        ));

        // phase-06 task-2: a COMPLETE warm cache skips EVERY language even on
        // the empty-target case phase-02's unfiltered path would otherwise
        // spawn — the recorded facts + scaffolding reconstruct the graph.
        assert!(!should_spawn_language(false, true, false, true));
        assert!(!should_spawn_language(false, false, false, true));
        // A full scan still spawns every language (the warm flag never
        // overrides the correctness full-scan gate).
        assert!(should_spawn_language(true, true, false, true));

        // Phase-08 task-23: the `py` scan id receives its `.py`/`.pyi`
        // targets — `language_of` renders the scan-side token `py`, not the
        // stale `python`, so the filter matches.
        let mut py_rel = BTreeSet::new();
        py_rel.insert("pkg/a.py".to_string());
        py_rel.insert("pkg/stub.pyi".to_string());
        assert_eq!(
            targets_for_language(&py_rel, Path::new("/root"), "py"),
            vec![
                "/root/pkg/a.py".to_string(),
                "/root/pkg/stub.pyi".to_string(),
            ],
            "the py scan id must receive its .py/.pyi targets"
        );
        assert!(
            targets_for_language(&py_rel, Path::new("/root"), "go").is_empty(),
            "a py target must not be delivered to another language"
        );

        // Phase-08 task-24: the unified js/ts artifact is ONE `language_of`
        // bucket under TWO scan ids. A JS-only repo scans under `js`, and a
        // TS-containing repo under `ts`; both must receive the js-family
        // targets even though `language_of` renders the single token `ts`.
        let mut js_rel = BTreeSet::new();
        js_rel.insert("web/app.js".to_string());
        js_rel.insert("web/widget.jsx".to_string());
        js_rel.insert("web/esm.mjs".to_string());
        js_rel.insert("web/cjs.cjs".to_string());
        js_rel.insert("web/typed.ts".to_string());
        for id in ["js", "ts"] {
            let got = targets_for_language(&js_rel, Path::new("/root"), id);
            for suffix in ["web/app.js", "web/widget.jsx", "web/esm.mjs", "web/cjs.cjs"] {
                assert!(
                    got.iter().any(|p| p.ends_with(suffix)),
                    "the `{id}` scan id must receive its {suffix} target: {got:?}"
                );
            }
        }
        assert!(
            targets_for_language(&js_rel, Path::new("/root"), "go").is_empty(),
            "a js/ts target must not be delivered to another language"
        );
    }

    /// Phase-03 task-5: the pure scan-time toolchain mapping and the named,
    /// actionable error text. The mapping is exactly the per-language
    /// scan-time tool set (`go`; a JDK's `java`; `cargo` + `rustc`; `node`
    /// for both `ts` and `js`) and nothing for the four self-contained
    /// frontends; the error names the language, the missing tool, and its
    /// install hint — no panic, no I/O.
    #[test]
    fn scan_time_tools_and_error_text() {
        assert_eq!(scan_time_tools("go"), &["go"][..], "go needs the go tool");
        assert_eq!(scan_time_tools("java"), &["java"][..], "java needs a JDK");
        assert_eq!(
            scan_time_tools("rust"),
            &["cargo", "rustc"][..],
            "rust needs cargo + rustc"
        );
        assert_eq!(scan_time_tools("ts"), &["node"][..], "ts needs node");
        assert_eq!(scan_time_tools("js"), &["node"][..], "js needs node");
        for lang in ["cpp", "csharp", "py", "md"] {
            assert!(
                scan_time_tools(lang).is_empty(),
                "{lang} is self-contained at scan time"
            );
        }
        assert!(
            scan_time_tools("unknown-lang").is_empty(),
            "an unknown language never demands a tool"
        );

        let go = scan_time_tool_error("go", "go");
        assert!(go.contains("`go` frontend"), "names the language: {go}");
        assert!(go.contains("required tool `go`"), "names the tool: {go}");
        assert!(go.contains("brew install go"), "carries the hint: {go}");

        let rust = scan_time_tool_error("rust", "cargo");
        assert!(rust.contains("`cargo`"), "names the tool: {rust}");
        assert!(
            rust.contains("brew install rust"),
            "carries the hint: {rust}"
        );

        let java = scan_time_tool_error("java", "java");
        assert!(java.contains("JDK"), "carries the JDK hint: {java}");

        let node = scan_time_tool_error("ts", "node");
        assert!(node.contains("`node`"), "names node: {node}");
        assert!(
            node.contains("brew install node"),
            "carries the hint: {node}"
        );
    }

    /// The discovered-work protocol: implementation-discovered work is
    /// re-planned (a planned node plus a `creates` task) before it is
    /// implemented. The embedded coordinator/authoring prompts must carry it
    /// so the rule cannot be silently dropped from the distributed agents.
    #[test]
    fn discovered_work_is_replanned_before_it_is_implemented() {
        fn agent(name: &str) -> &'static str {
            AGENTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("{name} is in the embedded AGENTS set"))
        }
        assert!(
            agent("codebase-navigator.md")
                .contains("discovered work is planned before it is implemented"),
            "the coordinator prompt must state the discovered-work order"
        );
        assert!(
            agent("agent-builder.md").contains("stops before editing"),
            "the agent-builder template must carry the stop-and-report clause"
        );
        assert!(
            agent("plan-writer.md").contains("declared before the code exists"),
            "the plan-writer prompt must carry the planned-before-code rule"
        );
        assert!(
            agent("spec-writer.md").contains("divergence discovered during implementation"),
            "the spec-writer prompt must carry the reconciliation route for discovered divergence"
        );
    }

    /// Phase-06 task-10: `builtin_code_type`'s JS rules are **extension-
    /// keyed, not stream-id-keyed** — the bundle-filename rule fires under
    /// BOTH the `ts` id (a mixed/TS-detected repo scans its incidental
    /// JavaScript under `ts`) and the `js` id. A bare `.cjs` suffix is never
    /// a bundle marker; only a real `min`/`bundle` filename infix (or a
    /// `gen|generated|dist|build|out` path segment) classifies generated.
    ///
    /// `classify_code_type` is metadata-only — it never drops a node — so
    /// every case additionally asserts the file stays a recognised in-graph
    /// code type (present, never filtered out).
    #[test]
    fn builtin_code_type_js_rules_are_extension_keyed() {
        let cases: &[(&str, &str)] = &[
            // Bundle filename markers: generated under both ids.
            ("/proj/src/app.min.js", "generated"),
            ("/proj/dist/app.min.js", "generated"),
            ("/proj/build/app.bundle.js", "generated"),
            ("/proj/src/app.min.jsx", "generated"),
            ("/proj/src/widget.min.mjs", "generated"),
            ("/proj/src/widget.bundle.mjs", "generated"),
            ("/proj/dist/app.min.cjs", "generated"),
            ("/proj/dist/app.bundle.cjs", "generated"),
            // A bare `.cjs`/`.js`/`.mjs` suffix is NEVER generated.
            ("/proj/src/index.js", "src"),
            ("/proj/src/util.mjs", "src"),
            ("/proj/src/index.cjs", "src"),
            ("/proj/src/x.cjs", "src"),
            ("/proj/src/widget.jsx", "src"),
            // test/vendor mirror the ts arm.
            ("/proj/tests/app.js", "test"),
            ("/proj/vendor/app.mjs", "external"),
        ];
        for (path, expected) in cases {
            for id in ["ts", "js"] {
                let got = classify::classify_code_type(path, path, id, None);
                assert_eq!(
                    got, *expected,
                    "{id} id: {path} must classify {expected}, got {got}"
                );
                assert!(
                    ["src", "test", "generated", "external"].contains(&got.as_str()),
                    "{id} id: {path} must stay an in-graph code type (never dropped), got {got}"
                );
            }
        }
        // The `js` arm also recognises the JS filename test suffixes
        // (`*.test.js`/`*.spec.js` and the other JS extensions).
        for (path, expected) in [
            ("/proj/src/app.test.js", "test"),
            ("/proj/src/app.spec.mjs", "test"),
            ("/proj/src/app_test.cjs", "test"),
        ] {
            assert_eq!(
                classify::classify_code_type(path, path, "js", None),
                expected,
                "js id: {path} must classify {expected}"
            );
        }
    }

    /// Phase-08 task-16: the `py` code-type arm. An ordinary module is
    /// `src`; a `*_test.py` (or `test_*.py`, or a `test/` segment) is
    /// `test`; a hand-written `.pyi` stub is `src` — NEVER `generated`
    /// (the `.d.ts` analog, a declaration is authored source) — UNLESS it
    /// sits under a `gen`/`generated` tree, which is `generated`; a
    /// `third_party/` file is `external`. `classify_code_type` is
    /// metadata-only — it never drops a node — so every case additionally
    /// asserts the file stays a recognised in-graph code type.
    #[test]
    fn builtin_code_type_py_rules_are_path_and_stem_keyed() {
        let cases: &[(&str, &str)] = &[
            ("/proj/pkg/mod.py", "src"),
            ("/proj/pkg/__init__.py", "src"),
            ("/proj/src/app.py", "src"),
            // Hand-written stubs are authored source, never generated.
            ("/proj/pkg/mod.pyi", "src"),
            ("/proj/stubs/pkg/mod.pyi", "src"),
            // A stub under a generated tree IS generated.
            ("/proj/gen/pkg/mod.pyi", "generated"),
            ("/proj/generated/pkg/mod.py", "generated"),
            // Test naming conventions and test trees.
            ("/proj/pkg/mod_test.py", "test"),
            ("/proj/test_helpers.py", "test"),
            ("/proj/tests/mod.py", "test"),
            // Dependency trees are external.
            ("/proj/third_party/lib/mod.py", "external"),
            ("/proj/vendor/lib/mod.py", "external"),
        ];
        for (path, expected) in cases {
            let got = classify::classify_code_type(path, path, "py", None);
            assert_eq!(got, *expected, "py id: {path} must classify {expected}");
            assert!(
                ["src", "test", "generated", "external"].contains(&got.as_str()),
                "py id: {path} must stay an in-graph code type (never dropped), got {got}"
            );
        }
    }
}

// -----------------------------------------------------------------------
