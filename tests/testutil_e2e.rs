mod common;

use apg::layers::{self, Layer};
use apg::testutil::*;

/// e2e tier -- real I/O: both tests spawn the candidate `apg` binary,
/// create scratch repos and inspect the socket/`db.lbug` on disk. Each is
/// `#[ignore]`d, so a plain `cargo test` never runs one; the only entry
/// point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`). apg.testutil's own
/// non-`#[test]` items (Repo, ApgCommand, spawn_apg, start_session_process,
/// scan_checkout, payload helpers) are the E2E HARNESS and live at module
/// level, never inside a tier. PIN: testutil's I/O harness (the
/// git/process/db fixtures) is e2e-only — it must not be used by a
/// unit/int test; the pure graph-fixture builder `located` performs no I/O
/// and may be shared by any tier.
mod e2e {
    use super::*;

    /// Phase-02 task-13 (cli-session-crash-recovery) + task-24: the
    /// killed-session contract at the testutil level, all three triggers.
    ///
    /// A live `apg session start` owns `db.lbug` and BUFFERS its durable
    /// mutations; `apg session save` is the single durability point (the node
    /// files plus exactly one commit). A SIGKILL is an UNCLEAN EXIT: it leaves
    /// the socket file behind with no live process, and whatever the session
    /// admitted but never saved stays in the derived index as a phantom
    /// projection the durable `apg/layers/**` store never saw.
    ///
    /// (1) CLEAN state loses nothing. A node saved before the kill is durable;
    ///     after the SIGKILL the next `apg session start` reclaims the stale
    ///     socket, has `reclaim_stale_socket` detect the present-but-unreachable
    ///     socket as an unclean exit, discards and rebuilds `db.lbug` from
    ///     `apg/layers/**`, and still serves the saved node.
    /// (2) DIRTY state is discarded, never served. A mutation buffered AFTER
    ///     that save (its phantom projection sits in the held index, its node
    ///     file is not on disk) is thrown away by the same rebuild: the
    ///     recovered DB serves ONLY the last-saved state, and the durable node
    ///     files, commits and HEAD stay put.
    /// (3) The SAME discard+rebuild is triggered by `apg session save` (NOT
    ///     `session start`) against the stale socket: the save must reclaim the
    ///     socket AND repair the derived DB rather than merely deleting the
    ///     socket and stranding the phantom projection for the next reader.
    ///
    /// The stale socket is reclaimed in all three. The phase-01 assertions
    /// that still hold — no partial file, no `db.lbug` holder, socket reclaim —
    /// are kept.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn killed_session_loses_nothing_and_its_stale_socket_is_reclaimed() {
        let (repo, wt, wt_apg) = project_with_db("session-crash");
        let home = repo.root.join("home");
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

        // --- (1) CLEAN RUN: a node saved before the kill loses nothing. -----
        let session = start_session_process(&wt, &home);
        let saved_add = ApgCommand::new(&["node", "add", "requirements", "requirement", "saved"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            saved_add.status.success(),
            "{}",
            String::from_utf8_lossy(&saved_add.stderr)
        );
        // The single durability point: `save` flushes the buffer — one node
        // file, one commit.
        let save = spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved"
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "saved").exists(),
            "save must write the saved node file"
        );
        let saved_commits = commit_count(&wt);
        let saved_head = head_sha(&wt);

        // SIGKILL over a CLEAN buffer — deliberately NOT a graceful `end`.
        // The stale socket is the unclean-exit signal.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
        );
        let socket = apg::session::socket_path(&wt_apg);
        assert!(socket.exists(), "SIGKILL leaves the stale socket behind");
        assert!(
            !apg::session::live_session_at(&socket),
            "the socket is present but unreachable — the unclean-exit signal"
        );

        // The next start reclaims the stale socket (discard + rebuild from
        // `apg/layers/**`) and still serves the saved node: nothing was lost.
        let session2 = start_session_process(&wt, &home);
        let q_saved = spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.saved'}) RETURN count(n)",
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
            "the rebuilt index must keep the saved node"
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "saved").exists(),
            "the recovery must not lose the saved node file"
        );
        assert_eq!(
            commit_count(&wt),
            saved_commits,
            "the recovery must not add or drop a commit"
        );
        assert_eq!(head_sha(&wt), saved_head, "the recovery must not move HEAD");
        let end = spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out2 = session2.child.wait_with_output().unwrap();
        assert!(
            out2.status.success(),
            "{}",
            String::from_utf8_lossy(&out2.stderr)
        );
        let stderr2 = String::from_utf8_lossy(&out2.stderr);
        assert!(
            stderr2.contains("reclaimed stale socket"),
            "the next start must reclaim the stale socket: {stderr2}"
        );

        // --- (2) DIRTY RUN: an unsaved buffered mutation is discarded. -------
        let session = start_session_process(&wt, &home);

        // One routed mutation is admitted into the live session's write-back
        // buffer and projected into the held index, so it must NOT touch
        // `apg/layers/**` before a save — but a routed read DOES see it.
        let add = ApgCommand::new(&["node", "add", "requirements", "requirement", "survivor"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        let routed = spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.survivor'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            routed.status.success(),
            "{}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the live session's index must carry the buffered projection"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "survivor")
                .exists(),
            "a buffered add must not write a node file before save"
        );

        // Baseline for the durable-state assertions: commits, HEAD, and the
        // whole on-disk node-file store (still exactly the CLEAN run's save).
        let before_commits = commit_count(&wt);
        let before_head = head_sha(&wt);
        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();

        // SIGKILL — deliberately NOT a graceful `end`. The unsaved buffer dies
        // with the process while its phantom projection stays in `db.lbug`.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
        );
        let socket = apg::session::socket_path(&wt_apg);
        assert!(socket.exists(), "SIGKILL leaves the stale socket behind");
        assert!(
            !apg::session::live_session_at(&socket),
            "the socket is present but unreachable — the unclean-exit signal"
        );

        // (a) no half-written node file: the buffered `survivor` never became
        // durable, so the store is still exactly its last saved state.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "survivor")
                .exists(),
            "the killed session left a durable node file it should not have"
        );
        // (b) no paired edge half mismatched (the store still pairs cleanly).
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        // (c) no process holds db.lbug: a direct read-write open succeeds now,
        // and the STALE index it opens still carries the phantom projection the
        // kill left behind (so the rebuild below is a real discard).
        {
            let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(
                db.has_node("requirements.requirement.saved"),
                "the stale index must carry the last-saved node"
            );
            assert!(
                db.has_node("requirements.requirement.survivor"),
                "the stale index must carry the phantom buffered projection"
            );
        }

        // (d) the SIGKILL left the socket file behind; the next start reclaims
        // it (no live process behind it), detects the present-but-unreachable
        // socket as an unclean exit, discards the stale index (+ `db.lbug.wal`)
        // and rebuilds it from `apg/layers/**`, then serves normally — the
        // phantom projection is never served.
        let session2 = start_session_process(&wt, &home);
        let q = spawn_apg(
            &["query", "MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn"],
            &wt,
        );
        assert!(
            q.status.success(),
            "the recovery `apg query` failed: {}",
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
            vec!["n.fqn", "requirements.requirement.saved"],
            "the recovered query must serve only the last-saved state"
        );
        assert!(
            !stdout.contains("survivor"),
            "the recovered query must never serve the phantom projection"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "survivor")
                .exists(),
            "the discarded buffer must never write a node file"
        );
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the discarded buffer must leave apg/layers/** at its last-saved state"
        );
        assert_eq!(
            commit_count(&wt),
            before_commits,
            "a discarded buffer creates no commit"
        );
        assert_eq!(
            head_sha(&wt),
            before_head,
            "a discarded buffer must not move HEAD"
        );

        // The recovery leaves no live session behind once ended cleanly.
        let end2 = spawn_apg(&["session", "end"], &wt);
        assert!(
            end2.status.success(),
            "{}",
            String::from_utf8_lossy(&end2.stderr)
        );
        let out3 = session2.child.wait_with_output().unwrap();
        assert!(
            out3.status.success(),
            "{}",
            String::from_utf8_lossy(&out3.stderr)
        );
        let stderr3 = String::from_utf8_lossy(&out3.stderr);
        assert!(
            stderr3.contains("reclaimed stale socket"),
            "the next start must reclaim the stale socket: {stderr3}"
        );

        // --- (3) DIRTY RUN + `apg session save` (NOT `start`): the save
        // trigger must reclaim AND repair, not silently delete. ---------------
        let session = start_session_process(&wt, &home);

        // One routed mutation is admitted into the live session's write-back
        // buffer and projected into the held index — a routed read sees it,
        // but it never reaches `apg/layers/**` before a save.
        let add = ApgCommand::new(&["node", "add", "requirements", "requirement", "phantom"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        let routed = spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.phantom'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            routed.status.success(),
            "{}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the live session's index must carry the buffered projection"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "phantom")
                .exists(),
            "a buffered add must not write a node file before save"
        );

        // Baseline for the durable-state assertions: commits, HEAD, and the
        // whole on-disk node-file store.
        let before_commits = commit_count(&wt);
        let before_head = head_sha(&wt);
        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();

        // SIGKILL — deliberately NOT a graceful `end`. The unsaved buffer dies
        // with the process while its phantom projection stays in `db.lbug`.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
        );
        let socket = apg::session::socket_path(&wt_apg);
        assert!(socket.exists(), "SIGKILL leaves the stale socket behind");
        assert!(
            !apg::session::live_session_at(&socket),
            "the socket is present but unreachable — the unclean-exit signal"
        );

        // The stale index it opens still carries the phantom projection, so the
        // save-triggered rebuild below is a real discard.
        {
            let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(
                db.has_node("requirements.requirement.saved"),
                "the stale index must carry the last-saved node"
            );
            assert!(
                db.has_node("requirements.requirement.phantom"),
                "the stale index must carry the phantom buffered projection"
            );
        }

        // `apg session save` (NOT `session start`) against the stale socket: it
        // must reclaim the socket AND discard+rebuild the derived DB from
        // `apg/layers/**`, reporting the reclaimed stale socket — a bare
        // `remove_file(socket)` would strand the phantom projection with no
        // later socket for the crash-detection guard to trip on.
        let save = spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "no live session to save (reclaimed stale socket)",
            "the save over a stale socket must report the reclaim, not a silent delete"
        );
        assert!(
            !socket.exists(),
            "the save trigger must reclaim the stale socket file"
        );

        // The rebuilt index serves only the last-saved durable state: the
        // phantom is gone, the saved node survives.
        let q = spawn_apg(
            &["query", "MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn"],
            &wt,
        );
        assert!(
            q.status.success(),
            "the post-save read failed: {}",
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
            vec!["n.fqn", "requirements.requirement.saved"],
            "the save-triggered rebuild must serve only the last-saved state"
        );
        assert!(
            !stdout.contains("phantom"),
            "the save-triggered rebuild must never serve the phantom projection"
        );

        // The durable store, commits and HEAD stay at their last-saved state.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "phantom")
                .exists(),
            "the discarded buffer must never write a node file"
        );
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the discarded buffer must leave apg/layers/** at its last-saved state"
        );
        assert_eq!(
            commit_count(&wt),
            before_commits,
            "a discarded buffer creates no commit"
        );
        assert_eq!(
            head_sha(&wt),
            before_head,
            "a discarded buffer must not move HEAD"
        );

        remove(&repo);
    }

    /// Phase-01 task-35 (acceptance): a REAL long-running `apg session start`
    /// BUFFERS its durable mutations — writing NOTHING to `apg/layers/**` and
    /// making NO commit until the single save — and a SIGKILL (NOT a graceful
    /// `end`) after that save loses nothing durable and leaves no partial store:
    ///
    /// (a) no partial `apg/layers/**` file and no commit BEFORE save — the
    ///     buffered adds/edge leave the durable store exactly at its last saved
    ///     state (no half-written file, no commit);
    /// (b) `apg session save` is the single durability point: both node files
    ///     and BOTH halves of the routed edge land in exactly one commit;
    /// (c) no half-written node file after the crash — every expected file
    ///     parses with the right identity, and the store still pairs;
    /// (d) no process holds `db.lbug` — a direct read-write open succeeds;
    /// (e) the stale socket is reclaimed by the next `apg session start` with
    ///     no live process behind it.
    ///
    /// The routed mutations deliberately write a node AND an edge (both
    /// endpoint files), so a crash between the two halves of the edge would be
    /// caught by (c) — the node-only phase-03 regression cannot see that.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn acceptance_crash_durability_no_partial_files_no_db_holder_and_socket_reclaim() {
        let (repo, wt, wt_apg) = project_with_db("accept-crash");
        let home = repo.root.join("home");
        let session = start_session_process(&wt, &home);

        // Two routed node adds and the routed edge between them, all admitted
        // into the live session's write-back buffer.
        for name in ["crash-a", "crash-b"] {
            let add = ApgCommand::new(&["node", "add", "requirements", "requirement", name])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                add.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&add.stderr)
            );
        }
        let edge = ApgCommand::new(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.crash-a",
            "requirements.requirement.crash-b",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            edge.status.success(),
            "{}",
            String::from_utf8_lossy(&edge.stderr)
        );

        // (a) the buffer leaves `apg/layers/**` at its last saved state: no
        // partial node file for either buffered node and no commit before save.
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
        let before_commits = commit_count(&wt);
        let before_head = head_sha(&wt);
        for name in ["crash-a", "crash-b"] {
            assert!(
                !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", name).exists(),
                "a buffered add must not write a node file for `{name}` before save"
            );
        }
        assert_eq!(
            commit_count(&wt),
            before_commits,
            "a buffered mutation must not create a commit before save"
        );
        assert_eq!(
            head_sha(&wt),
            before_head,
            "git history must stay at the last saved state until save"
        );

        // (b) the single durability point: one `session save` flushes the whole
        // buffer — both node files, both edge halves, exactly one commit.
        let save = spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved"
        );
        for name in ["crash-a", "crash-b"] {
            assert!(
                layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", name).exists(),
                "save must write the `{name}` node file"
            );
        }
        assert_eq!(
            commit_count(&wt),
            before_commits + 1,
            "the whole buffered set must land in exactly one commit"
        );
        assert_ne!(head_sha(&wt), before_head, "save must create a commit");

        // SIGKILL — deliberately NOT a graceful `end`.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
        );

        // (c) no half-written node file after the crash: every expected file
        // parses with its identity intact.
        for name in ["crash-a", "crash-b"] {
            let nf =
                layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", name).unwrap();
            assert_eq!(nf.name, name, "node file {name} must be complete");
        }

        // (d) no paired edge half mismatched: the store pairs cleanly AND both
        // halves of the routed edge are present.
        let all = layers::read_existing_nodes(&wt_apg).unwrap();
        layers::check_edge_pairing(&all).unwrap();
        let a =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "crash-a").unwrap();
        assert!(
            a.out.iter().any(
                |oe| oe.kind == "depends-on" && oe.target == "requirements.requirement.crash-b"
            ),
            "the source out-half must be present and complete"
        );
        let b =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "crash-b").unwrap();
        assert!(
            b.in_edges.iter().any(
                |ie| ie.kind == "depends-on" && ie.source == "requirements.requirement.crash-a"
            ),
            "the target in-half must be present and match the out-half"
        );

        // (e) no process holds db.lbug: a direct read-write open succeeds now
        // (the SIGKILL released the OS lock) and the derived DB is consistent
        // with the saved store.
        let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.requirement.crash-a"));
        assert!(db.has_node("requirements.requirement.crash-b"));
        drop(db);

        // (f) the SIGKILL left the socket file behind; the next start reclaims
        // it (no live process behind it) and serves normally.
        let socket = apg::session::socket_path(&wt_apg);
        assert!(socket.exists(), "SIGKILL leaves the stale socket behind");
        let session2 = start_session_process(&wt, &home);
        let end = spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out2 = session2.child.wait_with_output().unwrap();
        assert!(
            out2.status.success(),
            "{}",
            String::from_utf8_lossy(&out2.stderr)
        );
        let stderr2 = String::from_utf8_lossy(&out2.stderr);
        assert!(
            stderr2.contains("reclaimed stale socket"),
            "the next start must reclaim the stale socket: {stderr2}"
        );

        remove(&repo);
    }
}
