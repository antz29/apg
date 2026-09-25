//! Relocated e2e tests for the `pyfrontend` frontend crate (from
//! `src/lib.rs`'s inline `#[cfg(test)] mod tests`). Real I/O only; every test
//! is `#[ignore]`d and runs through `cargo test-e2e`
//! (= `cargo test e2e:: -- --ignored`).

mod common;

use common::{
    edge_target, has_edge, module_fqns, node_id, records, run_frontend, scratch_dir, write_file,
};

/// e2e tier -- real I/O: every test here writes a scratch `/tmp` fixture
/// tree and spawns the built `pyfrontend` over it. Each is `#[ignore]`d, so a
/// plain `cargo test` never runs one; the only entry point is the named guard
/// `cargo test-e2e`.
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn discovers_package_identity_files_and_declarations() {
        let root = scratch_dir("discover");
        write_file(&root, "pkg/__init__.py", "");
        write_file(
            &root,
            "pkg/mod.py",
            "class Base:\n    pass\n\n\ndef helper(x):\n    return x\n",
        );
        write_file(&root, "flat.py", "def standalone():\n    return 1\n");

        let recs = records(&run_frontend(&root, &[]));

        let modules = module_fqns(&recs);
        assert!(
            modules.contains("pkg"),
            "missing package module: {modules:?}"
        );
        assert!(
            modules.contains("pkg.mod"),
            "missing submodule: {modules:?}"
        );
        assert!(
            modules.contains("flat"),
            "missing flat module identity: {modules:?}"
        );

        let files: Vec<&serde_json::Value> = recs.iter().filter(|r| r["type"] == "file").collect();
        let parent_of = |suffix: &str| -> Option<String> {
            files
                .iter()
                .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with(suffix)))
                .and_then(|r| r["parent"].as_str().map(str::to_string))
        };
        assert_eq!(parent_of("pkg/__init__.py").as_deref(), Some("pkg"));
        assert_eq!(parent_of("pkg/mod.py").as_deref(), Some("pkg.mod"));
        assert_eq!(parent_of("flat.py").as_deref(), Some("flat"));

        assert!(
            node_id(&recs, "struct", "pkg.mod", "Base").is_some(),
            "class declaration missing"
        );
        assert!(
            node_id(&recs, "function", "pkg.mod", "helper").is_some(),
            "function declaration missing"
        );
        assert!(
            node_id(&recs, "function", "flat", "standalone").is_some(),
            "flat module function missing"
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn namespace_packages_fold_directory_components_into_module_identity() {
        let root = scratch_dir("namespace");
        // PEP-420 namespace packages: no `__init__.py`, same-stem modules.
        write_file(&root, "namespace_a/mod.py", "def a():\n    return 1\n");
        write_file(&root, "namespace_b/mod.py", "def b():\n    return 2\n");
        // A regular (init-bearing) package, including a namespace dir
        // nested inside it.
        write_file(&root, "regular/__init__.py", "");
        write_file(&root, "regular/mod.py", "def regular():\n    return 3\n");
        write_file(&root, "regular/ns/mod.py", "def nested():\n    return 4\n");

        let recs = records(&run_frontend(&root, &[]));

        let modules = module_fqns(&recs);
        for expected in [
            "namespace_a",
            "namespace_a.mod",
            "namespace_b",
            "namespace_b.mod",
            "regular",
            "regular.mod",
            "regular.ns",
            "regular.ns.mod",
        ] {
            assert!(
                modules.contains(expected),
                "module {expected} missing: {modules:?}"
            );
        }
        assert!(
            !modules.contains("mod"),
            "same-stem namespace modules must not collapse to the bare stem: {modules:?}"
        );

        let files: Vec<&serde_json::Value> = recs.iter().filter(|r| r["type"] == "file").collect();
        let parent_of = |suffix: &str| -> Option<String> {
            files
                .iter()
                .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with(suffix)))
                .and_then(|r| r["parent"].as_str().map(str::to_string))
        };
        assert_eq!(
            parent_of("namespace_a/mod.py").as_deref(),
            Some("namespace_a.mod")
        );
        assert_eq!(
            parent_of("namespace_b/mod.py").as_deref(),
            Some("namespace_b.mod")
        );
        assert_eq!(
            parent_of("regular/__init__.py").as_deref(),
            Some("regular"),
            "an init-bearing package identity is unchanged"
        );
        assert_eq!(parent_of("regular/mod.py").as_deref(), Some("regular.mod"));
        assert_eq!(
            parent_of("regular/ns/mod.py").as_deref(),
            Some("regular.ns.mod")
        );

        assert!(
            node_id(&recs, "function", "namespace_a.mod", "a").is_some(),
            "namespace_a declaration missing"
        );
        assert!(
            node_id(&recs, "function", "namespace_b.mod", "b").is_some(),
            "namespace_b declaration missing"
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over scratch temp dirs; run via cargo test-e2e"]
    fn module_identity_is_repo_base_relative_and_scan_root_independent() {
        // `root` is a git toplevel; the same file is scanned once as the
        // whole repo and once from the `pkg` subdirectory. The identity
        // boundary is the repo base (the git toplevel), never the scan
        // root, so both scans must mint the same dotted identities
        // (`requirements.requirement.portable-graph-identity` AC2+AC4).
        let root = scratch_dir("repo-base");
        std::fs::create_dir_all(root.join(".git")).expect("git toplevel marker");
        write_file(&root, "pkg/__init__.py", "");
        write_file(&root, "pkg/mod.py", "def f():\n    return 1\n");
        // A PEP-420 namespace sibling (no `__init__.py`).
        write_file(&root, "ns/mod.py", "def g():\n    return 2\n");

        let repo_recs = records(&run_frontend(&root, &[]));
        let sub_recs = records(&run_frontend(&root.join("pkg"), &[]));

        let repo_modules = module_fqns(&repo_recs);
        let sub_modules = module_fqns(&sub_recs);

        // Repo-root scan: identities are relative to the git toplevel.
        for expected in ["pkg", "pkg.mod", "ns", "ns.mod"] {
            assert!(
                repo_modules.contains(expected),
                "repo scan missing module {expected}: {repo_modules:?}"
            );
        }
        assert!(
            !repo_modules.contains("mod"),
            "same-stem modules must not collapse to the bare stem: {repo_modules:?}"
        );

        // Subdir scan of `pkg`: the SAME identities. The subdirectory is
        // NOT the boundary, so the identity keeps its `pkg` prefix instead
        // of regressing to the scan-root-relative bare stem `mod`.
        assert!(
            sub_modules.contains("pkg"),
            "a subdir scan must keep the pkg package identity: {sub_modules:?}"
        );
        assert!(
            sub_modules.contains("pkg.mod"),
            "a subdir scan must keep pkg.mod: {sub_modules:?}"
        );
        assert!(
            !sub_modules.contains("mod"),
            "a subdir scan must not mint the scan-root-relative bare stem: {sub_modules:?}"
        );

        // The emitted File parent agrees with the module identity in both
        // scans, so every Struct/Function fqn parented at it agrees too.
        let parent_of = |recs: &[serde_json::Value], suffix: &str| -> Option<String> {
            recs.iter()
                .filter(|r| r["type"] == "file")
                .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with(suffix)))
                .and_then(|r| r["parent"].as_str().map(str::to_string))
        };
        assert_eq!(
            parent_of(&repo_recs, "pkg/mod.py").as_deref(),
            Some("pkg.mod")
        );
        assert_eq!(
            parent_of(&sub_recs, "pkg/mod.py").as_deref(),
            Some("pkg.mod")
        );
        assert_eq!(
            parent_of(&sub_recs, "pkg/__init__.py").as_deref(),
            Some("pkg")
        );
        assert!(
            node_id(&sub_recs, "function", "pkg.mod", "f").is_some(),
            "the subdir scan's declaration must hang under the repo-relative module"
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn resolves_cross_module_calls_and_uses() {
        let root = scratch_dir("resolve");
        write_file(&root, "pkg/__init__.py", "");
        write_file(&root, "pkg/base.py", "class Base:\n    pass\n");
        write_file(&root, "pkg/helpers.py", "def helper(x):\n    return x\n");
        write_file(
            &root,
            "pkg/app.py",
            "from pkg.base import Base\nfrom pkg.helpers import helper\n\n\n\
             class Foo(Base):\n    def run(self):\n        return helper(1)\n",
        );

        let recs = records(&run_frontend(&root, &[]));
        let foo = node_id(&recs, "struct", "pkg.app", "Foo").expect("Foo class");
        let run = node_id(&recs, "function", "pkg.app.Foo", "run").expect("Foo.run method");
        let base = node_id(&recs, "struct", "pkg.base", "Base").expect("Base class");
        let helper = node_id(&recs, "function", "pkg.helpers", "helper").expect("helper fn");

        // `self` is dropped and annotations are erased, so a method with no
        // other parameters carries an empty param list.
        let run_record = recs
            .iter()
            .find(|r| r["id"] == run)
            .expect("run declaration record");
        assert!(
            run_record["params"]
                .as_array()
                .is_some_and(|params| params.is_empty()),
            "self/cls must be dropped from the erased param list: {run_record:?}"
        );

        assert!(
            has_edge(&recs, "calls", &run, &helper),
            "run() must Calls helper()"
        );
        assert!(
            has_edge(&recs, "uses", &foo, &base),
            "Foo must Uses its base Base"
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn stdlib_references_are_categorized_stdlib() {
        let root = scratch_dir("stdlib");
        write_file(
            &root,
            "app.py",
            "import os\n\n\ndef go():\n    return os.getcwd()\n",
        );

        let recs = records(&run_frontend(&root, &[]));
        let stdlib = recs.iter().find(|r| {
            r["type"] == "unresolved" && r["fqn"] == "os.getcwd" && r["category"] == "stdlib"
        });
        assert!(
            stdlib.is_some(),
            "os.getcwd must be unresolved with category stdlib, got: {:?}",
            recs.iter()
                .filter(|r| r["type"] == "unresolved")
                .map(|r| (r["fqn"].clone(), r["category"].clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn site_packages_tree_is_never_a_code_node() {
        let root = scratch_dir("exclude");
        write_file(&root, "src/real.py", "def real():\n    return 1\n");
        write_file(
            &root,
            ".venv/lib/python3.12/site-packages/dep.py",
            "def dep():\n    return 2\n",
        );
        write_file(
            &root,
            "site-packages/other.py",
            "def other():\n    return 3\n",
        );

        let recs = records(&run_frontend(&root, &[]));
        let paths: Vec<String> = recs
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str().map(str::to_string))
            .collect();
        assert!(
            paths.iter().all(|p| !p.contains("site-packages")),
            "site-packages must never become a code node: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("src/real.py")),
            "the real source file must still be scanned: {paths:?}"
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn target_filter_emits_only_target_modules_with_canonical_endpoints() {
        let root = scratch_dir("targets");
        write_file(&root, "pkg/__init__.py", "");
        write_file(&root, "pkg/base.py", "class Base:\n    pass\n");
        write_file(&root, "pkg/helpers.py", "def helper(x):\n    return x\n");
        let app = write_file(
            &root,
            "pkg/app.py",
            "from pkg.base import Base\nfrom pkg.helpers import helper\n\n\n\
             class Foo(Base):\n    def run(self):\n        return helper(1)\n",
        );
        let targets = write_file(&root, "targets.txt", &format!("{}\n", app.display()));

        let recs = records(&run_frontend(
            &root,
            &["--targets", &targets.to_string_lossy()],
        ));

        // Module records are global scaffolding, present in full.
        let modules = module_fqns(&recs);
        for expected in ["pkg", "pkg.base", "pkg.helpers", "pkg.app"] {
            assert!(modules.contains(expected), "module {expected} missing");
        }

        // Only the target module's per-file facts are emitted.
        assert!(
            node_id(&recs, "struct", "pkg.base", "Base").is_none(),
            "non-target module declarations must not be emitted"
        );
        assert!(
            node_id(&recs, "function", "pkg.helpers", "helper").is_none(),
            "non-target module declarations must not be emitted"
        );
        let file_paths: Vec<String> = recs
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            file_paths.len(),
            1,
            "only the target file emits: {file_paths:?}"
        );
        assert!(file_paths[0].ends_with("pkg/app.py"));

        // Edges to non-emitted targets carry the canonical FQN, not a
        // dangling opaque id.
        let run = node_id(&recs, "function", "pkg.app.Foo", "run").expect("Foo.run");
        let foo = node_id(&recs, "struct", "pkg.app", "Foo").expect("Foo");
        assert_eq!(
            edge_target(&recs, "calls", &run).as_deref(),
            Some("pkg.helpers.helper")
        );
        assert_eq!(
            edge_target(&recs, "uses", &foo).as_deref(),
            Some("pkg.base.Base")
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns the built frontend over a scratch temp dir; run via cargo test-e2e"]
    fn third_party_venv_reference_is_external() {
        // A HAND-BUILT environment (markers + stub site-packages tree) with
        // no Python interpreter on PATH: auto-detection must be
        // filesystem-marker based and the dependency must resolve as
        // `external`, never `stdlib`.
        let root = scratch_dir("venv");
        write_file(
            &root,
            "pyproject.toml",
            "[project]\nname = \"fixture\"\nversion = \"0.1.0\"\nrequires-python = \">=3.12\"\n",
        );
        write_file(
            &root,
            ".venv/pyvenv.cfg",
            "home = /nonexistent\nversion = 3.12.0\n",
        );
        write_file(
            &root,
            ".venv/lib/python3.12/site-packages/dep/__init__.py",
            "def thing():\n    return 1\n",
        );
        write_file(
            &root,
            "app.py",
            "import dep\n\n\ndef go():\n    return dep.thing()\n",
        );

        let recs = records(&run_frontend(&root, &[]));
        let paths: Vec<String> = recs
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str().map(str::to_string))
            .collect();
        assert!(
            paths.iter().all(|p| !p.contains(".venv")),
            "the venv tree is a resolution input only: {paths:?}"
        );
        let unresolved: Vec<(serde_json::Value, serde_json::Value)> = recs
            .iter()
            .filter(|r| r["type"] == "unresolved")
            .map(|r| (r["fqn"].clone(), r["category"].clone()))
            .collect();
        assert!(
            unresolved
                .iter()
                .any(|(_, category)| category == "external"),
            "a venv dependency reference must be external; unresolved = {unresolved:?}"
        );
        assert!(
            unresolved.iter().all(|(_, category)| category != "stdlib"),
            "a venv dependency must never be classified stdlib: {unresolved:?}"
        );
    }
}
