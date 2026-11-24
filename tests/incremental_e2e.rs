mod common;

use apg::cache::{FactStore, FileFragment, worktree_id};
use apg::delta::ScanRecord;
use apg::graph::{Graph, Node, NodeKind};
use apg::incremental::*;
use apg::testutil::located;
use std::collections::BTreeSet;

/// An empty manifest rooted at `root`: the per-worktree store key derives from
/// the root, so a fixture record must carry a real checkout root.
fn rooted_manifest(root: &std::path::Path) -> apg::cache::Manifest {
    apg::cache::Manifest {
        root: root.to_string_lossy().into_owned(),
        entries: Default::default(),
    }
}

/// The code identities a scan's database carries — the oracle a warm assembly
/// must reproduce. Read from `db.lbug`, never a JSONL export.
fn code_identities(dir: &std::path::Path) -> BTreeSet<String> {
    let db_path = dir.join("apg/.trans/db.lbug");
    let db = lbug::Database::new(&db_path, apg::testutil::fixture_db_config().read_only(true))
        .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
    let conn = lbug::Connection::new(&db).unwrap();
    let mut ids = BTreeSet::new();
    for (label, property) in [
        ("Module", "fqn"),
        ("File", "fqn"),
        ("Struct", "path"),
        ("Function", "path"),
    ] {
        let (_, rows) =
            apg::splice::query_rows(&conn, &format!("MATCH (n:{label}) RETURN n.{property}"))
                .unwrap();
        ids.extend(rows.into_iter().filter_map(|row| row.into_iter().next()));
    }
    ids
}

/// e2e tier -- real I/O: these tests stage temp dirs, write state files and
/// run real libgit2 operations. Each is `#[ignore]`d, so a plain `cargo
/// test` never runs one; the only entry point is the named guard
/// `cargo test-e2e` (= `cargo test e2e:: -- --ignored`).
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
        state.save(&dir, &root).unwrap();

        let loaded = State::load(&dir, &root);
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
        State::from_graph(&old, &root).save(&dir, &root).unwrap();

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
    #[ignore = "e2e tier: real I/O (temp dir/state files/git/database); run via cargo test-e2e"]
    fn no_previous_export_forces_full_scan_only_when_filtering() {
        // Eligibility is the previous DATABASE rule (feedback-92), asserted
        // where `prepare` decides it: with a non-empty target set an absent
        // `db.lbug`, an unreadable one, or one carrying pending state (a dirty
        // row or a pending-removal identity) forces the full scan — no
        // unusable previous database may seed the full-universe seam. An EMPTY
        // target set emits every fact, so the full spool is already the
        // universe and reuse stays available; a clean database keeps the
        // ordinary delta.
        use apg::cache::{CacheKey, Manifest, ScanConfigKey};
        use apg::delta::{FullScanReason, ScanRecord};

        let config = ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        };
        let key = CacheKey::compute(&config);

        // A fresh scratch git repo per case, so the four previous-database
        // states cannot leak into each other. An EMPTY recorded manifest makes
        // the unrecorded `a.go` a changed target; the recorded manifest is
        // supplied per case.
        let fixture = |tag: &str| {
            let dir =
                std::env::temp_dir().join(format!("apg-previous-db-{}-{tag}", std::process::id()));
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

            let apg_root = repo_dir.join("apg");
            std::fs::create_dir_all(&apg_root).unwrap();
            let store = FactStore::resolve(&apg_root).unwrap().root;
            (dir, repo_dir, apg_root, store, sha)
        };
        let record = |store: &std::path::Path, sha: &str, manifest: Manifest| {
            std::fs::create_dir_all(store).unwrap();
            manifest.save(store).unwrap();
            ScanRecord {
                sha: sha.to_string(),
                cache_key: key.clone(),
                manifest,
                content_key: None,
            }
            .save(store)
            .unwrap();
        };

        // (1) No `db.lbug` at all + a non-empty target set: there is no
        // previous database to derive the full universe from, so the scan
        // full-scans.
        {
            let (dir, repo_dir, apg_root, store, sha) = fixture("no-db");
            record(&store, &sha, rooted_manifest(&repo_dir));

            let prepared = prepare(&repo_dir, &apg_root, &config, false);
            assert!(
                matches!(prepared.full_scan, Some(FullScanReason::NoPreviousDatabase)),
                "an absent previous database with a non-empty target set must full-scan: {:?}",
                prepared.full_scan
            );
            assert!(
                prepared.targets_rel.is_empty(),
                "the fallback must clear the target set: {:?}",
                prepared.targets_rel
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // (2) `db.lbug` carrying a dirty row + a non-empty target set: an
        // admitted-but-unsaved row must never seed the splice — the
        // pending-state half of the same rule.
        {
            let (dir, repo_dir, apg_root, store, sha) = fixture("dirty-db");
            record(&store, &sha, rooted_manifest(&repo_dir));
            apg::testutil::touch_db(&apg_root);
            {
                let db = apg::artifacts::ArtifactDb::open(&apg_root).unwrap();
                db.conn()
                    .unwrap()
                    .query(
                        "CREATE (n:Requirement {fqn: 'requirements.requirement.probe', updated: 1})",
                    )
                    .unwrap();
            }

            let prepared = prepare(&repo_dir, &apg_root, &config, false);
            assert!(
                matches!(prepared.full_scan, Some(FullScanReason::NoPreviousDatabase)),
                "a previous database carrying pending state with a non-empty target set must full-scan: {:?}",
                prepared.full_scan
            );
            assert!(
                prepared.targets_rel.is_empty(),
                "the fallback must clear the target set: {:?}",
                prepared.targets_rel
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // (3) A clean `db.lbug` + a non-empty target set: the database is a
        // usable previous universe, so the ordinary delta keeps reuse.
        {
            let (dir, repo_dir, apg_root, store, sha) = fixture("clean-db");
            record(&store, &sha, rooted_manifest(&repo_dir));
            apg::testutil::touch_db(&apg_root);

            let prepared = prepare(&repo_dir, &apg_root, &config, false);
            assert!(
                prepared.full_scan.is_none(),
                "a clean previous database keeps the delta: {:?}",
                prepared.full_scan
            );
            assert!(
                prepared.targets_rel.contains("a.go"),
                "the unrecorded a.go change must stay a target: {:?}",
                prepared.targets_rel
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // (4) An EMPTY target set with no `db.lbug`: nothing is emission-
        // filtered, so the full spool is already the universe and reuse stays
        // available despite the absent database.
        {
            let (dir, repo_dir, apg_root, store, sha) = fixture("no-targets");
            record(&store, &sha, Manifest::build(&repo_dir));

            let prepared = prepare(&repo_dir, &apg_root, &config, false);
            assert!(
                prepared.full_scan.is_none(),
                "an empty target set keeps reuse available without a database: {:?}",
                prepared.full_scan
            );
            assert!(
                prepared.targets_rel.is_empty(),
                "a recorded manifest matching the tree yields no targets: {:?}",
                prepared.targets_rel
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
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
        let mut manifest = Manifest {
            root: scan_root.to_string_lossy().into_owned(),
            entries: Default::default(),
        };
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

        // (c) the baseline rows still land after the runs, and re-running
        // with a target set leaves a reused unit's fragment/inputs digest
        // unchanged.
        let id = worktree_id(&scan_root);
        assert!(
            Manifest::load(&store_b, &scan_root).is_some(),
            "manifest row"
        );
        assert!(ScanRecord::load(&store_b, &scan_root).is_some(), "scan row");
        assert!(b.deps(id).is_some(), "deps row");
        assert!(b.signatures(id).is_some(), "signatures row");
        assert!(b.overloads(id).is_some(), "overloads row");
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

    /// phase-05 task-4: an incremental `record` merges the target-only delta
    /// graph into the full baseline's persisted index — the merged
    /// `deps.json`/`signatures.json`/`overloads.json` reproduce a full-graph
    /// `State::from_graph` rebuild — and merges each TOUCHED language's stored
    /// module scaffolding (retiring a module whose every file was a target and
    /// that the delta no longer carries) while a SKIPPED language's record stays
    /// byte-identical. Real temp-dir FactStore — e2e by the boundaries law.
    #[test]
    #[ignore = "e2e tier: real I/O (temp-dir FactStore on disk); run via cargo test-e2e"]
    fn delta_record_merges_the_index_and_keeps_skipped_scaffolding() {
        use apg::cache::{CacheKey, Manifest, ScanConfigKey};

        let dir = std::env::temp_dir().join(format!("apg-delta-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scan_root = dir.join("wt");
        std::fs::create_dir_all(scan_root.join("src")).unwrap();
        let store_root = dir.join("store");
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let abs = |rel: &str| scan_root.join(rel).to_string_lossy().into_owned();

        let rels = ["src/a.go", "src/b.go", "src/c.go", "src/d.go", "src/old.go"];
        let mut manifest = Manifest {
            root: scan_root.to_string_lossy().into_owned(),
            entries: Default::default(),
        };
        for (i, rel) in rels.iter().chain(["src/J.java"].iter()).enumerate() {
            std::fs::write(scan_root.join(rel), format!("package m // {i}\n")).unwrap();
            manifest
                .entries
                .insert((*rel).to_string(), format!("oid-{i}"));
        }

        // The full baseline (pre-edit tree). `go` owns two components:
        // `go.root -> {go.svc, go.retired}` (a pure-intermediate module with no
        // direct file, so no `files` entry — the legacy record that must survive
        // the merge) and the separate `go.mixed`; `java` is the skipped language.
        let mut base = Graph::default();
        for lang in ["go", "java"] {
            base.nodes.insert(
                lang.into(),
                Node {
                    kind: NodeKind::Language,
                    ..Node::default()
                },
            );
        }
        for m in ["go.root", "go.svc", "go.retired", "go.mixed", "java.core"] {
            base.nodes.insert(
                m.into(),
                Node {
                    kind: NodeKind::Module,
                    ..Node::default()
                },
            );
        }
        for (rel, module) in [
            ("src/a.go", "go.svc"),
            ("src/b.go", "go.svc"),
            ("src/old.go", "go.retired"),
            ("src/c.go", "go.mixed"),
            ("src/d.go", "go.mixed"),
            ("src/J.java", "java.core"),
        ] {
            let p = abs(rel);
            base.nodes.insert(p.clone(), located(NodeKind::File, &p));
            base.contains.insert((module.to_string(), p));
        }
        base.contains.insert(("go.root".into(), "go.svc".into()));
        base.contains
            .insert(("go.root".into(), "go.retired".into()));
        for (lang, module) in [
            ("go", "go.root"),
            ("go", "go.svc"),
            ("go", "go.retired"),
            ("go", "go.mixed"),
            ("java", "java.core"),
        ] {
            base.contains.insert((lang.to_string(), module.to_string()));
        }
        for (fqn, file) in [
            ("go.svc.A", "src/a.go"),
            ("go.svc.B", "src/b.go"),
            ("go.retired.Old", "src/old.go"),
            ("go.mixed.C", "src/c.go"),
            ("go.mixed.D", "src/d.go"),
            ("java.core.J", "src/J.java"),
        ] {
            let p = abs(file);
            base.nodes
                .insert(fqn.into(), located(NodeKind::Function, &p));
            base.contains.insert((p, fqn.into()));
        }
        // Baseline references: `a.go` used to reach the soon-retired `old.go`;
        // `b.go` reaches both `a.go` (the surviving target) and `old.go`
        // (deleted/renamed away); `d.go` reaches the other surviving target.
        base.calls
            .insert(("go.svc.A".into(), "go.retired.Old".into()));
        base.calls.insert(("go.svc.B".into(), "go.svc.A".into()));
        base.calls
            .insert(("go.svc.B".into(), "go.retired.Old".into()));
        base.calls
            .insert(("go.mixed.D".into(), "go.mixed.C".into()));

        record(
            &store_root,
            &cache_key,
            &scan_root,
            &base,
            &manifest,
            "sha-base",
            &BTreeSet::new(),
        )
        .unwrap();

        // A skipped language's scaffolding is only ever written by a full scan;
        // capture it to prove the delta path never touches it.
        let java_before = FactStore::at(store_root.clone())
            .load()
            .scaffolding("java", &cache_key)
            .expect("the full scan wrote the skipped language");

        // The target-only delta graph: `src/a.go`/`src/c.go` re-emitted with a
        // changed signature and a cross-cut call into an unchanged file's
        // declaration; `src/old.go` was deleted, so the delta does not carry it.
        let mut delta = Graph::default();
        delta.nodes.insert(
            "go".into(),
            Node {
                kind: NodeKind::Language,
                ..Node::default()
            },
        );
        for m in ["go.svc", "go.mixed"] {
            delta.nodes.insert(
                m.into(),
                Node {
                    kind: NodeKind::Module,
                    ..Node::default()
                },
            );
        }
        for (rel, module) in [("src/a.go", "go.svc"), ("src/c.go", "go.mixed")] {
            let p = abs(rel);
            delta.nodes.insert(p.clone(), located(NodeKind::File, &p));
            delta.contains.insert((module.to_string(), p));
        }
        for module in ["go.svc", "go.mixed"] {
            delta
                .contains
                .insert(("go".to_string(), module.to_string()));
        }
        let mut a = located(NodeKind::Function, &abs("src/a.go"));
        a.params = vec!["int".into()];
        delta.nodes.insert("go.svc.A".into(), a);
        let mut c = located(NodeKind::Function, &abs("src/c.go"));
        c.params = vec!["string".into()];
        delta.nodes.insert("go.mixed.C".into(), c);
        delta.calls.insert(("go.svc.A".into(), "go.svc.B".into()));
        delta
            .calls
            .insert(("go.mixed.C".into(), "go.mixed.D".into()));

        let targets: BTreeSet<String> = ["src/a.go", "src/c.go", "src/old.go"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        record(
            &store_root,
            &cache_key,
            &scan_root,
            &delta,
            &manifest,
            "sha-delta",
            &targets,
        )
        .unwrap();

        let id = worktree_id(&scan_root);
        let persisted = FactStore::at(store_root.clone()).load();
        assert!(persisted.deps(id).is_some(), "deps row");
        assert!(persisted.signatures(id).is_some(), "signatures row");
        assert!(persisted.overloads(id).is_some(), "overloads row");

        // The full NEW graph the delta is a target-only slice of: the same tree
        // with `src/old.go` gone and the two targets re-declared.
        let mut full = Graph::default();
        let mut a = located(NodeKind::Function, &abs("src/a.go"));
        a.params = vec!["int".into()];
        full.nodes.insert("go.svc.A".into(), a);
        full.nodes.insert(
            "go.svc.B".into(),
            located(NodeKind::Function, &abs("src/b.go")),
        );
        let mut c = located(NodeKind::Function, &abs("src/c.go"));
        c.params = vec!["string".into()];
        full.nodes.insert("go.mixed.C".into(), c);
        full.nodes.insert(
            "go.mixed.D".into(),
            located(NodeKind::Function, &abs("src/d.go")),
        );
        full.nodes.insert(
            "java.core.J".into(),
            located(NodeKind::Function, &abs("src/J.java")),
        );
        full.calls.insert(("go.svc.A".into(), "go.svc.B".into()));
        full.calls.insert(("go.svc.B".into(), "go.svc.A".into()));
        full.calls
            .insert(("go.mixed.C".into(), "go.mixed.D".into()));
        full.calls
            .insert(("go.mixed.D".into(), "go.mixed.C".into()));
        let truth = State::from_graph(&full, &scan_root);

        // The merged index reproduces the full-graph rebuild. Dep edges and
        // signatures compare directly; overload scopes are sorted on the merge
        // path, so compare their members as SETS.
        let merged = State::load(&store_root, &scan_root);
        assert_eq!(merged.dep_edges_rel, truth.dep_edges_rel);
        assert_eq!(merged.signatures, truth.signatures);
        assert_eq!(merged.overloads.file_scopes, truth.overloads.file_scopes);
        let scopes_as_sets = |index: &apg::impact::OverloadIndex| {
            index
                .scopes
                .iter()
                .map(|(scope, members)| {
                    (
                        scope.clone(),
                        members.iter().cloned().collect::<BTreeSet<String>>(),
                    )
                })
                .collect::<std::collections::BTreeMap<String, BTreeSet<String>>>()
        };
        assert_eq!(
            scopes_as_sets(&merged.overloads),
            scopes_as_sets(&truth.overloads)
        );

        // The target's own dependency entries are dropped and replaced from the
        // delta (`src/a.go` no longer reaches the retired `old.go`, it now
        // reaches `b.go` through the unchanged file's signature).
        assert!(
            merged
                .dep_edges_rel
                .contains(&("src/a.go".to_string(), "src/b.go".to_string()))
        );
        assert!(
            !merged
                .dep_edges_rel
                .contains(&("src/a.go".to_string(), "src/old.go".to_string()))
        );
        // A surviving target's second-half entry (non-target `d.go` depend on
        // target `c.go`) is kept.
        assert!(
            merged
                .dep_edges_rel
                .contains(&("src/d.go".to_string(), "src/c.go".to_string()))
        );
        // A dependency on the deleted/renamed-away file is gone.
        assert!(
            !merged
                .dep_edges_rel
                .contains(&("src/b.go".to_string(), "src/old.go".to_string()))
        );

        // The touched `go` scaffolding merged: the retired module is gone, the
        // module with a surviving non-target file and the legacy module (no
        // `files` entry) survive, and each edge/language edge to the retired
        // module is dropped.
        let store = FactStore::at(store_root.clone()).load();
        let go = store
            .scaffolding("go", &cache_key)
            .expect("the touched language has a record");
        assert!(go.modules.contains(&"go.svc".to_string()), "{go:?}");
        assert!(go.modules.contains(&"go.mixed".to_string()), "{go:?}");
        assert!(
            go.modules.contains(&"go.root".to_string()),
            "a legacy module with no `files` entry survives: {go:?}"
        );
        assert!(
            !go.modules.contains(&"go.retired".to_string()),
            "a module whose only file was a target and that the delta lacks is retired: {go:?}"
        );
        assert!(
            !go.files.contains_key("go.root"),
            "the legacy module keeps its absent `files` entry: {go:?}"
        );
        assert_eq!(
            go.files["go.svc"],
            vec!["src/a.go".to_string(), "src/b.go".to_string()],
            "a target module keeps its surviving non-target file"
        );
        assert_eq!(
            go.files["go.mixed"],
            vec!["src/c.go".to_string(), "src/d.go".to_string()]
        );
        assert!(
            go.edges
                .contains(&("go.root".to_string(), "go.svc".to_string()))
        );
        assert!(
            !go.edges
                .contains(&("go.root".to_string(), "go.retired".to_string())),
            "an edge to a retired module is dropped: {go:?}"
        );
        assert!(
            go.language_edges
                .contains(&("go".to_string(), "go.mixed".to_string()))
        );
        assert!(
            !go.language_edges
                .contains(&("go".to_string(), "go.retired".to_string())),
            "a language edge to a retired module is dropped: {go:?}"
        );

        // The skipped language's stored scaffolding is unchanged.
        let java_after = FactStore::at(store_root.clone())
            .load()
            .scaffolding("java", &cache_key)
            .unwrap();
        assert_eq!(
            java_after, java_before,
            "a skipped language's scaffolding stays unchanged"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/state files/git/database); run via cargo test-e2e"]
    fn ps_change_widens_targets_to_every_ps_file() {
        // A change to any ps file re-targets EVERY manifest ps file (the ps
        // stream is whole-tree, `apg::ps::widen_targets`), so none is reused;
        // a non-ps change pulls in no ps file.
        use apg::cache::{CacheKey, Manifest, ScanConfigKey};
        use apg::impact::OverloadIndex;

        let config = ScanConfigKey {
            languages: vec!["ps".into(), "md".into(), "go".into()],
            ..Default::default()
        };
        let key = CacheKey::compute(&config);
        let ps_files: BTreeSet<String> = ["a.ps1", "b.ps1", "sub/Util.PSM1", "sub/Mod.psd1"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let commit = |repo: &git2::Repository, msg: &str| {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = repo.signature().unwrap();
            let parent = repo.head().ok().map(|h| h.peel_to_commit().unwrap());
            let parents: Vec<&git2::Commit> = parent.iter().collect();
            repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
                .unwrap()
                .to_string()
        };
        // A fresh scratch repo per case: the committed tree, its recorded
        // manifest + ScanRecord, the persisted overload state and a clean
        // previous database.
        let fixture = |tag: &str| {
            let dir =
                std::env::temp_dir().join(format!("apg-ps-widen-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("repo/sub")).unwrap();
            let repo_dir = std::fs::canonicalize(dir.join("repo")).unwrap();
            let repo = git2::Repository::init(&repo_dir).unwrap();
            {
                let mut cfg = repo.config().unwrap();
                cfg.set_str("user.name", "apg test").unwrap();
                cfg.set_str("user.email", "t@example.com").unwrap();
            }
            for (p, body) in [
                (".gitignore", "apg/.trans/\n"),
                ("a.ps1", "function F { 1 }\n"),
                ("b.ps1", "F\n"),
                ("sub/Util.PSM1", "function U { 2 }\n"),
                ("sub/Mod.psd1", "@{ RootModule = 'Util.PSM1' }\n"),
                ("x.go", "package x\n\nfunc X() {}\n"),
                ("README.md", "# readme\n"),
            ] {
                std::fs::write(repo_dir.join(p), body).unwrap();
            }
            let sha = commit(&repo, "base");

            let apg_root = repo_dir.join("apg");
            std::fs::create_dir_all(&apg_root).unwrap();
            let store = FactStore::resolve(&apg_root).unwrap().root;
            std::fs::create_dir_all(&store).unwrap();
            let manifest = Manifest::build(&repo_dir);
            manifest.save(&store).unwrap();
            ScanRecord {
                sha,
                cache_key: key.clone(),
                manifest,
                content_key: None,
            }
            .save(&store)
            .unwrap();

            let mut g = Graph::default();
            g.nodes.insert(
                "ps.a.ps1.F".into(),
                located(
                    NodeKind::Function,
                    &repo_dir.join("a.ps1").to_string_lossy(),
                ),
            );
            g.nodes.insert(
                "go.x.X".into(),
                located(NodeKind::Function, &repo_dir.join("x.go").to_string_lossy()),
            );
            State {
                overloads: OverloadIndex::from_graph(&g, &repo_dir),
                ..Default::default()
            }
            .save(&store, &repo_dir)
            .unwrap();
            apg::testutil::touch_db(&apg_root);
            (dir, repo_dir, apg_root, repo)
        };

        // (1) Renaming F to G in a.ps1 widens to every ps file, nothing else.
        {
            let (dir, repo_dir, apg_root, repo) = fixture("rename");
            std::fs::write(repo_dir.join("a.ps1"), "function G { 1 }\n").unwrap();
            commit(&repo, "rename F to G");

            let prepared = prepare(&repo_dir, &apg_root, &config, false);
            assert!(prepared.full_scan.is_none(), "{:?}", prepared.full_scan);
            assert_eq!(
                prepared.targets_rel, ps_files,
                "every ps file, no x.go/README.md"
            );
            assert!(
                prepared
                    .reuse_candidates
                    .iter()
                    .all(|r| !ps_files.contains(&r.rel)),
                "no ps file is reused: {:?}",
                prepared.reuse_candidates
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // (2) A non-ps change (README.md, then x.go) pulls in no ps file, and
        // every ps file stays a reuse candidate.
        for changed in ["README.md", "x.go"] {
            let (dir, repo_dir, apg_root, repo) = fixture(&changed.replace('.', "-"));
            std::fs::write(repo_dir.join(changed), "// changed\n").unwrap();
            commit(&repo, "non-ps change");

            let prepared = prepare(&repo_dir, &apg_root, &config, false);
            assert!(prepared.full_scan.is_none(), "{:?}", prepared.full_scan);
            assert!(
                prepared.targets_rel.contains(changed),
                "{changed} must be a target: {:?}",
                prepared.targets_rel
            );
            assert!(
                prepared.targets_rel.is_disjoint(&ps_files),
                "a {changed} change must pull in no ps file: {:?}",
                prepared.targets_rel
            );
            let reused: BTreeSet<String> = prepared
                .reuse_candidates
                .iter()
                .map(|r| r.rel.clone())
                .collect();
            assert!(
                ps_files.is_subset(&reused),
                "every ps file stays a reuse candidate: {reused:?}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// phase-04 task-12: a fresh worktree with no scan row of its own warm-
    /// assembles the graph from the FIRST worktree's committed scan row — the
    /// same tree, zero frontend spawns — while a worktree whose tree differs
    /// does not take the warm path and runs the frontends.
    ///
    /// The DEFAULT tracked-`apg/` layout is exercised: `apg/config.json` stays
    /// tracked (and `.gitignore` carries only the `apg init` scaffold), exactly
    /// as `apg init` leaves a project. The warm completeness selection ignores
    /// the tool-owned top-level `apg/` layout, so its tracked config — pruned by
    /// the structural scanner and blacklisted by the ingestor, so it never has a
    /// fact unit — does not block the warm path.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/worktrees/spawned apg/redb store); run via cargo test-e2e"]
    fn fresh_worktree_warm_assembles_from_the_shared_scan_row_with_zero_frontends() {
        use apg::testutil::{ApgCommand, Repo};

        let repo = Repo::new("warm-shared-row");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write("a/a.go", "package a\n\nfunc Leaf() int { return 1 }\n");
        repo.write(
            "b/b.go",
            "package b\n\nimport \"scratch/a\"\n\nfunc Use() int { return a.Leaf() }\n",
        );
        repo.commit_all("source");
        assert!(repo.is_clean(), "the fixture tree must be clean");
        // Prove the arm runs on the default initialized layout: the config is
        // tracked content that the warm selection must ignore.
        {
            let git = git2::Repository::open(&repo.root).unwrap();
            let index = git.index().unwrap();
            assert!(
                index
                    .get_path(std::path::Path::new("apg/config.json"), 0)
                    .is_some(),
                "the default layout's apg/config.json must stay tracked"
            );
        }

        let home = std::env::temp_dir().join(format!("apg-warm-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let home_s = home.to_string_lossy().into_owned();
        let scan = |dir: &std::path::Path| -> std::process::Output {
            ApgCommand::new(&["scan", "."])
                .cwd(dir)
                .env("HOME", &home_s)
                .output()
        };

        // The first worktree records the shared scan row.
        let cold = scan(&repo.root);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let main_ids = code_identities(&repo.root);
        let store_root = repo.root.join(".git/apg");
        let store = FactStore::at(store_root.clone()).load();
        let main_id = worktree_id(&repo.root);
        assert!(store.scan(main_id).is_some(), "the first worktree's row");
        assert_eq!(store.scans().len(), 1, "one committed scan row so far");
        drop(store);

        // A second worktree at the same content, with no scan row of its own.
        let wt = repo.start_project("wt2");
        let wt_id = worktree_id(&wt);
        assert_ne!(wt_id, main_id, "a distinct worktree id");
        let store = FactStore::at(store_root.clone()).load();
        assert!(
            store.scan(wt_id).is_none(),
            "the fresh worktree has no own scan row"
        );
        drop(store);

        let fresh = scan(&wt);
        let fresh_err = String::from_utf8_lossy(&fresh.stderr).into_owned();
        assert!(fresh.status.success(), "worktree scan: {fresh_err}");
        assert!(
            !fresh_err.contains("[scan] running"),
            "the warm scan must spawn no frontend: {fresh_err}"
        );
        assert!(
            fresh_err.contains("warm cache") && fresh_err.contains("frontends skipped"),
            "the warm/frontends-skipped marker must be reported: {fresh_err}"
        );
        assert_eq!(
            code_identities(&wt),
            main_ids,
            "the warm assembly reproduces the same tree's graph"
        );
        let store = FactStore::at(store_root.clone()).load();
        assert!(
            store.scan(wt_id).is_some(),
            "the warm scan records the reading worktree's own row"
        );
        drop(store);

        // A worktree whose tree DIFFERS does not warm: it runs the frontends.
        let other = repo.start_project("wt3");
        std::fs::create_dir_all(other.join("c")).unwrap();
        std::fs::write(
            other.join("c/c.go"),
            "package c\n\nfunc N() int { return 3 }\n",
        )
        .unwrap();
        let diff = scan(&other);
        let diff_err = String::from_utf8_lossy(&diff.stderr).into_owned();
        assert!(diff.status.success(), "different-tree scan: {diff_err}");
        assert!(
            !diff_err.contains("warm cache"),
            "a different tree must not take the warm path: {diff_err}"
        );
        assert!(
            diff_err.contains("[scan] running"),
            "a different tree runs the frontends: {diff_err}"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }
}
