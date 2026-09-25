mod common;

use apg::cache::*;
use apg::graph::{Graph, Location, Node, NodeKind};
use std::path::{Path, PathBuf};

fn tmp_root(dir: &Path) -> PathBuf {
    // The store root is nested; remove the whole scratch.
    dir.parent().unwrap().to_path_buf()
}

/// e2e tier -- real I/O: every test here writes files / the content-addressed
/// store under the temp dir. Each is `#[ignore]`d, so a plain `cargo test`
/// never runs one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir + store on disk); run via cargo test-e2e"]
    fn manifest_blob_oid_is_content_addressed_not_mtime() {
        let dir = std::env::temp_dir().join(format!("apg-cache-oid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.go");
        std::fs::write(&f, "package a\n").unwrap();
        let oid1 = blob_oid_of_file(&f).unwrap();

        // Touching without changing bytes keeps the OID (mtime ignored).
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        let times = std::fs::FileTimes::new().set_modified(later);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(times)
            .unwrap();
        assert_eq!(blob_oid_of_file(&f).unwrap(), oid1);

        // A byte edit changes it.
        std::fs::write(&f, "package a\n\nvar X = 1\n").unwrap();
        assert_ne!(blob_oid_of_file(&f).unwrap(), oid1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir + store on disk); run via cargo test-e2e"]
    fn fact_store_reuses_only_on_bytes_and_inputs_and_key() {
        let dir = std::env::temp_dir().join(format!("apg-cache-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        let mut g = Graph::default();
        g.nodes.insert(
            "fixture.mod".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/w/a.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: PathBuf::from("/w/a.go"),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 3,
                }),
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/w/a.go.A".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: PathBuf::from("/w/a.go"),
                    start: 0,
                    end: 3,
                    start_line: 1,
                    end_line: 3,
                }),
                code_type: "src".into(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("/w/a.go".to_string(), "/w/a.go.A".to_string()));
        g.contains
            .insert(("fixture.mod".to_string(), "/w/a.go".to_string()));
        // A function with a non-empty param list: the declaration surface the
        // signature early-cutoff compares must survive the cache round-trip.
        g.nodes.insert(
            "/w/a.go.F".to_string(),
            Node {
                kind: NodeKind::Function,
                location: Some(Location {
                    path: PathBuf::from("/w/a.go"),
                    start: 1,
                    end: 2,
                    start_line: 2,
                    end_line: 2,
                }),
                params: vec!["int".into()],
                code_type: "src".into(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("/w/a.go".to_string(), "/w/a.go.F".to_string()));
        let frag = FileFragment::from_graph(&g, "/w/a.go", "a.go", "oid-a", "go");
        let inputs = frag.inputs_digest();
        store.put(&frag, "/w", &cache_key).unwrap();

        // Same bytes + same inputs + same key ⇒ reusable.
        assert!(store.has("go", "a.go", "oid-a", &inputs, &cache_key));
        let (got, root) = store
            .reuse("go", "a.go", "oid-a", &inputs, &cache_key)
            .unwrap();
        assert_eq!(root, "/w");
        let (mods, nodes, edges) = got.project("/w", "/fresh");
        assert_eq!(mods, vec!["fixture.mod".to_string()]);
        // The function's params survive the round-trip (the signature cutoff
        // would otherwise see an empty list after one incremental generation).
        let f = nodes
            .iter()
            .find(|(fqn, _)| fqn == "a.go.F")
            .expect("the cached function projects");
        assert_eq!(f.1.params, vec!["int".to_string()]);
        // Identities are repo-relative (`requirements.requirement.
        // portable-graph-identity`): the stored File fqn/paths project
        // verbatim, never the absolute checkout path.
        assert!(nodes.iter().any(|(f, _)| f == "a.go"));
        assert!(nodes.iter().any(|(f, _)| f == "a.go.A"));
        assert!(edges.iter().any(|e| e.kind == "contains"));

        // Different bytes (new OID) ⇒ not reusable.
        assert!(!store.has("go", "a.go", "oid-b", &inputs, &cache_key));
        // Same bytes but drifted resolution inputs ⇒ not reusable.
        assert!(!store.has("go", "a.go", "oid-a", "different", &cache_key));
        assert!(
            store
                .reuse("go", "a.go", "oid-a", "different", &cache_key)
                .is_none()
        );
        // Cache-key drift ⇒ not reusable.
        let drifted = CacheKey::compute(&ScanConfigKey {
            languages: vec!["java".into()],
            ..Default::default()
        });
        assert!(!store.has("go", "a.go", "oid-a", &inputs, &drifted));
        let _ = std::fs::remove_dir_all(tmp_root(&dir));
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir + store on disk); run via cargo test-e2e"]
    fn cross_worktree_unit_is_rebased_but_content_addressed_once() {
        let dir = std::env::temp_dir().join(format!("apg-cache-xwt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        let mut g = Graph::default();
        g.nodes.insert(
            "fixture.mod".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/main/src/a.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: PathBuf::from("/main/src/a.go"),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 2,
                }),
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/main/src/a.go.A".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: PathBuf::from("/main/src/a.go"),
                    start: 0,
                    end: 2,
                    start_line: 1,
                    end_line: 2,
                }),
                code_type: "src".into(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("fixture.mod".to_string(), "/main/src/a.go".to_string()));
        let frag = FileFragment::from_graph(&g, "/main/src/a.go", "src/a.go", "oid-a", "go");
        let inputs = frag.inputs_digest();
        store.put(&frag, "/main", &cache_key).unwrap();

        // The fresh worktree has the SAME relative path + bytes (sharing keys
        // are relative), so it reuses the unit; its repo-relative identities
        // are the writing checkout's, verbatim.
        assert!(store.has("go", "src/a.go", "oid-a", &inputs, &cache_key));
        let (frag2, root) = store
            .reuse("go", "src/a.go", "oid-a", &inputs, &cache_key)
            .unwrap();
        let (mods, nodes, _) = frag2.project(&root, "/fresh");
        // Module FQNs are identifiers; the File fqn/paths are repo-relative.
        assert_eq!(mods, vec!["fixture.mod".to_string()]);
        assert!(
            nodes
                .iter()
                .any(|(f, n)| f == "src/a.go" && n.location.is_some())
        );
        assert!(nodes.iter().any(|(f, _)| f == "src/a.go.A"));
        // Exactly one stored unit for the shared content.
        assert_eq!(store.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// feedback-102: the per-language scaffolding carries the pure-intermediate
    /// modules, the file-less descendants, and every `Module -> Module` edge a
    /// per-file fact unit cannot — while planned placeholders and file-less
    /// components are excluded — and it round-trips through the store under a
    /// cache key.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir + store on disk); run via cargo test-e2e"]
    fn module_scaffolding_covers_intermediates_descendants_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("apg-cache-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let store = FactStore::at(dir.join("facts"));

        let module = || Node {
            kind: NodeKind::Module,
            ..Node::default()
        };
        let file = |path: &str| Node {
            kind: NodeKind::File,
            location: Some(Location {
                path: PathBuf::from(path),
                start: 0,
                end: 0,
                start_line: 1,
                end_line: 1,
            }),
            ..Node::default()
        };
        let mut g = Graph::default();
        // Go: a root and a child module, each with a file.
        g.nodes.insert("godemo".into(), module());
        g.nodes.insert("godemo/changed".into(), module());
        g.nodes.insert("/x/go/a.go".into(), file("/x/go/a.go"));
        g.nodes.insert("/x/go/b.go".into(), file("/x/go/b.go"));
        g.contains
            .insert(("godemo".into(), "godemo/changed".into()));
        g.contains.insert(("godemo".into(), "/x/go/a.go".into()));
        g.contains
            .insert(("godemo/changed".into(), "/x/go/b.go".into()));
        // C#: two pure-intermediate modules above a leaf module that owns the
        // file, plus a file-less descendant (an inline test module).
        g.nodes.insert("Apg".into(), module());
        g.nodes.insert("Apg.CsharpFrontend".into(), module());
        g.nodes.insert("Apg.CsharpFrontend.Tests".into(), module());
        g.nodes
            .insert("Apg.CsharpFrontend.Tests.Inline".into(), module());
        g.nodes
            .insert("/x/csharp/T.cs".into(), file("/x/csharp/T.cs"));
        g.contains
            .insert(("Apg".into(), "Apg.CsharpFrontend".into()));
        g.contains.insert((
            "Apg.CsharpFrontend".into(),
            "Apg.CsharpFrontend.Tests".into(),
        ));
        g.contains.insert((
            "Apg.CsharpFrontend.Tests".into(),
            "Apg.CsharpFrontend.Tests.Inline".into(),
        ));
        g.contains
            .insert(("Apg.CsharpFrontend.Tests".into(), "/x/csharp/T.cs".into()));
        // A planned (transient) module with no file: never scaffolding.
        g.nodes.insert(
            "rust.planned".into(),
            Node {
                kind: NodeKind::Module,
                status: Some("planned".into()),
                ..Node::default()
            },
        );

        let by_lang = ModuleScaffolding::extract(&g, Path::new("/x"));
        assert!(
            !by_lang.contains_key("rust"),
            "a planned, file-less module is not scaffolding: {by_lang:?}"
        );
        let go = &by_lang["go"];
        assert_eq!(go.modules, vec!["godemo", "godemo/changed"]);
        assert_eq!(
            go.edges,
            vec![("godemo".to_string(), "godemo/changed".to_string())]
        );
        let cs = &by_lang["csharp"];
        assert_eq!(
            cs.modules,
            vec![
                "Apg",
                "Apg.CsharpFrontend",
                "Apg.CsharpFrontend.Tests",
                "Apg.CsharpFrontend.Tests.Inline",
            ]
        );
        // Both hierarchy edges (intermediate -> intermediate, and the
        // file-less descendant) survive.
        assert!(
            cs.edges
                .contains(&("Apg".to_string(), "Apg.CsharpFrontend".to_string()))
        );
        assert!(cs.edges.contains(&(
            "Apg.CsharpFrontend".to_string(),
            "Apg.CsharpFrontend.Tests".to_string()
        )));
        assert!(cs.edges.contains(&(
            "Apg.CsharpFrontend.Tests".to_string(),
            "Apg.CsharpFrontend.Tests.Inline".to_string()
        )));

        store.put_scaffolding_all(&by_lang, &cache_key).unwrap();
        assert_eq!(store.scaffolding("csharp", &cache_key).as_ref(), Some(cs));
        // A drifted key sees nothing (the store never mis-reads an old unit).
        let drifted = CacheKey::compute(&ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        });
        assert!(store.scaffolding("csharp", &drifted).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// fix-module-identity task-18: cross-worktree cached reuse is
    /// repo-relative. The cold main-checkout scan records the shared cache;
    /// a second worktree at the same commit reuses it and mints exactly the
    /// SAME File identities — no checkout root appears in either graph, so
    /// the cache's original checkout-relative model is now the graph's
    /// repo-relative base. Candidate binary only, scratch repo.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/spawned apg/db.lbug); run via cargo test-e2e"]
    fn cross_worktree_cached_reuse_is_repo_relative() {
        use apg::testutil::{ApgCommand, Repo};
        let repo = Repo::new("cache-rebase");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");

        let home = std::env::temp_dir().join(format!("apg-cache-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let home_s = home.to_string_lossy().into_owned();
        let scan = |dir: &Path| -> std::process::Output {
            ApgCommand::new(&["scan", "."])
                .cwd(dir)
                .env("HOME", &home_s)
                .output()
        };
        let file_fqns = |dir: &Path| -> std::collections::BTreeSet<String> {
            let text = std::fs::read_to_string(dir.join("apg/.trans/graph.jsonl")).unwrap();
            text.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("file"))
                .filter_map(|v| v.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
                .collect()
        };

        // The cold main-checkout scan records the shared store.
        let cold = scan(&repo.root);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let main_files = file_fqns(&repo.root);
        assert!(
            main_files.contains("a/a.go"),
            "repo-relative File identity: {main_files:?}"
        );
        assert!(
            repo.root.join(".git/apg/facts/index.json").is_file(),
            "the shared fact store must be indexed after a scan"
        );

        // A clean second worktree at the same commit reuses that store and
        // mints the SAME identities.
        let wt = repo.start_project("wt2");
        let fresh = scan(&wt);
        assert!(
            fresh.status.success(),
            "worktree scan: {}",
            String::from_utf8_lossy(&fresh.stderr)
        );
        let fresh_err = String::from_utf8_lossy(&fresh.stderr).into_owned();
        assert!(
            fresh_err.contains("incremental")
                || fresh_err.contains("reusable file")
                || fresh_err.contains("warm cache"),
            "the worktree must reuse the shared cache: {fresh_err}"
        );
        let wt_files = file_fqns(&wt);
        for f in &wt_files {
            assert!(!f.starts_with('/'), "no absolute File fqn: {f}");
            assert!(
                !f.contains(&repo.root.to_string_lossy().into_owned()),
                "no checkout component: {f}"
            );
        }
        assert_eq!(
            wt_files, main_files,
            "cross-worktree identities must be identical"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }
}
