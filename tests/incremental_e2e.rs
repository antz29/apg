mod common;

use apg::cache::{FactStore, FileFragment};
use apg::delta::ScanRecord;
use apg::graph::{Graph, Node, NodeKind};
use apg::incremental::*;
use apg::testutil::located;
use std::collections::BTreeSet;

/// e2e tier -- real I/O: these tests stage temp dirs, write state files and
/// run real libgit2 operations. Each is `#[ignore]`d, so a plain `cargo
/// test` never runs one; the only entry point is the named guard
/// `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/state files/git); run via cargo test-e2e"]
    fn state_round_trips_and_is_checkout_relative() {
        let dir = std::env::temp_dir().join(format!("apg-inc-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("wt");
        std::fs::create_dir_all(root.join("src")).unwrap();

        let mut g = Graph::default();
        g.nodes.insert(
            "m.A".into(),
            located(NodeKind::Struct, &root.join("src/a.go").to_string_lossy()),
        );
        g.nodes.insert(
            "m.B".into(),
            located(NodeKind::Struct, &root.join("src/b.go").to_string_lossy()),
        );
        g.uses.insert(("m.B".into(), "m.A".into()));
        let state = State::from_graph(&g, &root);
        assert_eq!(
            state.dep_edges_rel,
            vec![("src/b.go".to_string(), "src/a.go".to_string())]
        );
        assert!(state.signatures.contains_key("src/a.go"));
        state.save(&dir).unwrap();

        let loaded = State::load(&dir);
        assert_eq!(loaded.dep_edges_rel, state.dep_edges_rel);
        assert_eq!(loaded.signatures, state.signatures);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/state files/git); run via cargo test-e2e"]
    fn cascade_targets_only_on_a_signature_change() {
        let dir = std::env::temp_dir().join(format!("apg-cascade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("wt");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::create_dir_all(root.join("c")).unwrap();
        let apath = root.join("a/a.go").to_string_lossy().into_owned();
        let bpath = root.join("b/b.go").to_string_lossy().into_owned();
        let cpath = root.join("c/c.go").to_string_lossy().into_owned();

        // The stored baseline: a.Leaf() — no params; b calls a; c calls b.
        let mut old = Graph::default();
        let mut fa = located(NodeKind::Function, &apath);
        fa.params = vec![];
        old.nodes.insert("scratch/a.Leaf".into(), fa);
        let mut fb = located(NodeKind::Function, &bpath);
        fb.params = vec![];
        old.nodes.insert("scratch/b.Foo".into(), fb);
        let mut fc = located(NodeKind::Function, &cpath);
        fc.params = vec![];
        old.nodes.insert("scratch/c.Bar".into(), fc);
        old.calls
            .insert(("scratch/b.Foo".into(), "scratch/a.Leaf".into()));
        old.calls
            .insert(("scratch/c.Bar".into(), "scratch/b.Foo".into()));
        State::from_graph(&old, &root).save(&dir).unwrap();

        // Body-only: a.Leaf keeps its (empty) params ⇒ no cascade.
        let body = old.clone();
        let stage1 = BTreeSet::from(["a/a.go".to_string()]);
        assert!(
            extra_cascade_targets(&dir, &root, &body, &stage1).is_empty(),
            "a body-only change must not cascade"
        );

        // Signature change: a.Leaf gains a param ⇒ b and c cascade.
        let mut sig = old.clone();
        sig.nodes.get_mut("scratch/a.Leaf").unwrap().params = vec!["int".into()];
        let extra = extra_cascade_targets(&dir, &root, &sig, &stage1);
        assert!(extra.contains("b/b.go"), "{extra:?}");
        assert!(extra.contains("c/c.go"), "{extra:?}");
        assert!(!extra.contains("a/a.go"), "the seed is already stage-1");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/state files/git); run via cargo test-e2e"]
    fn no_previous_export_forces_full_scan_only_when_filtering() {
        // A scratch git repo with a recorded scan (manifest + scan record) but
        // NO local `apg/.trans/graph.jsonl`: with a non-empty target set the
        // full-universe seam cannot be derived, so the correctness fallback
        // forces a full scan (feedback-92). With an EMPTY target set nothing is
        // emission-filtered, so reuse stays available.
        use apg::cache::{CacheKey, Manifest, ScanConfigKey};
        use apg::delta::{FullScanReason, ScanRecord};

        let dir = std::env::temp_dir().join(format!("apg-noexport-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo_dir = dir.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo = git2::Repository::init(&repo_dir).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "apg test").unwrap();
            cfg.set_str("user.email", "t@example.com").unwrap();
        }
        std::fs::write(repo_dir.join(".gitignore"), "apg/.trans/\n").unwrap();
        std::fs::write(repo_dir.join("a.go"), "package a\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let sha = repo
            .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap()
            .to_string();

        // The shared store carries a recorded scan + an empty manifest (so the
        // current tree looks changed), but the worktree has no export.
        let apg_root = repo_dir.join("apg");
        std::fs::create_dir_all(&apg_root).unwrap();
        let store = FactStore::resolve(&apg_root).unwrap().root;
        std::fs::create_dir_all(&store).unwrap();
        let config = ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        };
        let key = CacheKey::compute(&config);
        Manifest::default().save(&store).unwrap();
        ScanRecord {
            sha,
            cache_key: key.clone(),
            manifest: Manifest::default(),
            content_key: None,
        }
        .save(&store)
        .unwrap();

        let prepared = prepare(&repo_dir, &apg_root, &config);
        assert!(
            matches!(prepared.full_scan, Some(FullScanReason::NoPreviousExport)),
            "a non-empty target set with no previous export must full-scan: {:?}",
            prepared.full_scan
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// phase-04 task-21: an incremental scan records ONLY the target files'
    /// fact units, while a full-scan control records every located file's
    /// unit and the baseline writes (manifest / ScanRecord / portable
    /// index/state) still land. Real temp-dir FactStore — e2e by the
    /// boundaries law.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir + store on disk); run via cargo test-e2e"]
    fn record_writes_only_the_target_files_units_with_a_full_scan_control() {
        use apg::cache::{CacheKey, Manifest, ScanConfigKey};

        let dir = std::env::temp_dir().join(format!("apg-record-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scan_root = dir.join("wt");
        std::fs::create_dir_all(scan_root.join("src")).unwrap();
        let cache_key = CacheKey::compute(&ScanConfigKey::default());

        // F located files under one module, each with a real file (so a
        // manifest OID exists) and a declaration.
        let rels = ["src/a.go", "src/b.go", "src/c.go", "src/d.go", "src/e.go"];
        let mut g = Graph::default();
        g.nodes.insert(
            "m".into(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        let mut manifest = Manifest::default();
        let mut abs_of: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (i, rel) in rels.iter().enumerate() {
            let abs = scan_root.join(rel);
            std::fs::write(&abs, format!("package m // {i}\n")).unwrap();
            let abs_s = abs.to_string_lossy().into_owned();
            abs_of.insert((*rel).to_string(), abs_s.clone());
            g.nodes
                .insert(abs_s.clone(), located(NodeKind::File, &abs_s));
            g.nodes
                .insert(format!("m.F{i}"), located(NodeKind::Function, &abs_s));
            g.contains.insert(("m".to_string(), abs_s.clone()));
            g.contains.insert((abs_s.clone(), format!("m.F{i}")));
            manifest
                .entries
                .insert((*rel).to_string(), format!("oid-{i}"));
        }
        // A manifest entry + target for a path the assembled graph does NOT
        // carry: the ONLY reason its unit is not written is the missing node.
        manifest
            .entries
            .insert("src/missing.go".into(), "oid-missing".into());

        // Each file's reference fragment + inputs digest (the oracle).
        let digest_of = |rel: &str| {
            FileFragment::from_graph(&g, &abs_of[rel], rel, manifest.oid(rel).unwrap(), "go")
                .inputs_digest()
        };

        // (a) incremental: a target set of K of the F located files (plus the
        // graph-absent one) writes exactly those K units, not all F.
        let store_a = dir.join("store-a");
        let targets: BTreeSet<String> = ["src/a.go", "src/c.go", "src/missing.go"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        record(
            &store_a, &cache_key, &scan_root, &g, &manifest, "sha-a", &targets,
        )
        .unwrap();
        let a = FactStore::at(store_a.clone()).load();
        assert_eq!(a.len(), 2, "exactly the two graph-carried targets");
        for rel in ["src/a.go", "src/c.go"] {
            assert!(
                a.has(
                    "go",
                    rel,
                    manifest.oid(rel).unwrap(),
                    &digest_of(rel),
                    &cache_key
                ),
                "target {rel} must have a unit"
            );
        }
        for rel in ["src/b.go", "src/d.go", "src/e.go"] {
            assert!(
                !a.has(
                    "go",
                    rel,
                    manifest.oid(rel).unwrap(),
                    &digest_of(rel),
                    &cache_key
                ),
                "unchanged non-target {rel} must have NO unit"
            );
        }
        // (d) a target the assembled graph does not carry is skipped even with
        // a manifest OID: no stale unit is invented.
        assert!(
            a.candidate("go", "src/missing.go", "oid-missing", &cache_key)
                .is_none(),
            "no unit for a graph-absent target"
        );

        // (b) full-scan control: the target set absent writes EVERY located
        // file's unit (the graph-absent one still yields none).
        let store_b = dir.join("store-b");
        record(
            &store_b,
            &cache_key,
            &scan_root,
            &g,
            &manifest,
            "sha-b",
            &BTreeSet::new(),
        )
        .unwrap();
        let b = FactStore::at(store_b.clone()).load();
        assert_eq!(b.len(), rels.len(), "every located file gets a unit");
        for rel in rels {
            assert!(
                b.has(
                    "go",
                    rel,
                    manifest.oid(rel).unwrap(),
                    &digest_of(rel),
                    &cache_key
                ),
                "full scan must record {rel}"
            );
        }

        // (c) the baseline writes still land after the runs, and re-running
        // with a target set leaves a reused unit's fragment/inputs digest
        // unchanged.
        assert!(Manifest::load(&store_b).is_some(), "manifest.json on disk");
        assert!(ScanRecord::load(&store_b).is_some(), "scan.json on disk");
        for name in ["deps.json", "signatures.json", "overloads.json"] {
            assert!(
                store_b.join("index").join(name).exists(),
                "index/{name} on disk"
            );
        }
        let digest_a = digest_of("src/a.go");
        let (before, before_root) = b
            .reuse(
                "go",
                "src/a.go",
                manifest.oid("src/a.go").unwrap(),
                &digest_a,
                &cache_key,
            )
            .expect("the full scan recorded src/a.go");
        assert_eq!(before_root, scan_root.to_string_lossy());

        let one: BTreeSet<String> = ["src/a.go".to_string()].into_iter().collect();
        record(
            &store_b, &cache_key, &scan_root, &g, &manifest, "sha-c", &one,
        )
        .unwrap();
        let c = FactStore::at(store_b.clone()).load();
        let (after, _) = c
            .reuse(
                "go",
                "src/a.go",
                manifest.oid("src/a.go").unwrap(),
                &digest_a,
                &cache_key,
            )
            .expect("the re-run kept src/a.go reusable");
        assert_eq!(after, before, "a re-used unit's fragment is unchanged");
        assert_eq!(
            after.inputs_digest(),
            digest_a,
            "inputs digest byte-identical"
        );

        // A one-target run against a FRESH store writes exactly one unit —
        // the scoping is not an artefact of units persisting from an earlier
        // full run.
        let store_c = dir.join("store-c");
        record(
            &store_c, &cache_key, &scan_root, &g, &manifest, "sha-d", &one,
        )
        .unwrap();
        let cc = FactStore::at(store_c.clone()).load();
        assert_eq!(cc.len(), 1, "a one-target run writes exactly one unit");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
