//! e2e tier -- real I/O: both tests spawn the candidate `apg` binary, create
//! scratch repos and inspect `apg/layers/**` and `db.lbug` on disk. Each is
//! `#[ignore]`d, so a plain `cargo test` never runs one; the only entry point is
//! the named guard `cargo test-e2e` (= `cargo test e2e:: -- --ignored`).
//! apg.testutil's own non-`#[test]` items (Repo, ApgCommand, spawn_apg,
//! scan_checkout, payload helpers) are the E2E HARNESS and live at module
//! level, never inside a tier. PIN: testutil's I/O harness (the git/process/db
//! fixtures) is e2e-only — it must not be used by a unit/int test; the pure
//! graph-fixture builder `located` performs no I/O and may be shared by any
//! tier.

mod common;

use apg::layers::{self, Layer};
use apg::specs;
use apg::testutil::*;
use std::path::Path;

/// Spawns one durable-write invocation and SIGKILLs it while it waits for the
/// blocking worktree lock this process holds. The lock keeps the writer out of
/// its critical section, so this is the pre-commit half of a per-invocation
/// write, deterministically: the invocation opened and queued on the lock, and
/// the kill lands before its node-file write. `while_blocked` runs while the
/// writer is still queued (an independent observation of the committed state);
/// a write that completes while the lock is held is a real failure — a
/// per-invocation write must take the blocking worktree lock.
fn kill_write_blocked_on_the_worktree_lock(
    wt: &Path,
    wt_apg: &Path,
    home: &Path,
    args: &[&str],
    while_blocked: impl FnOnce(),
) {
    let held = apg::artifacts::acquire_spec_lock(wt_apg).unwrap();
    let mut child = ApgCommand::new(args)
        .cwd(wt)
        .env("HOME", home.to_str().unwrap())
        .spawn();
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => panic!(
                "the write completed while the worktree lock was held ({status}) — a \
                 per-invocation write must take the blocking worktree lock"
            ),
            None if started.elapsed() >= std::time::Duration::from_millis(500) => break,
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    while_blocked();
    let pid = child.id() as i32;
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let out = child.wait_with_output().unwrap();
    assert!(
        !out.status.success(),
        "the writer was killed, not completed"
    );
    drop(held);
}

mod e2e {
    use super::*;

    /// The per-invocation durability contract at the testutil level.
    ///
    /// Every database access opens the worktree `db.lbug` read-only in its own
    /// invocation; a durable write upgrades the SAME invocation under the
    /// blocking worktree lock and performs node-file write -> exactly one
    /// commit -> database projection in one critical section. There is no
    /// daemon, socket, save step or pending set, so a SIGKILL can only ever lose
    /// an uncommitted attempt:
    ///
    /// (1) A COMPLETED write loses nothing. The invocation that returns has its
    ///     node file on disk, exactly one commit and the projection; a later
    ///     read-only invocation serves it and lands no commit of its own.
    /// (2) A WRITE KILLED MID-INVOCATION loses nothing committed. While this
    ///     test holds the blocking worktree lock, an independent read-only
    ///     invocation already serves only the committed state (an uncommitted
    ///     write is never observable), and the killed writer leaves no node
    ///     file, no commit and no phantom projection; the next read opens
    ///     exactly the last committed state.
    /// (3) The killed attempt leaves `apg/layers/**`, commits and HEAD at their
    ///     last committed state and the store pairing-consistent, and a direct
    ///     read-write open succeeds — no process holds `db.lbug`. The next
    ///     durable write then proceeds normally, landing exactly one commit: the
    ///     atomic-write guarantee replaces the daemon's stale-socket reclaim,
    ///     which has no per-invocation counterpart.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn killed_write_loses_nothing_committed_and_leaves_no_phantom_projection() {
        let (repo, wt, wt_apg) = project_with_db("session-crash");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let head_sha = |dir: &std::path::Path| {
            git2::Repository::open(dir)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        };

        // --- (1) COMPLETED WRITE: durable on return. -------------------------
        let before_commits = commit_count(&wt);
        let saved_add = ApgCommand::new(&["node", "add", "global", "constraint", "saved"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            saved_add.status.success(),
            "{}",
            String::from_utf8_lossy(&saved_add.stderr)
        );
        // The one invocation wrote its node file, landed its one commit and
        // projected — there is no save step to defer any of it.
        let saved_file = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "saved");
        assert!(
            saved_file.exists(),
            "the write invocation must write its node file"
        );
        let saved_commits = commit_count(&wt);
        let saved_head = head_sha(&wt);
        assert_eq!(
            saved_commits,
            before_commits + 1,
            "the write invocation must land exactly one commit"
        );
        let saved_bytes = std::fs::read(&saved_file).unwrap();

        // A SEPARATE read-only invocation sees the projection as soon as the
        // writer exits, and commits/HEAD/node file stay put: a read has no
        // durability point of its own and leaves no state behind.
        let q_saved = spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.saved'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q_saved.status.success(),
            "{}",
            String::from_utf8_lossy(&q_saved.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q_saved.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the completed write must be queryable as soon as its invocation returns"
        );
        assert_eq!(
            commit_count(&wt),
            saved_commits,
            "a read-only invocation must make no commit"
        );
        assert_eq!(
            head_sha(&wt),
            saved_head,
            "a read-only invocation must not move HEAD"
        );
        assert_eq!(
            std::fs::read(&saved_file).unwrap(),
            saved_bytes,
            "a read-only invocation must leave the node file byte-identical"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "a per-invocation write must leave no session socket"
        );

        // --- (2) WRITE KILLED MID-INVOCATION: loses nothing committed. -------
        // Baseline for the durable-state assertions: commits, HEAD, and the
        // whole on-disk node-file store (exactly the COMPLETED run above).
        let before_commits = commit_count(&wt);
        let before_head = head_sha(&wt);
        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();

        // Hold the blocking worktree lock and let the writer queue on it: while
        // it waits, an independent read-only invocation already serves the last
        // committed state — an uncommitted write is never observable. The
        // SIGKILL then lands inside its invocation, before its critical
        // section, so there is no buffer whose phantom projection could survive
        // it.
        kill_write_blocked_on_the_worktree_lock(
            &wt,
            &wt_apg,
            &home,
            &["node", "add", "global", "constraint", "survivor"],
            || {
                let during = spawn_apg(
                    &[
                        "query",
                        "MATCH (n:Constraint {fqn: 'global.constraint.survivor'}) RETURN count(n)",
                    ],
                    &wt,
                );
                assert!(
                    during.status.success(),
                    "{}",
                    String::from_utf8_lossy(&during.stderr)
                );
                assert_eq!(
                    String::from_utf8_lossy(&during.stdout)
                        .lines()
                        .last()
                        .map(str::trim),
                    Some("0"),
                    "a read while the writer waits must see only the committed state"
                );
            },
        );

        // Nothing partial: no node file, no commit, HEAD put, store untouched
        // and still pairing cleanly.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "survivor").exists(),
            "a killed pre-commit write must not write a node file"
        );
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the killed write must leave apg/layers/** at its last committed state"
        );
        assert_eq!(
            commit_count(&wt),
            before_commits,
            "a killed write must not create a commit"
        );
        assert_eq!(
            head_sha(&wt),
            before_head,
            "a killed write must not move HEAD"
        );

        // The next read opens exactly the last committed state, never a phantom.
        let q = spawn_apg(
            &["query", "MATCH (n:Constraint) RETURN n.fqn ORDER BY n.fqn"],
            &wt,
        );
        assert!(
            q.status.success(),
            "the next read failed: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        let stdout = String::from_utf8_lossy(&q.stdout);
        let rows: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            rows,
            vec!["n.fqn", "global.constraint.saved"],
            "the next read must open the last committed state"
        );
        assert!(
            !stdout.contains("survivor"),
            "the next read must never serve a phantom projection"
        );

        // No process holds `db.lbug`: a direct read-write open succeeds now (the
        // SIGKILL released the OS lock) and the index carries only the
        // committed node.
        {
            let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(
                db.has_node("global.constraint.saved"),
                "the index must carry the last committed node"
            );
            assert!(
                !db.has_node("global.constraint.survivor"),
                "the index must not carry the killed write's phantom projection"
            );
        }

        // --- (3) ATOMIC WRITE AFTER THE CRASH: one commit, one projection. ---
        let after = ApgCommand::new(&["node", "add", "global", "constraint", "after-crash"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            after.status.success(),
            "{}",
            String::from_utf8_lossy(&after.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "after-crash").exists(),
            "the next write must write its node file"
        );
        assert_eq!(
            commit_count(&wt),
            before_commits + 1,
            "the next write must land exactly one commit"
        );
        assert_ne!(
            head_sha(&wt),
            before_head,
            "the next write must create a commit"
        );

        let q_after = spawn_apg(
            &["query", "MATCH (n:Constraint) RETURN n.fqn ORDER BY n.fqn"],
            &wt,
        );
        assert!(
            q_after.status.success(),
            "{}",
            String::from_utf8_lossy(&q_after.stderr)
        );
        let stdout = String::from_utf8_lossy(&q_after.stdout);
        let rows: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            rows,
            vec![
                "n.fqn",
                "global.constraint.after-crash",
                "global.constraint.saved"
            ],
            "the next write must project onto the committed state"
        );
        assert!(
            !stdout.contains("survivor"),
            "the killed write must never resurface"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "no invocation in this scenario may leave a session socket"
        );

        remove(&repo);
    }

    /// Every write in the burst is its OWN per-invocation critical section —
    /// node-file write -> exactly one commit -> projection — so the durable
    /// store is never a half-written intermediate between mutations, and a
    /// writer killed mid-invocation loses nothing committed:
    ///
    /// (a) two node adds and five edge adds each land exactly ONE commit, with
    ///     their node files written: there is no buffer, so the daemon's "no
    ///     partial file and no commit BEFORE save" case cannot exist;
    /// (b) every committed node file parses with its identity, the store still
    ///     pairs, and BOTH halves of the routed edge are present — the burst
    ///     deliberately writes a node AND an edge (both endpoint files), so a
    ///     crash between the two halves of the edge would be caught here;
    /// (c) a writer killed while blocked on the worktree lock leaves no partial
    ///     file, no commit and no HEAD move — the store is exactly its last
    ///     committed state;
    /// (d) no process holds `db.lbug` — a direct read-write open succeeds — and
    ///     the index carries the committed nodes, never the killed write's
    ///     phantom projection;
    /// (e) the next read opens the last committed state, and the next write
    ///     then lands exactly one commit: the atomic-write guarantee replaces
    ///     the daemon's stale-socket reclaim, which has no per-invocation
    ///     counterpart.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn acceptance_crash_durability_no_partial_files_no_db_holder_and_last_committed_state() {
        let (repo, wt, wt_apg) = project_with_db("accept-crash");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let head_sha = |dir: &std::path::Path| {
            git2::Repository::open(dir)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        };

        let run = |args: &[&str]| -> std::process::Output {
            ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output()
        };

        // --- (a) One atomic invocation per mutation: exactly one commit each.
        // Two node adds and the edge between them, plus the minimal
        // whole-model-valid spine (SPEC §8.1/§3.4): a Stakeholder `requires` a
        // Requirement that a leaf Group's Service is `driven-by`, and that
        // Service is `realises`'d by a Component that `is-role`s a Role.
        let mut expected_commits = commit_count(&wt);
        for node in [
            &["node", "add", "requirements", "stakeholder", "crash-a"][..],
            &["node", "add", "requirements", "requirement", "crash-b"][..],
            &["node", "add", "domain", "group", "g"][..],
            &["node", "add", "domain", "service", "svc"][..],
            &["node", "add", "solution", "component", "cmp"][..],
            &[
                "node",
                "add",
                "solution",
                "role",
                "r",
                "--property",
                "applies-to=component",
            ][..],
        ] {
            let out = run(node);
            assert!(
                out.status.success(),
                "{node:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            expected_commits += 1;
            assert_eq!(
                commit_count(&wt),
                expected_commits,
                "{node:?} must land exactly one commit"
            );
        }
        for edge in [
            &[
                "edge",
                "add",
                "requires",
                "requirements.stakeholder.crash-a",
                "requirements.requirement.crash-b",
            ][..],
            &[
                "edge",
                "add",
                "contains",
                "domain.group.g",
                "domain.service.svc",
            ][..],
            &[
                "edge",
                "add",
                "driven-by",
                "domain.service.svc",
                "requirements.requirement.crash-b",
            ][..],
            &[
                "edge",
                "add",
                "realises",
                "solution.component.cmp",
                "domain.service.svc",
            ][..],
            &[
                "edge",
                "add",
                "is-role",
                "solution.component.cmp",
                "solution.role.r",
            ][..],
        ] {
            let out = run(edge);
            assert!(
                out.status.success(),
                "{edge:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            expected_commits += 1;
            assert_eq!(
                commit_count(&wt),
                expected_commits,
                "{edge:?} must land exactly one commit"
            );
        }

        // --- (b) The whole burst is durable and complete. --------------------
        for (node_type, name) in [("stakeholder", "crash-a"), ("requirement", "crash-b")] {
            let nf = layers::read_node_file(&wt_apg, Layer::Requirements, node_type, name).unwrap();
            assert_eq!(nf.name, name, "node file {name} must be complete");
        }
        let all = layers::read_existing_nodes(&wt_apg).unwrap();
        layers::check_edge_pairing(&all).unwrap();
        let a =
            layers::read_node_file(&wt_apg, Layer::Requirements, "stakeholder", "crash-a").unwrap();
        assert!(
            a.out
                .iter()
                .any(|oe| oe.kind == "requires" && oe.target == "requirements.requirement.crash-b"),
            "the source out-half must be present and complete"
        );
        let b =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "crash-b").unwrap();
        assert!(
            b.in_edges
                .iter()
                .any(|ie| ie.kind == "requires" && ie.source == "requirements.stakeholder.crash-a"),
            "the target in-half must be present and match the out-half"
        );

        // --- (c) A writer killed mid-invocation loses nothing committed. -----
        let before_commits = commit_count(&wt);
        let before_head = head_sha(&wt);
        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();

        kill_write_blocked_on_the_worktree_lock(
            &wt,
            &wt_apg,
            &home,
            &["node", "add", "global", "constraint", "crash-killed"],
            || {},
        );

        // No half-written node file, no commit, no HEAD move: the durable store
        // is exactly its last committed state and still pairs.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "crash-killed").exists(),
            "the killed writer left a durable node file it should not have"
        );
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the killed write must leave apg/layers/** at its last committed state"
        );
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        assert_eq!(
            commit_count(&wt),
            before_commits,
            "a killed write must not create a commit"
        );
        assert_eq!(
            head_sha(&wt),
            before_head,
            "a killed write must not move HEAD"
        );

        // --- (d) No process holds `db.lbug`; no phantom projection. ---------
        {
            let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.stakeholder.crash-a"));
            assert!(db.has_node("requirements.requirement.crash-b"));
            assert!(db.has_node("domain.service.svc"));
            assert!(
                !db.has_node("global.constraint.crash-killed"),
                "the killed write must leave no phantom projection"
            );
        }

        // --- (e) The next read opens the last committed state; the next write
        // is atomic on its own. ----------------------------------------------
        let q = spawn_apg(
            &["query", "MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn"],
            &wt,
        );
        assert!(
            q.status.success(),
            "the next read failed: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        let stdout = String::from_utf8_lossy(&q.stdout);
        let rows: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            rows,
            vec!["n.fqn", "requirements.requirement.crash-b"],
            "the next read must open the last committed state"
        );
        assert!(
            !stdout.contains("crash-killed"),
            "the killed write must never be served"
        );

        let next = run(&["node", "add", "global", "constraint", "crash-next"]);
        assert!(
            next.status.success(),
            "{}",
            String::from_utf8_lossy(&next.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "crash-next").exists(),
            "the next write must write its node file"
        );
        assert_eq!(
            commit_count(&wt),
            before_commits + 1,
            "the next write must land exactly one commit"
        );
        assert_ne!(
            head_sha(&wt),
            before_head,
            "the next write must create a commit"
        );

        // No socket, no daemon and no pending set anywhere in the scenario.
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "a per-invocation write must leave no session socket"
        );

        remove(&repo);
    }
}
