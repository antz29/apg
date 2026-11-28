mod common;

use apg::cache::*;
use apg::graph::{Graph, Location, Node, NodeKind};
use redb::{ReadableDatabase, ReadableTable};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Shared e2e helpers (file scope, outside the tier, as the layout requires):
// every one does real I/O, so only the `e2e` tier calls them.
// ---------------------------------------------------------------------------

/// A scratch isolated `HOME` for a spawned `apg` — never the real one.
fn scratch_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("apg-cache-home-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// Runs `apg scan .` in `dir` with an isolated `HOME`, capturing its output.
fn scan_in(dir: &Path, home: &str) -> std::process::Output {
    apg::testutil::ApgCommand::new(&["scan", "."])
        .cwd(dir)
        .env("HOME", home)
        .output()
}

/// The code identities a scan's database carries — the oracle a rebuilt store
/// must reproduce. Read from `db.lbug`, never a JSONL export.
fn code_identities(dir: &Path) -> std::collections::BTreeSet<String> {
    let db_path = dir.join("apg/.trans/db.lbug");
    let db = lbug::Database::new(&db_path, apg::testutil::fixture_db_config().read_only(true))
        .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
    let conn = lbug::Connection::new(&db).unwrap();
    let mut ids = std::collections::BTreeSet::new();
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

/// Opens a fact store directly, with the same builder settings as
/// `src/cache/store.rs` (`Store::database`).
fn open_store_db(path: &Path) -> redb::Database {
    redb::Database::builder()
        .set_concurrency_mode(redb::ConcurrencyMode::MultiWriter)
        .set_check_integrity_on_open(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()))
}

/// The facts format marker the store records (redb has no `user_version`, so
/// the `meta` row is it).
fn stored_format(path: &Path) -> Option<u32> {
    let db = open_store_db(path);
    let txn = db.begin_read().unwrap();
    let table = txn
        .open_table(redb::TableDefinition::<&str, u32>::new("meta"))
        .unwrap();
    table.get("format").unwrap().map(|guard| guard.value())
}

/// Overwrites the facts format marker in one committed write transaction.
fn overwrite_format_marker(path: &Path, format: u32) {
    let db = open_store_db(path);
    let txn = db.begin_write().unwrap();
    {
        let mut table = txn
            .open_table(redb::TableDefinition::<&str, u32>::new("meta"))
            .unwrap();
        table.insert("format", format).unwrap();
    }
    txn.commit().unwrap();
}

/// Every stored fact-unit key in the redb `units` table (the five-part key is
/// postcard-encoded, exactly as `src/cache/store.rs` writes it).
fn stored_unit_keys(path: &Path) -> Vec<UnitKey> {
    let db = open_store_db(path);
    let txn = db.begin_read().unwrap();
    let table = match txn.open_table(redb::TableDefinition::<&[u8], &[u8]>::new("units")) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Vec::new(),
        Err(err) => panic!("open the units table: {err}"),
    };
    let mut keys = Vec::new();
    for row in table.iter().unwrap() {
        let (key, _) = row.unwrap();
        keys.push(postcard::from_bytes::<UnitKey>(key.value()).unwrap());
    }
    keys
}

/// Every `(worktree id, recorded unix seconds)` row in the real `intents` table
/// of a `facts.db` on disk — the table `cmd_scan` writes before its first reuse
/// read and clears after the record commit, read straight from the redb file
/// rather than through the `FactStore` facade. `None` while the store does not
/// exist yet or redb's open is momentarily behind the writer, so a caller can
/// poll.
fn intent_rows(db_path: &Path) -> Option<Vec<(Vec<u8>, u64)>> {
    if !db_path.exists() {
        return None;
    }
    let db = redb::Database::builder()
        .set_concurrency_mode(redb::ConcurrencyMode::MultiWriter)
        .open(db_path)
        .ok()?;
    let txn = db.begin_read().ok()?;
    let table = match txn.open_table(redb::TableDefinition::<&[u8], u64>::new("intents")) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Some(Vec::new()),
        Err(_) => return None,
    };
    let mut rows = Vec::new();
    for row in table.iter().ok()? {
        let (key, at) = row.ok()?;
        rows.push((key.value().to_vec(), at.value()));
    }
    Some(rows)
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
        // Commit the write round: the facade's reads see committed state only.
        store.save_index().unwrap();

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
        let _ = std::fs::remove_dir_all(&dir);
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
        store.save_index().unwrap();

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

    /// The per-language scaffolding carries the pure-intermediate modules, the
    /// file-less descendants, and every `Module -> Module` edge a per-file fact
    /// unit cannot — while planned placeholders and file-less components are
    /// excluded — and it round-trips through the store under a cache key. It
    /// also carries the `Language` root and one `Language -> Module` edge for
    /// every module of a component, the intermediate and file-less ones
    /// included.
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
        // The derived `Language` root and one `Language -> Module` edge for
        // every module, the file-bearing child included.
        assert_eq!(go.languages, vec!["go".to_string()]);
        assert_eq!(
            go.language_edges,
            vec![
                ("go".to_string(), "godemo".to_string()),
                ("go".to_string(), "godemo/changed".to_string()),
            ]
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
        // The `Language` root and a `Language -> Module` edge for every module:
        // the two pure-intermediate modules and the file-less descendant
        // included, though only the leaf module owns a file.
        assert_eq!(cs.languages, vec!["csharp".to_string()]);
        assert_eq!(
            cs.language_edges,
            vec![
                ("csharp".to_string(), "Apg".to_string()),
                ("csharp".to_string(), "Apg.CsharpFrontend".to_string()),
                ("csharp".to_string(), "Apg.CsharpFrontend.Tests".to_string()),
                (
                    "csharp".to_string(),
                    "Apg.CsharpFrontend.Tests.Inline".to_string()
                ),
            ]
        );

        store.put_scaffolding_all(&by_lang, &cache_key).unwrap();
        store.save_index().unwrap();
        assert_eq!(store.scaffolding("csharp", &cache_key).as_ref(), Some(cs));
        // The `Language` root and every module's edge survive the round-trip.
        let replayed_go = store
            .scaffolding("go", &cache_key)
            .expect("go scaffolding round-trips");
        assert_eq!(&replayed_go, go);
        // A drifted key sees nothing (the store never mis-reads an old unit).
        let drifted = CacheKey::compute(&ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        });
        assert!(store.scaffolding("csharp", &drifted).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Cross-worktree cached reuse is repo-relative. The cold main-checkout
    /// scan records the shared cache; a second worktree at the same commit
    /// reuses it, and the `db.lbug` each scan built carries exactly the SAME
    /// repo-relative code identities — no checkout root appears in either
    /// database, so the cached checkout-relative model now shares the
    /// database's repo-relative base. Candidate binary only, scratch repo.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/spawned apg/db.lbug); run via cargo test-e2e"]
    fn cross_worktree_cached_reuse_is_repo_relative() {
        use apg::testutil::{ApgCommand, Repo, fixture_db_config};
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
        // The identities come from the database each scan built, never the
        // JSONL export.
        let code_identities = |dir: &Path| -> std::collections::BTreeSet<String> {
            let db_path = dir.join("apg/.trans/db.lbug");
            let db = lbug::Database::new(&db_path, fixture_db_config().read_only(true))
                .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
            let conn = lbug::Connection::new(&db).unwrap();
            let mut ids = std::collections::BTreeSet::new();
            for (label, property) in [
                ("Module", "fqn"),
                ("File", "fqn"),
                ("Struct", "path"),
                ("Function", "path"),
            ] {
                let (_, rows) = apg::splice::query_rows(
                    &conn,
                    &format!("MATCH (n:{label}) RETURN n.{property}"),
                )
                .unwrap();
                ids.extend(rows.into_iter().filter_map(|row| row.into_iter().next()));
            }
            ids
        };

        // The cold main-checkout scan records the shared store: one redb
        // database at the git common dir, shared by every worktree.
        let cold = scan(&repo.root);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let main_ids = code_identities(&repo.root);
        assert!(
            main_ids.contains("a/a.go"),
            "repo-relative File identity: {main_ids:?}"
        );
        let shared_db = repo.root.join(".git/apg/facts.db");
        let store_meta = std::fs::metadata(&shared_db)
            .unwrap_or_else(|e| panic!("the shared fact store {}: {e}", shared_db.display()));
        assert!(
            store_meta.is_file() && store_meta.len() > 0,
            "the shared fact store must hold the recorded facts after a scan"
        );

        // A clean second worktree at the same commit reuses that store, and
        // its scan's database carries the SAME identities.
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
        let wt_ids = code_identities(&wt);
        for id in &wt_ids {
            assert!(!id.starts_with('/'), "no absolute identity: {id}");
            assert!(
                !id.contains(&repo.root.to_string_lossy().into_owned()),
                "no checkout component: {id}"
            );
        }
        assert_eq!(
            wt_ids, main_ids,
            "cross-worktree identities must be identical"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git); run via cargo test-e2e"]
    fn manifest_build_matches_walk_files_over_full_fixture_shape() {
        use apg::testutil::Repo;

        let repo = Repo::new("manifest-build");

        // Base commit: a plain file, a symlink (created with std, not a new
        // testutil surface), a nested file for the scan-subdirectory arm, and
        // the ignore rules the later arms need.
        repo.write("committed.txt", "committed\n");
        repo.write("sub/a.rs", "fn a() {}\n");
        repo.write(
            ".gitignore",
            "apg/.trans/\napg/.worktrees/\napg/.scratch/\nignored.txt\nignored_dir/\n",
        );
        std::os::unix::fs::symlink("committed.txt", repo.root.join("link.txt")).unwrap();
        repo.write("modified.txt", "before\n");
        repo.write("deleted.txt", "gone soon\n");
        let base = repo.commit_all("fixture base");
        assert_eq!(repo.head_sha(), base, "the base fixture must be committed");

        // A worktree-only (unstaged) modification of a tracked file.
        repo.write("modified.txt", "after\n");
        // A tracked file deleted from the worktree.
        std::fs::remove_file(repo.root.join("deleted.txt")).unwrap();
        // A staged addition whose working tree matches the index.
        repo.write("staged.txt", "staged\n");
        // A path staged AND worktree-modified: its manifest value must be the
        // working-tree hash, not the index blob OID (the keying discriminator).
        repo.write("both.txt", "index version\n");
        let git = git2::Repository::open(&repo.root).unwrap();
        let mut index = git.index().unwrap();
        index.add_path(Path::new("staged.txt")).unwrap();
        index.add_path(Path::new("both.txt")).unwrap();
        index.write().unwrap();
        repo.write("both.txt", "worktree version\n");

        // Untracked root and nested files, a gitignored file, a gitignored
        // nested directory, and an unignored node_modules-style directory
        // (the SKIP_DIRS pruning arm).
        repo.write("untracked.txt", "untracked\n");
        repo.write("nested/untracked.txt", "nested untracked\n");
        repo.write("ignored.txt", "ignored\n");
        repo.write("ignored_dir/keep.txt", "ignored dir\n");
        repo.write("node_modules/pkg.js", "module.exports = 1;\n");
        assert!(
            repo.root.join("node_modules/pkg.js").is_file(),
            "the SKIP_DIRS fixture file must exist"
        );

        // The canonical root the manifest and the baseline both key against.
        let root = std::fs::canonicalize(&repo.root).unwrap();
        let git = git2::Repository::open(&root).unwrap();
        let manifest = Manifest::build(&root);
        let mut baseline = std::collections::BTreeMap::new();
        walk_files(&root, &root, Some(&git), &mut baseline);
        assert_eq!(
            manifest.entries, baseline,
            "Manifest::build must equal the walk_files baseline entry for entry"
        );

        // The arms whose intent an entry-for-entry diff alone understates.
        assert!(
            !manifest.entries.contains_key("deleted.txt"),
            "a worktree-deleted tracked path must be absent"
        );
        assert!(
            !manifest.entries.contains_key("node_modules/pkg.js"),
            "a SKIP_DIRS directory must be pruned"
        );
        assert!(manifest.entries.contains_key("link.txt"));

        // A scan rooted at a repo subdirectory keeps scan-root-relative keys.
        let sub = std::fs::canonicalize(repo.root.join("sub")).unwrap();
        let sub_manifest = Manifest::build(&sub);
        let mut sub_baseline = std::collections::BTreeMap::new();
        walk_files(&sub, &sub, Some(&git), &mut sub_baseline);
        assert_eq!(sub_manifest.entries, sub_baseline);
        assert!(sub_manifest.entries.contains_key("a.rs"));
        assert!(!sub_manifest.entries.contains_key("sub/a.rs"));

        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// The store is ONE redb database per repository at
    /// `<git-common-dir>/apg/facts.db`, beside its `facts.lock` writer gate,
    /// shared by every worktree and branch — never a per-worktree copy.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/worktrees/spawned apg); run via cargo test-e2e"]
    fn one_store_file_shared_by_every_worktree() {
        use apg::testutil::Repo;

        let repo = Repo::new("store-one-file");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");
        let home = scratch_home("one-store");
        let home_s = home.to_string_lossy().into_owned();

        let cold = scan_in(&repo.root, &home_s);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let shared_db = repo.root.join(".git/apg/facts.db");
        let meta = std::fs::metadata(&shared_db)
            .unwrap_or_else(|e| panic!("the shared store {}: {e}", shared_db.display()));
        assert!(
            meta.is_file() && meta.len() > 0,
            "the shared store is one non-empty redb file at {}",
            shared_db.display()
        );
        assert!(
            repo.root.join(".git/apg/facts.lock").exists(),
            "the writer gate sits beside facts.db"
        );

        // A second worktree at the same commit scans off the SAME one store.
        let wt = repo.start_project("wt");
        let fresh = scan_in(&wt, &home_s);
        assert!(
            fresh.status.success(),
            "worktree scan: {}",
            String::from_utf8_lossy(&fresh.stderr)
        );
        let meta = std::fs::metadata(&shared_db)
            .unwrap_or_else(|e| panic!("the shared store after a worktree scan: {e}"));
        assert!(
            meta.is_file() && meta.len() > 0,
            "the shared store still serves the worktree"
        );

        // ...and no worktree keeps its own facts.db anywhere.
        for stray in [
            wt.join("apg").join("facts.db"),
            wt.join("apg/.trans/facts.db"),
            repo.root.join(".git/worktrees/wt/apg/facts.db"),
        ] {
            assert!(
                !stray.exists(),
                "a worktree never keeps a local store: {}",
                stray.display()
            );
        }

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The decisive two-process case
    /// (`requirements.requirement.concurrent-fact-writers-wait-then-fail-named`):
    /// a scan that finds another process writing the store waits the bounded
    /// budget, then exits NON-ZERO with the named contention on stderr; a scan
    /// that acquires the gate after the release succeeds.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/two processes/facts.lock); run via cargo test-e2e"]
    fn concurrent_writers_wait_then_fail_with_the_named_contention_error() {
        use apg::testutil::Repo;

        let repo = Repo::new("store-contention");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");
        let home = scratch_home("contention");
        let home_s = home.to_string_lossy().into_owned();

        let cold = scan_in(&repo.root, &home_s);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );

        // A source edit so the contending child cannot take the unchanged-tree
        // fast path (which returns BEFORE the contention probe).
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 2.\nfunc Leaf() int { return 2 }\n",
        );

        // Hold the external writer gate: the write transaction owns
        // `<store>/facts.lock` for its lifetime.
        let store_path = repo.root.join(".git/apg/facts.db");
        let store = Store::open(&store_path).unwrap();
        let held = store.begin_write().unwrap();

        let mut child = apg::testutil::ApgCommand::new(&["scan", "."])
            .cwd(&repo.root)
            .env("HOME", &home_s)
            .spawn();
        let started = std::time::Instant::now();
        let bound = std::time::Duration::from_secs(45);
        let status = loop {
            match child.try_wait().unwrap() {
                Some(status) => break status,
                None => {
                    if started.elapsed() >= bound {
                        let _ = child.kill();
                        let out = child.wait_with_output().unwrap();
                        panic!(
                            "the contended scan was still running after {bound:?} — the \
                             facts.lock gate is not gating; stderr: {}",
                            String::from_utf8_lossy(&out.stderr)
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        };
        let elapsed = started.elapsed();
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !status.success(),
            "the contended scan must exit non-zero; stderr: {stderr}"
        );
        for needle in ["held by another writer", "facts.db", "waited"] {
            assert!(
                stderr.contains(needle),
                "the contention error must name {needle:?}; stderr: {stderr}"
            );
        }
        eprintln!(
            "contended scan: exit {:?} after {elapsed:?}: {}",
            status.code(),
            stderr
                .lines()
                .find(|line| line.contains("held by another writer"))
                .unwrap_or("")
        );

        // Release the gate (dropping the transaction aborts it) and rescan.
        drop(held);
        let retry = scan_in(&repo.root, &home_s);
        let retry_err = String::from_utf8_lossy(&retry.stderr).into_owned();
        assert!(
            retry.status.success(),
            "the scan after the release must succeed: {retry_err}"
        );
        assert!(
            !retry_err.contains("held by another writer"),
            "the released gate is not reported as contention: {retry_err}"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A format-marker mismatch drops and rebuilds the store on open: the scan
    /// re-emits the facts rather than warming from incompatible rows, and the
    /// graph equals a cold scan's.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/redb store/spawned apg); run via cargo test-e2e"]
    fn wrong_format_marker_rebuilds_the_store_on_open() {
        use apg::testutil::Repo;

        let repo = Repo::new("store-format");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");
        let home = scratch_home("store-format");
        let home_s = home.to_string_lossy().into_owned();

        let cold = scan_in(&repo.root, &home_s);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let oracle = code_identities(&repo.root);
        let db = repo.root.join(".git/apg/facts.db");
        assert_eq!(
            stored_format(&db),
            Some(FACTS_FORMAT_VERSION),
            "the cold scan stamps the current facts format"
        );
        let stale = stored_unit_keys(&db)
            .into_iter()
            .find(|key| key.rel == "a/a.go")
            .expect("the cold scan records a/a.go's fact unit");
        let stale_oid = stale.oid;

        // A foreign/older writer's marker: the store is no longer current.
        overwrite_format_marker(&db, 999);

        // A body edit changes the blob OID (so no old-format row can satisfy the
        // reuse) while every code identity stays the same.
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 2.\nfunc Leaf() int { return 2 }\n",
        );

        let rebuilt = scan_in(&repo.root, &home_s);
        assert!(
            rebuilt.status.success(),
            "rebuild scan: {}",
            String::from_utf8_lossy(&rebuilt.stderr)
        );
        assert_eq!(
            stored_format(&db),
            Some(FACTS_FORMAT_VERSION),
            "the rebuild stamps the current facts format again"
        );
        let keys = stored_unit_keys(&db);
        assert!(
            !keys.iter().any(|key| key.oid == stale_oid),
            "the rebuilt store holds no pre-rebuild fact: {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|key| key.rel == "a/a.go" && key.oid != stale_oid),
            "the rebuild's scan re-records a/a.go under the new blob OID: {keys:?}"
        );
        assert_eq!(
            code_identities(&repo.root),
            oracle,
            "the rebuilt store yields the cold-scan identities"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An externally corrupted `facts.db` (integrity failure on open) is rebuilt
    /// just like a stale format marker: the scan succeeds at the cost of one full
    /// scan, and the graph equals a cold scan's.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/redb store/spawned apg); run via cargo test-e2e"]
    fn corrupt_facts_db_is_rebuilt_on_open() {
        use apg::testutil::Repo;

        let repo = Repo::new("store-corrupt");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");
        let home = scratch_home("store-corrupt");
        let home_s = home.to_string_lossy().into_owned();

        let cold = scan_in(&repo.root, &home_s);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let oracle = code_identities(&repo.root);
        let db = repo.root.join(".git/apg/facts.db");
        let stale = stored_unit_keys(&db)
            .into_iter()
            .find(|key| key.rel == "a/a.go")
            .expect("the cold scan records a/a.go's fact unit");
        let stale_oid = stale.oid;

        // Truncated/foreign bytes where the database should be.
        std::fs::write(&db, vec![0xA5u8; 4096]).unwrap();

        // Keep the tree off the unchanged-tree fast path; the body edit keeps
        // the identities intact while changing the blob OID.
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 2.\nfunc Leaf() int { return 2 }\n",
        );

        let rebuilt = scan_in(&repo.root, &home_s);
        assert!(
            rebuilt.status.success(),
            "the scan over a corrupt store must succeed: {}",
            String::from_utf8_lossy(&rebuilt.stderr)
        );
        let meta = std::fs::metadata(&db)
            .unwrap_or_else(|e| panic!("the store is recreated at {}: {e}", db.display()));
        assert!(meta.is_file() && meta.len() > 0, "the store is recreated");
        assert_eq!(
            stored_format(&db),
            Some(FACTS_FORMAT_VERSION),
            "the recreated store carries the current facts format"
        );
        let keys = stored_unit_keys(&db);
        assert!(
            !keys.iter().any(|key| key.oid == stale_oid),
            "the corrupt store's rows are gone: {keys:?}"
        );
        assert_eq!(
            code_identities(&repo.root),
            oracle,
            "the rebuilt store yields the cold-scan identities"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A write transaction that never commits leaves the last committed snapshot
    /// readable and never exposes its own pending rows.
    #[test]
    #[ignore = "e2e tier: real I/O (store on disk); run via cargo test-e2e"]
    fn uncommitted_write_transaction_leaves_the_previous_snapshot_readable() {
        let dir =
            std::env::temp_dir().join(format!("apg-cache-uncommitted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("facts.db")).unwrap();
        let first = [1u8; 32];
        let second = [2u8; 32];

        let txn = store.begin_write().unwrap();
        txn.put_body(first, b"first").unwrap();
        txn.commit().unwrap();

        // Begin a second transaction, write, and drop it without committing.
        let txn = store.begin_write().unwrap();
        txn.put_body(second, b"second").unwrap();
        drop(txn);

        let read = store.read().unwrap();
        assert_eq!(
            read.get_body(first).unwrap().as_deref(),
            Some(&b"first"[..]),
            "the last committed snapshot stays readable"
        );
        assert!(
            read.get_body(second).unwrap().is_none(),
            "an uncommitted write is never visible"
        );
        assert!(!read.body_exists(second).unwrap());
        drop(read);
        drop(store);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The first successful open retires the legacy `<store>/facts/` directory
    /// once (reporting the bytes reclaimed) while the checkout-local toolchain
    /// cache survives; a retirement that cannot delete it warns and continues.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/legacy dir/spawned apg); run via cargo test-e2e"]
    fn legacy_fact_directory_is_retired_once_and_the_toolchain_cache_survives() {
        use apg::testutil::Repo;
        use std::os::unix::fs::PermissionsExt;

        /// Seeds `<repo>/.git/apg/facts/` (1 KiB of known bytes) and the
        /// toolchain-cache marker, then cold-scans.
        fn seed(repo: &Repo, home: &str) -> std::process::Output {
            let legacy = repo.root.join(".git/apg/facts");
            std::fs::create_dir_all(&legacy).unwrap();
            std::fs::write(legacy.join("legacy.json"), vec![b'x'; 1024]).unwrap();
            let marker = repo.root.join("apg/.trans/cache/go/marker");
            std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
            std::fs::write(&marker, "toolchain\n").unwrap();
            scan_in(&repo.root, home)
        }

        // Arm 1: the legacy directory is retired, its bytes reported.
        let repo = Repo::new("store-legacy");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");
        let home = scratch_home("legacy");
        let home_s = home.to_string_lossy().into_owned();
        let out = seed(&repo, &home_s);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "scan: {stderr}");
        let legacy = repo.root.join(".git/apg/facts");
        assert!(!legacy.exists(), "the legacy fact directory is retired");
        assert!(
            stderr.contains("retired the legacy fact store"),
            "the retirement is reported: {stderr}"
        );
        assert!(
            stderr.contains("bytes reclaimed"),
            "the reclaimed bytes are reported: {stderr}"
        );
        assert!(
            stderr.contains("1024 bytes reclaimed"),
            "the known legacy bytes are counted: {stderr}"
        );
        assert!(
            repo.root.join("apg/.trans/cache/go/marker").is_file(),
            "the per-worktree toolchain cache is not the fact store and survives"
        );
        assert!(
            repo.root.join(".git/apg/facts.db").is_file(),
            "the redb store stands in for the retired directory"
        );
        eprintln!(
            "legacy retirement: {}",
            stderr
                .lines()
                .find(|line| line.contains("bytes reclaimed"))
                .unwrap_or("")
        );

        // Arm 2: a legacy directory that cannot be deleted warns and continues.
        let guarded = Repo::new("store-legacy-readonly");
        guarded.write("go.mod", "module scratch\n\ngo 1.21\n");
        guarded.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        guarded.commit_all("source");
        let home2 = scratch_home("legacy-readonly");
        let home2_s = home2.to_string_lossy().into_owned();
        let legacy2 = guarded.root.join(".git/apg/facts");
        let marker2 = guarded.root.join("apg/.trans/cache/go/marker");
        std::fs::create_dir_all(&legacy2).unwrap();
        std::fs::write(legacy2.join("legacy.json"), "legacy\n").unwrap();
        std::fs::create_dir_all(marker2.parent().unwrap()).unwrap();
        std::fs::write(&marker2, "toolchain\n").unwrap();
        // A read-only directory: a non-root process cannot unlink its contents.
        std::fs::set_permissions(&legacy2, std::fs::Permissions::from_mode(0o555)).unwrap();
        let out = scan_in(&guarded.root, &home2_s);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "the warned scan still succeeds: {stderr}"
        );
        if stderr.contains("could not retire") {
            assert!(
                legacy2.is_dir(),
                "a refused retirement leaves the legacy store in place"
            );
            assert!(
                marker2.is_file(),
                "the toolchain cache survives a refused retirement"
            );
        }
        // else: root could delete it anyway — the removal arm is accepted and
        // this read-only arm is skipped; never a test failure.
        eprintln!(
            "read-only legacy arm: {}",
            if stderr.contains("could not retire") {
                "warned and survived"
            } else {
                "removed anyway (running as root?) — arm skipped"
            }
        );
        assert!(
            guarded.root.join(".git/apg/facts.db").is_file(),
            "the store is created regardless of the legacy directory"
        );

        // Restore permissions so cleanup can remove the scratch repo.
        std::fs::set_permissions(&legacy2, std::fs::Permissions::from_mode(0o755)).ok();
        let _ = std::fs::remove_dir_all(&guarded.root);
        let _ = std::fs::remove_dir_all(&home2);
        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// One redb store holds each worktree's OWN `refs`/scan/delta rows, and a
    /// unit is selected on its five-part `(format, config, language,
    /// repo-relative path, blob OID)` identity alone — the recorded `inputs`
    /// diagnostic never gates the OID-only `candidate`/`has_unit` selection,
    /// while the inputs-gated `has`/`reuse` still does. The scan row is
    /// manifest-free — it carries only its `sha`/`cache_key`/`content_key`, and
    /// each worktree's tree is asserted through its own refs rows.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/worktrees/redb store); run via cargo test-e2e"]
    fn each_worktree_keeps_its_rows_and_units_reuse_on_the_oid_alone() {
        use apg::delta::ScanRecord;
        use apg::testutil::Repo;

        let repo = Repo::new("store-worktree-rows");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write("a/a.go", "package a\n\nfunc Leaf() int { return 1 }\n");
        repo.commit_all("source");
        let home = scratch_home("store-worktree-rows");
        let home_s = home.to_string_lossy().into_owned();

        let cold = scan_in(&repo.root, &home_s);
        assert!(
            cold.status.success(),
            "cold scan: {}",
            String::from_utf8_lossy(&cold.stderr)
        );

        // A second worktree at the same commit, given a DIFFERENT tree (an
        // untracked file that calls into the leaf), scanned on its own.
        let wt = repo.start_project("wt");
        std::fs::create_dir_all(wt.join("b")).unwrap();
        std::fs::write(
            wt.join("b/b.go"),
            "package b\n\nimport \"scratch/a\"\n\nfunc Use() int { return a.Leaf() }\n",
        )
        .unwrap();
        let fresh = scan_in(&wt, &home_s);
        assert!(
            fresh.status.success(),
            "worktree scan: {}",
            String::from_utf8_lossy(&fresh.stderr)
        );

        let store_root = repo.root.join(".git/apg");
        let store = FactStore::at(store_root.clone()).load();
        let main_id = apg::cache::worktree_id(&repo.root);
        let wt_id = apg::cache::worktree_id(&wt);
        assert_ne!(main_id, wt_id, "each checkout has its own worktree id");

        // (1) refs: each worktree's own manifest rows, matching its own tree.
        let main_refs = store.refs(main_id);
        let wt_refs = store.refs(wt_id);
        assert_eq!(
            main_refs,
            Manifest::build(&repo.root).entries,
            "the main worktree's refs are its tree"
        );
        assert_eq!(
            wt_refs,
            Manifest::build(&wt).entries,
            "the worktree's refs are its own tree"
        );
        assert!(
            main_refs.contains_key("a/a.go") && !main_refs.contains_key("b/b.go"),
            "the main tree has a/a.go and no b/b.go: {main_refs:?}"
        );
        assert!(
            wt_refs.contains_key("b/b.go"),
            "the worktree's refs carry its added file: {wt_refs:?}"
        );
        assert_ne!(main_refs, wt_refs, "the two trees' refs differ");

        // The scan rows: each worktree keeps its OWN row. The row is
        // manifest-free — it carries only the commit it ran at, its cache key
        // and its content-identity key — because a worktree's refs rows (above)
        // ARE its manifest, so a decoded row's manifest is empty.
        let main_row = store.scan(main_id).expect("the main scan row");
        let wt_row = store.scan(wt_id).expect("the worktree scan row");
        assert_ne!(main_row, wt_row, "the two scan rows differ");
        let main_record: ScanRecord =
            postcard::from_bytes(&main_row).expect("the main scan row decodes");
        let wt_record: ScanRecord = postcard::from_bytes(&wt_row).expect("the wt scan row decodes");
        let head = repo.head_sha();
        assert_eq!(main_record.sha, head, "the main row records its commit");
        assert_eq!(wt_record.sha, head, "the wt row records the same commit");
        assert_eq!(
            main_record.cache_key, wt_record.cache_key,
            "both scans ran under the same cache key"
        );
        assert!(
            main_record.content_key.is_some() && wt_record.content_key.is_some(),
            "each row records a content identity"
        );
        assert_ne!(
            main_record.content_key, wt_record.content_key,
            "each row records its OWN tree's content identity"
        );
        assert!(
            main_record.manifest.entries.is_empty()
                && main_record.manifest.root.is_empty()
                && wt_record.manifest.entries.is_empty()
                && wt_record.manifest.root.is_empty(),
            "the row carries no per-file manifest: the refs rows are the manifest"
        );
        let ids: std::collections::BTreeSet<Vec<u8>> = store
            .scans()
            .into_iter()
            .map(|(id, _)| id.to_vec())
            .collect();
        assert_eq!(
            ids,
            [main_id.to_vec(), wt_id.to_vec()].into_iter().collect(),
            "exactly one scan row per worktree"
        );

        // The delta rows: each worktree's own file-dependency index, matching
        // its tree (the worktree's b/b.go → a/a.go edge only exists in its row).
        let main_deps: Vec<(String, String)> =
            postcard::from_bytes(&store.deps(main_id).expect("the main worktree delta row"))
                .unwrap();
        let wt_deps: Vec<(String, String)> =
            postcard::from_bytes(&store.deps(wt_id).expect("the worktree delta row")).unwrap();
        assert!(
            main_deps.is_empty(),
            "the main tree has no cross-file dependency: {main_deps:?}"
        );
        assert_eq!(
            wt_deps,
            vec![("b/b.go".to_string(), "a/a.go".to_string())],
            "the worktree's delta rows are its own"
        );
        for (label, row) in [
            ("signatures", store.signatures(main_id)),
            ("overloads", store.overloads(main_id)),
            ("signatures", store.signatures(wt_id)),
            ("overloads", store.overloads(wt_id)),
        ] {
            assert!(row.is_some(), "each worktree keeps its own {label} row");
        }
        drop(store);

        // (2) The OID-only selection: a unit recorded with one `inputs`
        // diagnostic is selected on its five-part identity alone, while the
        // inputs-gated read still refuses a drifted digest.
        let key = CacheKey::compute(&ScanConfigKey::default());
        let mut unit_store = FactStore::at(store_root).load();
        let mut g = Graph::default();
        g.nodes.insert(
            "fixture.mod".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/w/diag.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: PathBuf::from("/w/diag.go"),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 3,
                }),
                ..Node::default()
            },
        );
        g.contains
            .insert(("fixture.mod".to_string(), "/w/diag.go".to_string()));
        let frag = FileFragment::from_graph(&g, "/w/diag.go", "diag/a.go", "oid-diag", "go");
        let recorded_inputs = frag.inputs_digest();
        assert!(!recorded_inputs.is_empty(), "the unit records inputs");
        unit_store.put(&frag, "/w", &key).unwrap();
        unit_store.save_index().unwrap();

        // The OID-only selection returns it regardless of the inputs column.
        assert!(unit_store.has_unit("go", "diag/a.go", "oid-diag", &key));
        let (candidate, root) = unit_store
            .candidate("go", "diag/a.go", "oid-diag", &key)
            .expect("the OID-only candidate is selected");
        assert_eq!(root, "/w");
        assert_eq!(candidate.rel_path, "diag/a.go");

        // ...while a DIFFERENT inputs digest fails the inputs-gated read: the
        // digest is diagnostic, never part of the key.
        assert!(unit_store.has("go", "diag/a.go", "oid-diag", &recorded_inputs, &key));
        assert!(
            !unit_store.has(
                "go",
                "diag/a.go",
                "oid-diag",
                "a-different-inputs-digest",
                &key
            ),
            "a drifted inputs diagnostic is not reusable through `has`"
        );
        assert!(
            unit_store
                .reuse(
                    "go",
                    "diag/a.go",
                    "oid-diag",
                    "a-different-inputs-digest",
                    &key
                )
                .is_none(),
            "a drifted inputs diagnostic is not reusable through `reuse`"
        );
        assert!(
            unit_store
                .reuse("go", "diag/a.go", "oid-diag", &recorded_inputs, &key)
                .is_some(),
            "the recorded inputs digest still reuses"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The delta refs relay writes only the changed rows — a
    /// one-file delta upserts exactly that path's ref row and removes a vanished
    /// path's row, leaving every other path's row and a sibling worktree's rows
    /// untouched (no wholesale delete-plus-insert of the worktree's manifest).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/sibling checkout/redb store); run via cargo test-e2e"]
    fn delta_refs_relay_touches_only_the_changed_rows() {
        use apg::testutil::Repo;
        use std::collections::{BTreeMap, BTreeSet};

        let repo = Repo::new("delta-refs-relay");
        // A sibling checkout OUTSIDE the repo, so it resolves its own worktree
        // id rather than the repo's canonical workdir.
        let sibling = std::env::temp_dir().join(format!(
            "apg-cache-sibling-{}-delta-refs-relay",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&sibling);
        std::fs::create_dir_all(&sibling).unwrap();
        let main_id = worktree_id(&repo.root);
        let sibling_id = worktree_id(&sibling);
        assert_ne!(main_id, sibling_id, "each checkout has its own worktree id");

        let refs = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(path, oid)| (path.to_string(), oid.to_string()))
                .collect()
        };
        let store = FactStore::at(repo.root.join(".git/apg"));
        store
            .put_refs(
                main_id,
                &refs(&[("a.go", "oid-a"), ("b.go", "oid-b"), ("c.go", "oid-c")]),
            )
            .unwrap();
        store
            .put_refs(sibling_id, &refs(&[("s.go", "oid-s")]))
            .unwrap();
        let sibling_scan = b"sibling scan row".to_vec();
        store.put_scan(sibling_id, &sibling_scan).unwrap();
        store.save_index().unwrap();
        assert_eq!(
            store.refs(main_id),
            refs(&[("a.go", "oid-a"), ("b.go", "oid-b"), ("c.go", "oid-c")]),
            "the baseline manifest rows"
        );

        // One file changed, one file added, one file vanished.
        store
            .apply_refs(
                main_id,
                &refs(&[("b.go", "oid-b2"), ("d.go", "oid-d")]),
                &BTreeSet::from(["a.go".to_string()]),
            )
            .unwrap();
        store.save_index().unwrap();

        assert_eq!(
            store.refs(main_id),
            refs(&[("b.go", "oid-b2"), ("c.go", "oid-c"), ("d.go", "oid-d")]),
            "only b.go is upserted (and d.go added), a.go removed, c.go untouched"
        );
        assert_eq!(
            store.refs(sibling_id),
            refs(&[("s.go", "oid-s")]),
            "the sibling worktree's refs rows are untouched"
        );
        assert_eq!(
            store.scan(sibling_id).as_deref(),
            Some(sibling_scan.as_slice()),
            "the sibling worktree's scan row is untouched"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    /// A body written one minute ago — inside the intent grace
    /// window — is rooted by its worktree's live intent even though no manifest
    /// reaches it, so a collection leaves the body and its unit intact. An
    /// unrooted orphan body proves the collector actually ran.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/redb store/git); run via cargo test-e2e"]
    fn one_minute_old_body_survives_collection() {
        use apg::testutil::Repo;

        let repo = Repo::new("gc-intent-grace");
        let store_root = repo.root.join(".git/apg");
        let db = store_root.join(FACTS_DB);
        let root = std::fs::canonicalize(&repo.root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let wt_id = worktree_id(&repo.root);
        let key = UnitKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: "cfg-grace".to_string(),
            lang: "go".to_string(),
            rel: "a/a.go".to_string(),
            oid: "oid-grace".to_string(),
        };
        let payload = b"a body written one minute ago".to_vec();
        let payload_id = body_id(&payload);
        let orphan = b"a body nothing roots".to_vec();
        let orphan_id = body_id(&orphan);

        {
            let store = Store::open(&db).unwrap();
            let txn = store.begin_write().unwrap();
            txn.put_body(payload_id, &payload).unwrap();
            txn.put_unit(&key, payload_id, "inputs", &root).unwrap();
            txn.put_body(orphan_id, &orphan).unwrap();
            txn.register_worktree(wt_id, &root).unwrap();
            // One minute old — inside INTENT_GRACE_SECS, so the scan that wrote
            // the body is still in flight and the collector must not reap it.
            txn.put_intent(wt_id, unix_now().saturating_sub(60))
                .unwrap();
            txn.commit().unwrap();
        }

        let collection = collect(&store_root, unix_now(), INTENT_GRACE_SECS).unwrap();
        assert!(
            collection.bodies.contains(&orphan_id),
            "the unrooted body is collected: {collection:?}"
        );
        assert!(
            !collection.bodies.contains(&payload_id),
            "a one-minute-old body is protected by its live intent: {collection:?}"
        );
        assert!(
            !collection.units.contains(&key),
            "its unit is not collected: {collection:?}"
        );

        let store = Store::open(&db).unwrap();
        let read = store.read().unwrap();
        assert_eq!(
            read.get_body(payload_id).unwrap().as_deref(),
            Some(&payload[..]),
            "the protected body really survives in facts.db"
        );
        assert!(
            !read.body_exists(orphan_id).unwrap(),
            "the orphan body is really gone from facts.db"
        );
        assert!(
            read.get_unit(&key).unwrap().is_some(),
            "the protected unit really survives in facts.db"
        );
        drop(read);
        drop(store);

        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// A second worktree's scan is in flight — it has committed
    /// an unreferenced body + unit (no manifest row) and holds a live intent —
    /// while a collection runs against the same store. The intent still roots
    /// the body, so the body survives; an unrooted orphan body proves the
    /// collector ran.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/worktree + redb store/git); run via cargo test-e2e"]
    fn fresh_worktrees_unreferenced_body_survives_a_concurrent_collection() {
        use apg::testutil::Repo;

        let repo = Repo::new("gc-in-flight");
        let wt = repo.start_project("wt");
        let store_root = repo.root.join(".git/apg");
        let db = store_root.join(FACTS_DB);
        let wt_root = std::fs::canonicalize(&wt)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let wt_id = worktree_id(&wt);
        let key = UnitKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: "cfg-in-flight".to_string(),
            lang: "go".to_string(),
            rel: "b/b.go".to_string(),
            oid: "oid-in-flight".to_string(),
        };
        let payload = b"a fresh worktree's unreferenced body".to_vec();
        let payload_id = body_id(&payload);
        let orphan = b"an orphan under concurrent collection".to_vec();
        let orphan_id = body_id(&orphan);

        // The second worktree's scan: write its unreferenced body + unit and a
        // live intent, then stay "in flight" (holding the store open) while the
        // collector runs.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let scan = std::thread::spawn({
            let db = db.clone();
            let wt_root = wt_root.clone();
            let key = key.clone();
            let payload = payload.clone();
            let orphan = orphan.clone();
            move || {
                let store = Store::open(&db).unwrap();
                let txn = store.begin_write().unwrap();
                txn.put_body(payload_id, &payload).unwrap();
                txn.put_unit(&key, payload_id, "inputs", &wt_root).unwrap();
                txn.put_body(orphan_id, &orphan).unwrap();
                txn.register_worktree(wt_id, &wt_root).unwrap();
                txn.put_intent(wt_id, unix_now()).unwrap();
                txn.commit().unwrap();
                ready_tx.send(()).unwrap();
                // Still in flight: keep the store in use while the collector
                // runs against the same facts.db.
                std::thread::sleep(std::time::Duration::from_millis(750));
                drop(store);
            }
        });
        ready_rx.recv().unwrap();

        let collection = collect(&store_root, unix_now(), INTENT_GRACE_SECS).unwrap();
        scan.join().unwrap();

        assert!(
            collection.bodies.contains(&orphan_id),
            "the unrooted orphan is collected: {collection:?}"
        );
        assert!(
            !collection.bodies.contains(&payload_id),
            "the in-flight worktree's body is protected by its live intent: {collection:?}"
        );
        assert!(
            !collection.units.contains(&key),
            "the in-flight unit is not collected: {collection:?}"
        );

        let store = Store::open(&db).unwrap();
        let read = store.read().unwrap();
        assert_eq!(
            read.get_body(payload_id).unwrap().as_deref(),
            Some(&payload[..]),
            "the in-flight body really survives in facts.db"
        );
        assert!(
            !read.body_exists(orphan_id).unwrap(),
            "the orphan body is really gone from facts.db"
        );
        assert!(
            read.get_unit(&key).unwrap().is_some(),
            "the in-flight unit really survives in facts.db"
        );
        drop(read);
        drop(store);

        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// The collector takes its own write transaction, so it
    /// waits on the external gate the record's held transaction owns — the
    /// record commit is never blocked by it and never loses its row. The record
    /// commit completes promptly while the collector is still waiting; the
    /// collector then completes with nothing to collect.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/redb store/git); run via cargo test-e2e"]
    fn collection_waits_for_the_record_commit_without_blocking_it() {
        use apg::testutil::Repo;

        let repo = Repo::new("gc-record-gate");
        let store_root = repo.root.join(".git/apg");
        let db = store_root.join(FACTS_DB);
        let root = std::fs::canonicalize(&repo.root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let wt_id = worktree_id(&repo.root);
        let key = UnitKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: "cfg-gate".to_string(),
            lang: "go".to_string(),
            rel: "a/a.go".to_string(),
            oid: "oid-gate".to_string(),
        };
        let payload = b"a record body committed under the gate".to_vec();
        let payload_id = body_id(&payload);
        let mut refs = std::collections::BTreeMap::new();
        refs.insert("a/a.go".to_string(), "oid-gate".to_string());

        // A record-style write transaction holds the external gate across the
        // writes; it is committed once below.
        let store = Store::open(&db).unwrap();
        let txn = store.begin_write().unwrap();
        txn.put_body(payload_id, &payload).unwrap();
        txn.put_unit(&key, payload_id, "inputs", &root).unwrap();
        txn.register_worktree(wt_id, &root).unwrap();
        txn.replace_refs(wt_id, &refs).unwrap();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let collector = std::thread::spawn({
            let store_root = store_root.clone();
            move || {
                let result = collect(&store_root, unix_now(), INTENT_GRACE_SECS);
                done_tx.send(()).unwrap();
                result
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            done_rx.try_recv().is_err(),
            "the collector must still be waiting for the record commit's gate"
        );

        let started = std::time::Instant::now();
        txn.commit().unwrap();
        let commit_elapsed = started.elapsed();
        assert!(
            commit_elapsed < std::time::Duration::from_secs(5),
            "the record commit completes promptly, not behind the collector: {commit_elapsed:?}"
        );
        drop(store);

        let collection = collector.join().unwrap().unwrap();
        assert!(
            collection.bodies.is_empty() && collection.units.is_empty(),
            "the committed record row is reached, not collected: {collection:?}"
        );

        let store = Store::open(&db).unwrap();
        let read = store.read().unwrap();
        assert_eq!(
            read.get_body(payload_id).unwrap().as_deref(),
            Some(&payload[..]),
            "the record body is not lost"
        );
        assert!(
            read.get_unit(&key).unwrap().is_some(),
            "the record unit row is not lost"
        );
        drop(read);
        drop(store);

        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// The scan-side intent lifecycle. A real CLI
    /// scan of a scratch `/tmp` repo commits its `intents` row before its first
    /// reuse read and clears it after the record commit, so the redb `intents`
    /// table is empty once the scan exits. The row is read from `facts.db`
    /// while the child is still scanning — a missing `put_intent` never shows
    /// it — and the post-exit read rejects a stale row — a missing
    /// `remove_intent` leaves one.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo/spawned apg/redb store/git); run via cargo test-e2e"]
    fn scan_writes_its_intent_then_clears_it_after_the_record_commit() {
        use apg::testutil::{ApgCommand, Repo};

        let repo = Repo::new("scan-intent-lifecycle");
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
        );
        repo.commit_all("source");
        let home = scratch_home("intent-lifecycle");
        let home_s = home.to_string_lossy().into_owned();
        let db_path = repo.root.join(".git/apg/facts.db");
        let worktree = worktree_id(&repo.root);

        // Spawn the real CLI and poll the store's own `intents` table until this
        // worktree's row is committed. `cmd_scan` holds it from before its first
        // reuse read until after `run_pipeline` returns, so it is visible while
        // the child is still running.
        let mut child = ApgCommand::new(&["scan", "."])
            .cwd(&repo.root)
            .env("HOME", &home_s)
            .spawn();
        let started = std::time::Instant::now();
        let bound = std::time::Duration::from_secs(60);
        let mut observed = None;
        loop {
            if let Some(rows) = intent_rows(&db_path)
                && let Some((_, at)) = rows.iter().find(|(id, _)| id.as_slice() == &worktree[..])
            {
                observed = Some(*at);
                break;
            }
            if started.elapsed() >= bound {
                let _ = child.kill();
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "the scan exited ({status:?}) without its intent row ever being \
                     observable in facts.db — `cmd_scan` never committed it"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "the scan must succeed before its clearing is asserted: {stderr}"
        );
        let at = observed.unwrap_or_else(|| {
            panic!(
                "the scan's intent row was never observable in facts.db within \
                 {bound:?} — `cmd_scan` did not write it before its reuse read"
            )
        });
        // The observed row is a live intent: inside the grace the collector honours.
        let now = unix_now();
        assert!(
            intent_live(at, now, INTENT_GRACE_SECS),
            "the observed intent ({at}) is live as of {now} within the {INTENT_GRACE_SECS}s grace"
        );

        // After the record commit and its own clearing transaction the table is
        // empty: no stale row survives a successful scan.
        let rows = intent_rows(&db_path).expect("facts.db is readable after the scan");
        assert!(
            !rows.iter().any(|(id, _)| id.as_slice() == &worktree[..]),
            "no stale intent row survives the scan: {rows:?}"
        );
        assert!(
            rows.is_empty(),
            "the intent table is empty after a successful scan: {rows:?}"
        );

        let _ = std::fs::remove_dir_all(&repo.root);
        let _ = std::fs::remove_dir_all(&home);
    }
}
