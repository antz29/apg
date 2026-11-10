use super::*;
use crate::graph::NodeKind;
use crate::impact::{OverloadIndex, Signature, dep_index_rel};
use crate::testutil::located;
use std::collections::BTreeSet;

/// One persisted exported function signature — the shape
/// `impact::signatures_of_graph` writes into a `SignatureMap`.
fn persisted_sig(fqn: &str, parent: &str) -> Signature {
    Signature {
        kind: "function".to_string(),
        fqn: fqn.to_string(),
        params: Vec::new(),
        parent: parent.to_string(),
    }
}

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    #[test]
    fn language_of_maps_extensions() {
        assert_eq!(language_of("a.go"), "go");
        assert_eq!(language_of("A.java"), "java");
        assert_eq!(language_of("x.rs"), "rust");
        assert_eq!(language_of("x.tsx"), "ts");
        assert_eq!(language_of("x.cs"), "csharp");
        // Every C#-frontend-claimed extension lands in the csharp target set.
        assert_eq!(language_of("x.csx"), "csharp");
        assert_eq!(language_of("Views/Index.cshtml"), "csharp");
        assert_eq!(language_of("Pages/App.razor"), "csharp");
        // `.py`/`.pyi` classify under the scan-side id `py` (phase-08
        // task-23), so the win-B target filter matches them.
        assert_eq!(language_of("x.py"), "py");
        assert_eq!(language_of("x.pyi"), "py");
        assert_eq!(language_of("x.cpp"), "cpp");
        // Every extension `cpplib.is_cpp_ext` accepts classifies as cpp, so a
        // changed header/impl lands in the C++ target list (feedback-99).
        assert_eq!(language_of("x.cc"), "cpp");
        assert_eq!(language_of("x.cxx"), "cpp");
        assert_eq!(language_of("x.c++"), "cpp");
        assert_eq!(language_of("x.h"), "cpp");
        assert_eq!(language_of("x.hpp"), "cpp");
        assert_eq!(language_of("x.hh"), "cpp");
        assert_eq!(language_of("x.hxx"), "cpp");
        assert_eq!(language_of("x.tpp"), "cpp");
        assert_eq!(language_of("x.ipp"), "cpp");
        // COBOL sources and copybooks land in the `cobol` bucket in any case.
        assert_eq!(language_of("PAYROLL.cob"), "cobol");
        assert_eq!(language_of("ledger.cbl"), "cobol");
        assert_eq!(language_of("ledger.CBL"), "cobol");
        assert_eq!(language_of("x.cobol"), "cobol");
        assert_eq!(language_of("copy/REC.Cpy"), "cobol");
        assert_eq!(language_of("x.md"), "md");
        // A tracked `.opencode/agents/` definition is the agent frontend's
        // claim, so it lands in the agent stream's target set ahead of the
        // `.md` mapping; other Markdown stays on `md`.
        assert_eq!(language_of(".opencode/agents/implementer.md"), "agent");
        assert_eq!(language_of("docs/implementer.md"), "md");
        // apg-0.17.0 phase-03 task-17: the structural stream mappings mirror
        // the bundled scanner's `stream_for_path` taxonomy — extension-keyed
        // and filename-keyed — so each structural stream gets its own
        // changed-file target set.
        assert_eq!(language_of("run.sh"), "sh");
        assert_eq!(language_of("run.bash"), "sh");
        assert_eq!(language_of("run.zsh"), "sh");
        assert_eq!(language_of("ci.yaml"), "yaml");
        assert_eq!(language_of("ci.yml"), "yaml");
        assert_eq!(language_of("data.json"), "json");
        assert_eq!(language_of("package-lock.json"), "json");
        assert_eq!(language_of("config.toml"), "toml");
        assert_eq!(language_of("Cargo.lock"), "toml");
        assert_eq!(language_of("pom.xml"), "xml");
        assert_eq!(language_of("App.sln"), "sln");
        assert_eq!(language_of("Dockerfile"), "dockerfile");
        assert_eq!(language_of("app.dockerfile"), "dockerfile");
        assert_eq!(language_of("Makefile"), "makefile");
        assert_eq!(language_of("makefile"), "makefile");
        assert_eq!(language_of("GNUmakefile"), "makefile");
        assert_eq!(language_of("rules.mk"), "makefile");
        assert_eq!(language_of("app.ini"), "ini");
        assert_eq!(language_of("app.cfg"), "ini");
        assert_eq!(language_of("app.conf"), "ini");
        assert_eq!(language_of("build.properties"), "ini");
        assert_eq!(language_of(".editorconfig"), "ini");
        assert_eq!(language_of(".gitconfig"), "ini");
        assert_eq!(language_of(".env"), "ini");
        assert_eq!(language_of(".env.local"), "ini");
        // The residual — unknown extension, extension-less, dotfile, fixture
        // or binary — is the `misc` stream, matching the scanner's residual.
        assert_eq!(language_of("x.unknown"), "misc");
        assert_eq!(language_of("LICENSE"), "misc");
        assert_eq!(language_of(".gitignore"), "misc");
        assert_eq!(language_of("fixtures/blob.bin"), "misc");
    }

    #[test]
    fn function_params_flow_to_signatures() {
        let mut g = Graph::default();
        let mut n = located(NodeKind::Function, "/r/a.go");
        n.params = vec!["int".into()];
        g.nodes.insert("m.Leaf".into(), n);
        let sigs = signatures_of_graph(&g, Path::new("/r"));
        let set = &sigs["a.go"];
        let sig = set.iter().find(|s| s.fqn == "m.Leaf").unwrap();
        assert_eq!(sig.params, vec!["int".to_string()]);
    }

    #[test]
    fn absolute_joins_relative_only() {
        let root = Path::new("/r");
        assert_eq!(absolute(root, "src/a.go"), "/r/src/a.go");
        assert_eq!(absolute(root, "/abs/a.go"), "/abs/a.go");
    }

    /// The incremental path merges a target-only delta graph into the persisted
    /// `State` instead of rebuilding it: the targets' entries are replaced from
    /// the delta while the unchanged files' entries — including a
    /// `(dependent, target)` edge that still points at a surviving target —
    /// survive.
    #[test]
    fn merge_delta_replaces_target_entries() {
        let root = Path::new("/r");
        // The persisted pre-merge state. `a.go` and `gone.go` are the target
        // files: `a.go` is re-emitted (and keeps `b.go` as a dependency through
        // the unchanged file's signature), `gone.go` was deleted and the delta
        // graph no longer carries it.
        let mut signatures = std::collections::BTreeMap::new();
        signatures.insert(
            "a.go".to_string(),
            BTreeSet::from([persisted_sig("m.S.old", "m.S")]),
        );
        signatures.insert(
            "b.go".to_string(),
            BTreeSet::from([persisted_sig("m.S.peer", "m.S")]),
        );
        signatures.insert(
            "c.go".to_string(),
            BTreeSet::from([persisted_sig("m.c", "m")]),
        );
        signatures.insert(
            "gone.go".to_string(),
            BTreeSet::from([persisted_sig("m.gone", "m")]),
        );
        signatures.insert(
            "y.go".to_string(),
            BTreeSet::from([persisted_sig("m.y", "m")]),
        );

        let mut overloads = OverloadIndex::default();
        overloads.scopes.insert(
            "m.S".to_string(),
            vec!["m.S.old".to_string(), "m.S.peer".to_string()],
        );
        overloads.scopes.insert(
            "m".to_string(),
            vec!["m.c".to_string(), "m.gone".to_string(), "m.y".to_string()],
        );
        for (file, scope) in [
            ("a.go", "m.S"),
            ("b.go", "m.S"),
            ("c.go", "m"),
            ("gone.go", "m"),
            ("y.go", "m"),
        ] {
            overloads
                .file_scopes
                .insert(file.to_string(), BTreeSet::from([scope.to_string()]));
        }

        let mut state = State {
            // `a.go` referenced `c.go` before the edit; `y.go` referenced the
            // target `a.go` and the deleted `gone.go`.
            dep_edges_rel: vec![
                ("a.go".to_string(), "c.go".to_string()),
                ("y.go".to_string(), "a.go".to_string()),
                ("y.go".to_string(), "b.go".to_string()),
                ("y.go".to_string(), "gone.go".to_string()),
            ],
            signatures,
            overloads,
        };

        // The target-only delta graph: `a.go` re-emitted, now declaring
        // `m.S.new` under `m.S` and calling `m.S.peer` in the unchanged `b.go`
        // (which the delta graph does not carry); `gone.go` is absent.
        let mut delta = Graph::default();
        delta
            .nodes
            .insert("/r/a.go".to_string(), located(NodeKind::File, "/r/a.go"));
        delta.nodes.insert(
            "m.S.new".to_string(),
            located(NodeKind::Function, "/r/a.go"),
        );
        delta
            .calls
            .insert(("m.S.new".to_string(), "m.S.peer".to_string()));

        let targets: BTreeSet<String> = ["a.go".to_string(), "gone.go".to_string()].into();
        state.merge_delta(&delta, root, &targets);

        // `a.go`'s stale `(a.go, c.go)` edge is dropped and replaced by the
        // delta's `(a.go, b.go)` — resolved through the unchanged file's
        // signature. `(y.go, a.go)` survives because `a.go` is a target the
        // delta still carries; `(y.go, gone.go)` is dropped.
        assert_eq!(
            state.dep_edges_rel,
            vec![
                ("a.go".to_string(), "b.go".to_string()),
                ("y.go".to_string(), "a.go".to_string()),
                ("y.go".to_string(), "b.go".to_string()),
            ]
        );
        let merged = dep_index_rel(&state.dep_edges_rel);
        assert_eq!(merged.forward["a.go"], BTreeSet::from(["b.go".to_string()]));
        assert_eq!(merged.reverse["a.go"], BTreeSet::from(["y.go".to_string()]));

        // The target's signature entry is replaced from the delta; `b.go`'s
        // survives and the deleted target contributes none.
        assert_eq!(
            state.signatures["a.go"]
                .iter()
                .map(|s| s.fqn.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["m.S.new"])
        );
        assert!(state.signatures["b.go"].iter().any(|s| s.fqn == "m.S.peer"));
        assert!(!state.signatures.contains_key("gone.go"));

        // The touched `m.S` scope keeps its non-target member, drops the
        // retired FQN and gains the delta's — sorted. `m` (touched only through
        // the deleted target) loses just the retired FQN.
        assert_eq!(
            state.overloads.scopes["m.S"],
            vec!["m.S.new".to_string(), "m.S.peer".to_string()]
        );
        assert_eq!(
            state.overloads.scopes["m"],
            vec!["m.c".to_string(), "m.y".to_string()]
        );

        // `file_scopes`: the target's entry is replaced, the non-targets
        // survive, the deleted target contributes none.
        assert_eq!(
            state.overloads.file_scopes["a.go"],
            BTreeSet::from(["m.S".to_string()])
        );
        assert_eq!(
            state.overloads.file_scopes["b.go"],
            BTreeSet::from(["m.S".to_string()])
        );
        assert!(!state.overloads.file_scopes.contains_key("gone.go"));
    }
}
