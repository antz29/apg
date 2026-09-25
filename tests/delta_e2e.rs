mod common;

use apg::cache::{CacheKey, Manifest, ScanConfigKey};
use apg::delta::*;
use std::path::{Path, PathBuf};

fn scratch(tag: &str) -> (PathBuf, git2::Repository) {
    let dir = std::env::temp_dir().join(format!("apg-delta-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("refs/heads/main");
    let repo = git2::Repository::init_opts(&dir, &opts).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "apg test").unwrap();
        cfg.set_str("user.email", "t@example.com").unwrap();
    }
    (dir, repo)
}

fn commit(repo: &git2::Repository, msg: &str) -> String {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = repo.signature().unwrap();
    let head = repo.head().ok().map(|h| h.peel_to_commit().unwrap());
    let parents: Vec<&git2::Commit> = head.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, parents.as_slice())
        .unwrap()
        .to_string()
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// e2e tier -- real I/O: every test here builds a scratch git repo under
/// the temp dir and runs real libgit2 operations. Each is `#[ignore]`d, so
/// a plain `cargo test` never runs one; the only entry point is the named
/// guard `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn delta_parses_committed_add_modify_delete_and_rename() {
        let (dir, repo) = scratch("parse");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        write(&dir, "b.go", "package b\n");
        write(&dir, "gone.go", "package g\n");
        let base = commit(&repo, "base");

        // Committed: modify a, delete gone, add c, rename b -> renamed.
        write(&dir, "a.go", "package a\n\nvar X = 1\n");
        std::fs::remove_file(dir.join("gone.go")).unwrap();
        write(&dir, "c.go", "package c\n");
        // A real rename: b.go -> renamed.go (same bytes so find_similar sees it).
        let b = std::fs::read(dir.join("b.go")).unwrap();
        std::fs::remove_file(dir.join("b.go")).unwrap();
        std::fs::write(dir.join("renamed.go"), b).unwrap();
        commit(&repo, "change");

        let d = compute(&dir, &base).unwrap();
        assert!(d.modified.contains("a.go"), "{d:?}");
        assert!(d.removed.contains("gone.go"), "{d:?}");
        assert!(d.added.contains("c.go"), "{d:?}");
        assert!(
            d.renamed
                .iter()
                .any(|(o, n)| o == "b.go" && n == "renamed.go"),
            "{d:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn delta_parses_untracked_and_working_tree_state() {
        let (dir, repo) = scratch("status");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        let base = commit(&repo, "base");

        // Uncommitted: modify a (working tree), add untracked new.go, delete
        // b.go (tracked), all at the same sha.
        write(&dir, "b.go", "package b\n");
        commit(&repo, "add b");
        let base2 = commit(&repo, "noop");
        let _ = base;
        write(&dir, "a.go", "package a\n\nvar Y = 2\n");
        write(&dir, "new.go", "package new\n");
        std::fs::remove_file(dir.join("b.go")).unwrap();

        let d = compute(&dir, &base2).unwrap();
        assert!(d.modified.contains("a.go"), "{d:?}");
        assert!(d.added.contains("new.go"), "{d:?}");
        assert!(d.removed.contains("b.go"), "{d:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn full_scan_when_recorded_commit_not_an_ancestor() {
        let (dir, repo) = scratch("nonancestor");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        let base = commit(&repo, "base");
        write(&dir, "a.go", "package a\n\nvar X = 1\n");
        let tip = commit(&repo, "tip");

        let store = dir.join("store");
        let key = CacheKey::compute(&ScanConfigKey::default());
        ScanRecord {
            sha: tip.clone(),
            cache_key: key.clone(),
            manifest: Manifest::default(),
            content_key: None,
        }
        .save(&store)
        .unwrap();
        // The recorded scan is at exactly HEAD ⇒ the warm-cache verdict: no
        // full scan and no delta — the recorded manifest IS the state.
        assert_eq!(full_scan_reason(&dir, Some(&store), &key), None);
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(!p.requires_full_scan(), "{p:?}");
        assert!(p.warm_cache, "{p:?}");
        assert!(p.delta.is_none());

        // A recorded ANCESTOR that is not HEAD takes the incremental delta
        // path (the warm verdict is exactly the recorded-HEAD condition).
        ScanRecord {
            sha: base.clone(),
            cache_key: key.clone(),
            manifest: Manifest::default(),
            content_key: None,
        }
        .save(&store)
        .unwrap();
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(!p.requires_full_scan(), "{p:?}");
        assert!(!p.warm_cache, "{p:?}");
        assert!(p.delta.is_some());

        // Restore the recorded `tip` scan before rewriting history.
        ScanRecord {
            sha: tip.clone(),
            cache_key: key.clone(),
            manifest: Manifest::default(),
            content_key: None,
        }
        .save(&store)
        .unwrap();

        // Rewrite history: reset main back to `base` (the recorded `tip`
        // commit is no longer an ancestor) ⇒ full scan.
        repo.reset(
            &repo
                .find_object(git2::Oid::from_str(&base).unwrap(), None)
                .unwrap(),
            git2::ResetType::Hard,
            None,
        )
        .unwrap();
        let reason = full_scan_reason(&dir, Some(&store), &key);
        assert!(
            matches!(reason, Some(FullScanReason::NotAncestor { .. })),
            "{reason:?}"
        );
        // The plan refuses the incremental path (correct full-scan result).
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(p.requires_full_scan());
        assert!(p.delta.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn malformed_recorded_sha_forces_a_full_scan() {
        // A recorded sha that is not a valid object id cannot be verified as an
        // ancestor of HEAD; the fallback must fire (a correct full-scan), never
        // fall through to the incremental path.
        let (dir, repo) = scratch("badsha");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        commit(&repo, "base");
        let key = CacheKey::compute(&ScanConfigKey::default());
        let store = dir.join("store");
        ScanRecord {
            sha: "not-a-sha".to_string(),
            cache_key: key.clone(),
            manifest: Manifest::default(),
            content_key: None,
        }
        .save(&store)
        .unwrap();
        let reason = full_scan_reason(&dir, Some(&store), &key);
        assert!(reason.is_some(), "a malformed recorded sha must full-scan");
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(p.requires_full_scan());
        assert!(p.delta.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn full_scan_on_cache_key_drift() {
        let (dir, repo) = scratch("drift");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        let base = commit(&repo, "base");
        let _ = repo;

        let store = dir.join("store");
        let recorded = CacheKey::compute(&ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        });
        ScanRecord {
            sha: base,
            cache_key: recorded,
            manifest: Manifest::default(),
            content_key: None,
        }
        .save(&store)
        .unwrap();

        // Same config ⇒ incremental allowed.
        let same = ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        };
        assert_eq!(
            full_scan_reason(&dir, Some(&store), &CacheKey::compute(&same)),
            None
        );

        // Config drift (an added exclude) ⇒ cache-key drift ⇒ full scan.
        let drifted = ScanConfigKey {
            languages: vec!["go".into()],
            excludes: vec!["vendor".into()],
            ..Default::default()
        };
        let reason = full_scan_reason(&dir, Some(&store), &CacheKey::compute(&drifted));
        assert!(
            matches!(reason, Some(FullScanReason::CacheKeyDrift { .. })),
            "{reason:?}"
        );
        let p = plan(&dir, Some(&store), &drifted);
        assert!(p.requires_full_scan());
        assert!(p.delta.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn no_recorded_scan_forces_full_scan() {
        let (dir, repo) = scratch("norecord");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        commit(&repo, "base");
        let key = CacheKey::compute(&ScanConfigKey::default());
        let reason = full_scan_reason(&dir, None, &key);
        assert!(matches!(reason, Some(FullScanReason::NoRecordedScan)));
        // Even with a store path but no record on disk, the same verdict holds.
        let store = dir.join("store");
        let reason = full_scan_reason(&dir, Some(&store), &key);
        assert!(matches!(reason, Some(FullScanReason::NoRecordedScan)));
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The phase-01 content-identity key on the shared scan record
    /// (feedback-101). It was listed as "pure serde ⇒ unit" but its body does
    /// real filesystem I/O (`std::env::temp_dir()` + `ScanRecord::save/load`
    /// via std::fs), so by `global.constraint.test-tier-boundaries` it is e2e
    /// (the law governs, the body wins).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir + scan.json fs); run via cargo test-e2e"]
    fn scan_record_content_key_round_trips_and_defaults_absent() {
        // The phase-01 content-identity key is part of the shared scan record
        // (feedback-101): the next scan's splice guard compares this against the
        // local DB's own recorded key. A pre-hardening `scan.json` (no field)
        // still loads — serde-defaults to `None` — which simply makes the
        // splice ineligible rather than failing to read the record.
        let dir = std::env::temp_dir().join(format!("apg-scanrecord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = CacheKey::compute(&ScanConfigKey::default());
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("scan.json"),
            format!(
                r#"{{"sha":"abc","cache_key":{},"manifest":{}}}"#,
                serde_json::to_string(&key).unwrap(),
                serde_json::to_string(&Manifest::default()).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(ScanRecord::load(&store).unwrap().content_key, None);

        ScanRecord {
            sha: "abc".into(),
            cache_key: key,
            manifest: Manifest::default(),
            content_key: Some("keyabc".into()),
        }
        .save(&store)
        .unwrap();
        assert_eq!(
            ScanRecord::load(&store).unwrap().content_key.as_deref(),
            Some("keyabc")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
