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

    /// Phase-01 task-38: a SIGKILLed session leaves NO half-written durable
    /// state and no process holding `db.lbug`, and the stale socket it leaves
    /// behind (no live process) is reclaimed by the next `apg session start`.
    ///
    /// Under mandatory admission the routed mutation is admitted into the live
    /// session's write-back buffer and never touches `apg/layers/**` before the
    /// single save, so a SIGKILL leaves the durable store exactly at its last
    /// saved state. The full crash-recovery discard/rebuild contract is
    /// PHASE-02 work (task-13 extends this test) and is deliberately NOT
    /// asserted here.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn killed_session_loses_nothing_and_its_stale_socket_is_reclaimed() {
        let (repo, wt, wt_apg) = project_with_db("session-crash");
        let home = repo.root.join("home");
        let session = start_session_process(&wt, &home);

        // One routed mutation is admitted into the live session's write-back
        // buffer, so it must NOT touch `apg/layers/**` before a save.
        let add = ApgCommand::new(&["node", "add", "requirements", "requirement", "survivor"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "survivor")
                .exists(),
            "a buffered add must not write a node file before save"
        );

        // SIGKILL — deliberately NOT a graceful `end`.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
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
        // (c) no process holds db.lbug: a direct read-write open succeeds now
        // (scoped so the OS lock is released before the next start).
        {
            let _db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
        }

        // (d) the SIGKILL left the socket file behind; the next start reclaims
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
