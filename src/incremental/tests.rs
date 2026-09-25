use super::*;
use crate::graph::NodeKind;
use crate::testutil::located;

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
        assert_eq!(language_of("x.md"), "md");
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
}
