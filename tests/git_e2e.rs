mod common;

use apg::git::*;
use apg::specs;
use apg::testutil::{self, Repo, git2_repo};
use common::wt_commit;
use std::path::Path;

fn fixture_repo(tag: &str) -> Repo {
    Repo::new(tag)
}

fn open_worktree_sha(wt: &Path) -> String {
    git2::Repository::open(wt)
        .unwrap()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string()
}

/// e2e tier -- real I/O: every test here creates scratch git repos/DBs
/// under the temp dir, writes files, runs git2/libgit2 operations or spawns
/// the `apg` binary. Each is `#[ignore]`d, so a plain `cargo test` never
/// runs one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    // ------------------------------------------------------------------
    // Staleness (git2-based reads; pure-git2 fixtures — no git CLI)
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn same_sha_and_clean_is_fresh() {
        let repo = fixture_repo("fresh");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert!(
            !is_stale(&apg),
            "clean tree at the recorded sha must be fresh"
        );
        assert!(refusal_message(&apg).is_none());
        let cur = git_state(&apg);
        assert_eq!(
            staleness_line(&apg, &cur),
            format!("Git state: recorded {sha}@true vs current {sha}@true → FRESH")
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn differing_sha_is_stale() {
        let repo = fixture_repo("sha");
        let sha0 = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha0), true, "2026-09-07T00:00:00Z");
        let sha1 = repo.commit_all("second");
        assert_ne!(sha0, sha1);
        assert!(is_stale(&apg), "a new commit must make the DB stale");
        let msg = refusal_message(&apg).unwrap();
        assert!(
            msg.contains(&format!("recorded {sha0}@true, current {sha1}@true")),
            "refusal message: {msg}"
        );
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");
        assert!(staleness_line(&apg, &git_state(&apg)).contains("→ STALE"));
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn dirty_tree_at_same_sha_is_stale() {
        let repo = fixture_repo("dirty");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        // Same sha, but the tree moved: the scan did not see this content.
        repo.write(".gitignore", "apg/.trans/\n# dirty after scan\n");
        assert!(!git_state(&apg).clean);
        assert!(
            is_stale(&apg),
            "a dirty tree at the same sha must be stale (recorded clean=true)"
        );
        assert!(
            refusal_message(&apg)
                .unwrap()
                .contains(&format!("recorded {sha}@true")),
            "dirty-state message should name the recorded clean state"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn recorded_dirty_matching_dirty_tree_is_fresh() {
        // A scan over an already-dirty tree records clean=false; the same dirty
        // tree at the same sha is (by design) indistinguishable from it, so the
        // DB stays fresh until the tree changes again.
        let repo = fixture_repo("dirtyrec");
        repo.write("dirty.txt", "x");
        assert!(!git_state(&repo.root).clean);
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn no_recorded_scan_meta_is_stale() {
        let repo = fixture_repo("norec");
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        // DB exists in a git repo, but no graph.jsonl / no scan_meta record:
        // freshness cannot be verified.
        assert!(is_stale(&apg));
        let msg = refusal_message(&apg).unwrap();
        assert!(msg.contains("recorded -@-"), "{msg}");
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn non_git_repo_is_never_stale() {
        // A DB whose scan was not in a git repo, plus no git repo now → N/A.
        let dir = std::env::temp_dir().join(format!("apg-nongit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let apg = dir.join("apg").join(specs::TRANS);
        std::fs::create_dir_all(&apg).unwrap();
        let apg = dir.join("apg");
        std::fs::write(apg.join(specs::TRANS).join("db.lbug"), "").unwrap();
        testutil::write_scan_meta(&apg, None, false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        // The gate is N/A, but the fast-path is NOT fresh: is_stale != !is_fresh.
        assert!(!is_fresh(&apg), "not a git repo → nothing to reuse");
        assert!(refusal_message(&apg).is_none());
        assert_eq!(
            staleness_line(&apg, &git_state(&apg)),
            "Git state: N/A (not a git repo)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn no_db_is_never_stale() {
        let repo = fixture_repo("nodb");
        let apg = repo.apg_root();
        // Even a recorded mismatch is irrelevant when there is no DB to guard.
        testutil::write_scan_meta(&apg, Some("stale-sha"), false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        // No DB → the fast-path is NOT fresh (nothing to reuse): is_stale !=
        // !is_fresh for this N/A gate case.
        assert!(!is_fresh(&apg));
        assert_eq!(
            staleness_line(&apg, &git_state(&apg)),
            format!("Git state: no scan yet (current {}@true)", repo.head_sha())
        );
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // Phase-01 task-9: content-identity freshness (win A)
    // ------------------------------------------------------------------

    /// The DB-existence precondition: with no `db.lbug`, or a `graph.jsonl`
    /// without its DB, the fast-path is NOT fresh (there is nothing to reuse) —
    /// even when the recorded scan_meta matches the tree.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn is_fresh_requires_the_live_db() {
        let repo = fixture_repo("freshdb");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert!(!is_fresh(&apg), "no db.lbug → not fresh");
        assert!(
            !is_stale(&apg),
            "the gate is N/A without a DB (is_stale != !is_fresh)"
        );
        // The DB appearing makes the matching recorded state fresh.
        testutil::touch_db(&apg);
        assert!(is_fresh(&apg), "matching recorded state + DB → fresh");
        assert!(!is_stale(&apg));
        // A DB but no recorded scan_meta (graph.jsonl removed) → not fresh.
        std::fs::remove_file(apg.join(specs::TRANS).join("graph.jsonl")).unwrap();
        assert!(!is_fresh(&apg), "no recorded scan → not fresh");
        assert!(is_stale(&apg), "DB in a repo with no recorded scan → stale");
        testutil::remove(&repo);
    }

    /// A pre-hardening scan_meta (no content-identity key) cannot be verified:
    /// it is NOT fresh and the gate treats it as stale.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn pre_hardening_scan_meta_is_not_fresh() {
        let repo = fixture_repo("prehard");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta_keyed(&apg, Some(&sha), true, "2026-09-07T00:00:00Z", None);
        assert!(
            !is_fresh(&apg),
            "missing key → freshness cannot be verified"
        );
        assert!(is_stale(&apg));
        testutil::remove(&repo);
    }

    /// mtime is never consulted: touching a tracked file forward without
    /// changing a byte keeps the DB fresh.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn touch_without_byte_change_stays_fresh() {
        let repo = fixture_repo("touch");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert!(is_fresh(&apg));
        // Bump the mtime far into the future — bytes unchanged.
        let p = repo.root.join("apg/config.json");
        let before = std::fs::read(&p).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        drop(f);
        assert_eq!(
            std::fs::read(&p).unwrap(),
            before,
            "bytes must be unchanged"
        );
        assert!(
            is_fresh(&apg),
            "a touch without a byte change must stay fresh"
        );
        assert!(!is_stale(&apg));
        testutil::remove(&repo);
    }

    /// A byte edit at the same sha changes the content identity: the DB is not
    /// fresh and the gate is stale.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn content_identity_byte_edit_invalidates() {
        let repo = fixture_repo("byteedit");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert!(is_fresh(&apg));
        repo.write(
            "apg/config.json",
            "{\n  \"default\": \"edited\",\n  \"types\": []\n}\n",
        );
        assert!(!is_fresh(&apg), "a byte edit must invalidate freshness");
        assert!(is_stale(&apg));
        assert!(staleness_line(&apg, &git_state(&apg)).contains("→ STALE"));
        testutil::remove(&repo);
    }

    /// A recorded DIRTY tree matched by content stays fresh; a content change
    /// of that same dirty tree at the same sha invalidates it (recorded
    /// `clean=false` alone is not enough — the digest must match too).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn recorded_dirty_content_change_invalidates() {
        let repo = fixture_repo("dirtykey");
        repo.write("scratch.txt", "one");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        testutil::write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
        assert!(
            is_fresh(&apg),
            "same dirty content at the same sha is fresh"
        );
        // Same sha, still dirty — but the dirty content changed.
        repo.write("scratch.txt", "two");
        assert!(!is_fresh(&apg), "changed dirty content must invalidate");
        assert!(is_stale(&apg));
        testutil::remove(&repo);
    }

    /// `staleness_line` prints the SAME verdict `is_fresh` returns — in both
    /// directions. A recorded-dirty tree whose content digest changed at the
    /// same sha prints STALE from BOTH (never FRESH followed by a full run).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn staleness_line_agrees_with_is_fresh() {
        let repo = fixture_repo("verdict");
        let sha = repo.head_sha();
        let apg = repo.apg_root();
        testutil::touch_db(&apg);
        // FRESH: a matching clean tree.
        testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert_eq!(
            is_fresh(&apg),
            staleness_line(&apg, &git_state(&apg)).contains("→ FRESH")
        );
        // Recorded DIRTY tree, then its dirty content changes at the same sha.
        repo.write("scratch.txt", "one");
        testutil::write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
        assert!(is_fresh(&apg));
        repo.write("scratch.txt", "two");
        let line = staleness_line(&apg, &git_state(&apg));
        assert!(!is_fresh(&apg), "content digest changed at the same sha");
        assert!(line.contains("→ STALE"), "printed line: {line}");
        assert!(!line.contains("→ FRESH"), "printed line: {line}");
        assert_eq!(is_fresh(&apg), line.contains("→ FRESH"));
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // Identity (R7): worktree vs main, default branch, the layout invariant
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn identity_in_main_checkout() {
        let repo = fixture_repo("ident-main");
        let apg = repo.apg_root();
        let id = repo_identity(&apg).unwrap();
        assert_eq!(id.main_root, id.checkout_root);
        assert!(!id.is_worktree);
        assert_eq!(id.branch.as_deref(), Some("main"));
        assert_eq!(id.default_branch.as_deref(), Some("main"));
        assert_eq!(id.head_sha.as_deref(), Some(repo.head_sha().as_str()));
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn identity_in_project_worktree() {
        let repo = fixture_repo("ident-wt");
        let wt = repo.start_project("foo");
        let apg = wt.join(specs::LAYOUT);
        let id = repo_identity(&apg).unwrap();
        assert_eq!(
            id.checkout_root,
            repo.project_worktree_dir("foo").canonicalize().unwrap()
        );
        assert_eq!(id.main_root, repo.root.canonicalize().unwrap());
        assert!(id.is_worktree);
        assert_eq!(id.branch.as_deref(), Some("foo"));
        // The DEFAULT branch is the repo's default (origin/HEAD when present,
        // else the main checkout's symbolic HEAD) — never the worktree's
        // branch. The fixture has no origin, so the fallback applies: main.
        assert_eq!(id.default_branch.as_deref(), Some("main"));
        assert!(id.head_sha.is_some());
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn identity_divergence_errors_when_layout_outside_checkout() {
        // An apg/ layout that git does not contain: the invariant errors.
        let dir = std::env::temp_dir().join(format!("apg-divergence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = fixture_repo("divergence");
        let outside = dir.join("apg");
        std::fs::create_dir_all(outside.join(specs::TRANS)).unwrap();
        let err = repo_identity(&outside).unwrap_err();
        assert!(
            format!("{err:#}").contains("divergence"),
            "divergence message: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // Membership (R3/R4)
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn membership_passes_in_project_worktree_on_project_branch() {
        let repo = fixture_repo("member-ok");
        repo.start_project("foo");
        let apg = repo.project_apg_root("foo");
        require_membership(&apg, "foo").unwrap();
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn membership_refuses_on_default_branch_naming_branch_half() {
        let repo = fixture_repo("member-main");
        let apg = repo.apg_root();
        let err = require_membership(&apg, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("half 1 (branch)"), "{msg}");
        assert!(msg.contains("branch `main`"), "{msg}");
        assert!(msg.contains("default branch"), "{msg}");
        assert!(msg.contains("apg project start foo"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn membership_refuses_wrong_project_from_another_worktree() {
        let repo = fixture_repo("member-wrong");
        repo.start_project("foo");
        repo.start_project("bar");
        let apg_bar = repo.project_apg_root("bar");
        let err = require_membership(&apg_bar, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("half 1 (branch)"), "{msg}");
        assert!(msg.contains("branch `bar`"), "{msg}");
        assert!(msg.contains("branch `foo`"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn bootstrap_branch_in_main_checkout_is_a_project_context() {
        // R20 bootstrap carve-out: a project branch that IS the main
        // checkout's HEAD (branch-without-worktree — git cannot host the
        // branch anywhere else) is a valid mutation context. The default
        // branch is not — and the default is resolved from origin/HEAD, so it
        // stays `main` even while the main checkout holds the project branch.
        let repo = fixture_repo("member-bootstrap");
        let main_repo = git2_repo(&repo);
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        // origin/HEAD → main (like a real remote-backed repo).
        main_repo
            .reference("refs/remotes/origin/main", head.id(), true, "origin main")
            .unwrap();
        main_repo
            .reference_symbolic(
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
                true,
                "origin head",
            )
            .unwrap();
        // The main checkout switches to the project branch (bootstrap state).
        main_repo.branch("apg-feature", &head, false).unwrap();
        main_repo.set_head("refs/heads/apg-feature").unwrap();
        assert_eq!(
            repo_identity(&repo.apg_root())
                .unwrap()
                .default_branch
                .as_deref(),
            Some("main")
        );
        // The bootstrap project context passes membership...
        require_membership(&repo.apg_root(), "apg-feature").unwrap();
        // ...and the default branch itself never does, even under its own name.
        main_repo.set_head("refs/heads/main").unwrap();
        let err = require_membership(&repo.apg_root(), "main").unwrap_err();
        assert!(
            format!("{err:#}").contains("default branch is never a mutation place"),
            "{err:#}"
        );
        // Back on main, the project branch refuses via the branch half.
        let err = require_membership(&repo.apg_root(), "apg-feature").unwrap_err();
        assert!(format!("{err:#}").contains("half 1 (branch)"), "{err:#}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn universal_context_refuses_outside_any_project() {
        let repo = fixture_repo("member-universal");
        // Main checkout on main: no project context.
        let err = require_project_context(&repo.apg_root()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("refused"), "{msg}");
        assert!(msg.contains("default branch"), "{msg}");
        // Inside a project worktree: universal mutations are fine.
        repo.start_project("foo");
        require_project_context(&repo.project_apg_root("foo")).unwrap();
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn non_git_dir_refuses_membership_with_git_guidance() {
        let dir = std::env::temp_dir().join(format!("apg-nongit-m-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        let err = require_membership(&dir.join("apg"), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not inside a git repository"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // Lifecycle cleanup helpers (merge self-cleanup / delete): the shared
    // git2 worktree-removal + branch-deletion primitives.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn remove_worktree_unregisters_and_removes_working_dir() {
        let repo = fixture_repo("rm-wt");
        let wt = repo.start_project("foo");
        assert!(wt.is_dir());
        let main_repo = git2_repo(&repo);
        assert!(main_repo.find_worktree("foo").is_ok());

        remove_worktree(&repo.root, "foo").unwrap();

        assert!(!wt.exists(), "the worktree working dir must be removed");
        assert!(
            main_repo.find_worktree("foo").is_err(),
            "the worktree registration must be pruned"
        );
        assert!(
            main_repo
                .find_branch("foo", git2::BranchType::Local)
                .is_ok(),
            "remove_worktree must leave the branch alone"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn remove_worktree_tolerates_missing_or_unregistered_worktrees() {
        let repo = fixture_repo("rm-wt-gone");
        repo.start_project("foo");
        let main_repo = git2_repo(&repo);
        // Simulate an already-pruned worktree: registration gone, leftover dir
        // (or no dir at all) — both must be safe no-ops. (`valid` is required
        // to prune a still-valid worktree; `working_tree` off keeps the dir.)
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(true).working_tree(false);
        main_repo
            .find_worktree("foo")
            .unwrap()
            .prune(Some(&mut opts))
            .unwrap();
        assert!(main_repo.find_worktree("foo").is_err());
        remove_worktree(&repo.root, "foo").unwrap();

        // A name that never existed is also a no-op.
        remove_worktree(&repo.root, "never-existed").unwrap();
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn delete_branch_removes_local_project_branch() {
        let repo = fixture_repo("del-br");
        repo.start_project("foo");
        let main_repo = git2_repo(&repo);
        assert!(
            main_repo
                .find_branch("foo", git2::BranchType::Local)
                .is_ok()
        );

        // The lifecycle ordering: the worktree is removed FIRST — a branch
        // that is still a linked worktree's HEAD cannot be deleted (libgit2
        // mirrors `git branch -d`) — then the branch.
        remove_worktree(&repo.root, "foo").unwrap();
        delete_branch(&repo.root, "foo").unwrap();

        assert!(
            main_repo
                .find_branch("foo", git2::BranchType::Local)
                .is_err(),
            "the local project branch must be deleted"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn delete_branch_never_touches_the_default_branch() {
        // The never-touch-default-branch law: deleting <default> is a hard
        // refusal, and the main checkout (default branch checked out) must
        // survive untouched.
        let repo = fixture_repo("del-default");
        let err = delete_branch(&repo.root, "main").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("refused"), "{msg}");
        assert!(msg.contains("default branch"), "{msg}");
        assert!(
            git2_repo(&repo)
                .find_branch("main", git2::BranchType::Local)
                .is_ok(),
            "the default branch must survive"
        );
        assert!(repo.root.is_dir(), "the main checkout must survive");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn delete_branch_tolerates_an_already_deleted_branch() {
        let repo = fixture_repo("del-br-gone");
        repo.start_project("foo");
        let main_repo = git2_repo(&repo);
        // Lifecycle ordering again: remove the worktree first so the branch is
        // deletable, then delete it out from under the helper.
        remove_worktree(&repo.root, "foo").unwrap();
        let mut branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        branch.delete().unwrap();
        assert!(
            main_repo
                .find_branch("foo", git2::BranchType::Local)
                .is_err()
        );
        delete_branch(&repo.root, "foo").unwrap();
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // Auto-commit (R8)
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn auto_commit_makes_one_commit_with_single_file_diff() {
        let repo = fixture_repo("autocommit");
        repo.start_project("foo");
        let wt = repo.project_worktree_dir("foo");
        // A mutation-style file change inside the worktree (spec JSONL).
        let rel = "apg/specs/foo.jsonl";
        let path = wt.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"type\":\"spec\"}\n").unwrap();
        let sha0 = open_worktree_sha(&wt);
        let Some(new_sha) = auto_commit(&wt.join("apg"), &path).unwrap() else {
            panic!("expected a commit");
        };
        assert_ne!(sha0, new_sha);
        // Exactly one commit ahead of the main checkout's branch tip...
        let main_repo = git2_repo(&repo);
        let branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        let branch_commit = branch.get().peel_to_commit().unwrap();
        assert_eq!(branch_commit.id().to_string(), new_sha);
        // ...whose tree diff is exactly the one file (single-file diff).
        let parent = branch_commit.parent(0).unwrap();
        let diff = main_repo
            .diff_tree_to_tree(
                Some(&parent.tree().unwrap()),
                Some(&branch_commit.tree().unwrap()),
                None,
            )
            .unwrap();
        assert_eq!(
            diff.deltas().len(),
            1,
            "auto-commit must be a single-file diff"
        );
        let delta = diff.deltas().next().unwrap();
        assert_eq!(delta.new_file().path().unwrap(), Path::new(rel));
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn auto_commit_is_a_noop_when_content_matches_head() {
        let repo = fixture_repo("autocommit-noop");
        repo.start_project("foo");
        let wt = repo.project_worktree_dir("foo");
        let path = wt.join("apg/specs/foo.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x\n").unwrap();
        // Commit the file on the worktree's branch (git2 — the same mechanics
        // auto_commit uses), so HEAD already carries this content.
        wt_commit(&wt, &["apg/specs/foo.jsonl"], "seed");
        assert_eq!(auto_commit(&wt.join("apg"), &path).unwrap(), None);
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn commit_file_commits_with_custom_message() {
        let repo = fixture_repo("commitfile");
        repo.start_project("foo");
        let wt = repo.project_worktree_dir("foo");
        // The start-flow self-heal uses commit_file for its .gitignore
        // scaffold commit — same mechanics, caller-supplied message.
        let path = wt.join(".gitignore");
        std::fs::write(&path, "apg/.trans/\napg/.worktrees/\n# extra\n").unwrap();
        let sha0 = open_worktree_sha(&wt);
        let Some(new_sha) =
            commit_file(&wt.join("apg"), &path, "apg: scaffold .gitignore entries").unwrap()
        else {
            panic!("expected a commit");
        };
        assert_ne!(sha0, new_sha);
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            head.message().unwrap(),
            "apg: scaffold .gitignore entries",
            "commit_file must use the caller's message"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn commit_files_commits_all_paths_in_one_commit() {
        let repo = fixture_repo("commitfiles");
        repo.start_project("foo");
        let wt = repo.project_worktree_dir("foo");
        let a = wt.join("apg/layers/requirements/requirement/a.json");
        let b = wt.join("apg/layers/domain/entity/b.json");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::write(&a, "a\n").unwrap();
        std::fs::write(&b, "b\n").unwrap();
        let sha0 = open_worktree_sha(&wt);
        let Some(new_sha) = commit_files(
            &wt.join("apg"),
            &[a.as_path(), b.as_path()],
            &[],
            "apg: graph mutation (apg/layers/...)",
        )
        .unwrap() else {
            panic!("expected a commit");
        };
        assert_ne!(sha0, new_sha);
        // Exactly one commit ahead of the branch tip, whose tree diff is
        // exactly the two files — one commit carrying all affected paths.
        let main_repo = git2_repo(&repo);
        let branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        let branch_commit = branch.get().peel_to_commit().unwrap();
        assert_eq!(branch_commit.id().to_string(), new_sha);
        let parent = branch_commit.parent(0).unwrap();
        let diff = main_repo
            .diff_tree_to_tree(
                Some(&parent.tree().unwrap()),
                Some(&branch_commit.tree().unwrap()),
                None,
            )
            .unwrap();
        assert_eq!(
            diff.deltas().len(),
            2,
            "commit_files must commit all paths in one commit"
        );
        testutil::remove(&repo);
    }

    /// `commit_files` stages a removed path as a deletion (`remove_path`), not
    /// an `add_path` — the latter stats the gone file and fails with a libgit2
    /// NotFound, which is why a `node rm` used to roll back inside a real repo.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn commit_files_stages_a_deletion() {
        let repo = fixture_repo("commitfiles-delete");
        repo.start_project("foo");
        let wt = repo.project_worktree_dir("foo");
        let rel = "apg/layers/requirements/requirement/gone.json";
        let path = wt.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "gone\n").unwrap();
        let sha0 = wt_commit(&wt, &[rel], "seed");
        // Remove it from disk and commit the deletion.
        std::fs::remove_file(&path).unwrap();
        let Some(new_sha) =
            commit_files(&wt.join("apg"), &[], &[path.as_path()], "apg: rm").unwrap()
        else {
            panic!("expected a deletion commit");
        };
        assert_ne!(sha0, new_sha);
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        let parent = head.parent(0).unwrap();
        let diff = wt_repo
            .diff_tree_to_tree(
                Some(&parent.tree().unwrap()),
                Some(&head.tree().unwrap()),
                None,
            )
            .unwrap();
        let deleted: Vec<&str> = diff
            .deltas()
            .filter(|d| d.status() == git2::Delta::Deleted)
            .map(|d| d.old_file().path().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            deleted,
            vec![rel],
            "the removed path must be staged as a deletion"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn reanchor_rewrites_graph_jsonl_line_one() {
        let repo = fixture_repo("reanchor");
        repo.start_project("foo");
        let apg = repo.project_apg_root("foo");
        let sha0 = repo.head_sha();
        testutil::write_scan_meta(&apg, Some(&sha0), true, "2026-09-07T00:00:00Z");
        let new_sha = "abcd1234";
        reanchor_scan_meta(
            &apg,
            &GitState {
                sha: Some(new_sha.to_string()),
                clean: false,
                content_key: Some("reanchored-key".to_string()),
            },
        )
        .unwrap();
        let first = std::fs::read_to_string(apg.join(specs::TRANS).join("graph.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(first.contains(new_sha), "line 1: {first}");
        assert!(first.contains("\"git_clean\":false"), "line 1: {first}");
        assert!(
            first.contains("\"scanned_at\":\"2026-09-07T00:00:00Z\""),
            "line 1: {first}"
        );
        // The content-identity key is preserved across the re-anchor.
        assert!(
            first.contains("\"content_key\":\"reanchored-key\""),
            "line 1: {first}"
        );
        // The recorded state now matches the new state → fresh.
        assert_eq!(
            recorded_scan(&apg),
            Some(RecordedScan {
                sha: new_sha.to_string(),
                clean: false,
                content_key: Some("reanchored-key".to_string()),
            })
        );
        testutil::remove(&repo);
    }

    /// Phase-04 task-5 (acceptance): one logical node/edge mutation produces
    /// exactly ONE commit, and that commit stages durable files only — never
    /// `apg/.trans` (the gitignored transient store).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn acceptance_one_logical_mutation_is_exactly_one_durable_commit() {
        let (repo, wt, _wt_apg) = testutil::project_with_db("accept-one-commit");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let run = |args: &[&str]| {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // The paths the HEAD commit's tree diff touched.
        let head_diff_paths = || -> Vec<String> {
            let r = git2::Repository::open(&wt).unwrap();
            let head = r.head().unwrap().peel_to_commit().unwrap();
            let parent = head.parent(0).unwrap();
            let diff = r
                .diff_tree_to_tree(
                    Some(&parent.tree().unwrap()),
                    Some(&head.tree().unwrap()),
                    None,
                )
                .unwrap();
            diff.deltas()
                .map(|d| d.new_file().path().unwrap().to_string_lossy().into_owned())
                .collect()
        };

        let base = testutil::commit_count(&wt);

        // Logical mutation 1 + 2: each `apg node add` is exactly one commit.
        run(&["node", "add", "requirements", "requirement", "one-a"]);
        assert_eq!(
            testutil::commit_count(&wt),
            base + 1,
            "node add one-a must be exactly one commit"
        );
        run(&["node", "add", "requirements", "requirement", "one-b"]);
        assert_eq!(
            testutil::commit_count(&wt),
            base + 2,
            "node add one-b must be exactly one commit"
        );

        // Logical mutation 3: the edge add rewrites BOTH endpoint files in ONE
        // commit whose diff is the two durable layer files.
        run(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.one-a",
            "requirements.requirement.one-b",
        ]);
        assert_eq!(
            testutil::commit_count(&wt),
            base + 3,
            "the edge add must be exactly one commit"
        );
        let edge_paths = head_diff_paths();
        assert_eq!(
            edge_paths.len(),
            2,
            "the edge commit stages both endpoint files: {edge_paths:?}"
        );
        for p in &edge_paths {
            assert!(p.starts_with("apg/layers/"), "durable only: {p}");
            assert!(!p.starts_with("apg/.trans/"), "never .trans: {p}");
        }

        // Logical mutation 4: `node rm` rewrites the referring file and deletes
        // the node in ONE commit, still durable-only.
        run(&["node", "rm", "requirements", "requirement", "one-b"]);
        assert_eq!(
            testutil::commit_count(&wt),
            base + 4,
            "node rm must be exactly one commit"
        );
        for p in head_diff_paths() {
            assert!(p.starts_with("apg/layers/"), "durable only: {p}");
            assert!(!p.starts_with("apg/.trans/"), "never .trans: {p}");
        }

        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // R6 VI: the git CLI is never shelled out to anywhere in the apg binary
    // (git2, default-features = false; push/tag remain human acts).
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn no_git_cli_shellouts_remain_in_src() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = root.join("src");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&src).unwrap().flatten() {
            let path = entry.path();
            // Only the apg binary's own flat top-level src/*.rs files
            // (read_dir yields direct children, so the vendored frontend
            // projects under src/*lib/ are never included).
            if !path.is_file() || path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(
                !content.contains("Command::new(\"git\")"),
                "{} shells out to the git CLI (R6: git2 only; push/tag remain human acts)",
                path.display()
            );
            checked += 1;
        }
        assert!(
            checked >= 10,
            "expected the flat src/*.rs set, checked {checked}"
        );
    }

    // ------------------------------------------------------------------
    // Structural claim boundary: the gitignore predicate is the ingestor's
    // share of the walk's claim boundary (apg-0.17.0 phase-01 task-21).
    // ------------------------------------------------------------------

    /// A TRACKED path is never reported ignored (Git's ignore rules apply to
    /// untracked content only; the walk emits every tracked file, so a
    /// tracked file matching an ignore glob must stay in the graph), while
    /// an untracked ignored path still is — in both repo-relative and
    /// absolute spellings. The checkout-identity use (an untracked worktree
    /// probe) is unaffected.
    #[test]
    #[ignore = "e2e tier: real git I/O (scratch repo); run via cargo test-e2e"]
    fn path_is_ignored_declines_tracked_and_reports_untracked() {
        let repo = fixture_repo("gitignore-boundary");
        // Commit the file FIRST, then ignore its name: it is tracked, so
        // the ignore rule never covers it.
        repo.write("secrets.env", "TRACKED=1\n");
        repo.commit_all("track secrets.env");
        repo.write(
            ".gitignore",
            "apg/.trans/\napg/.worktrees/\nsecrets.env\nbuild-out/\n",
        );
        repo.commit_all("ignore secrets.env and build-out");

        assert!(
            !path_is_ignored(&repo.root, Path::new("secrets.env")),
            "a tracked path must never read as ignored"
        );
        assert!(
            !path_is_ignored(&repo.root, &repo.root.join("secrets.env")),
            "the absolute spelling of a tracked path must never read as ignored"
        );
        assert!(
            !path_is_ignored(&repo.root, Path::new(".gitignore")),
            "an ordinary tracked path must not read as ignored"
        );

        // An untracked path matching an ignore rule is ignored.
        repo.write("build-out/leak.txt", "leak\n");
        assert!(
            path_is_ignored(&repo.root, Path::new("build-out/leak.txt")),
            "an untracked ignored path must read as ignored"
        );

        testutil::remove(&repo);
    }
}
