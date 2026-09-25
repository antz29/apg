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

    /// Phase-03 task-19: a SIGKILLed session leaves NO half-written durable
    /// state and no process holding `db.lbug`, and the stale socket it leaves
    /// behind (no live process) is reclaimed by the next `apg session start`.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn killed_session_loses_nothing_and_its_stale_socket_is_reclaimed() {
        let (repo, wt, wt_apg) = project_with_db("session-crash");
        let home = repo.root.join("home");
        let session = start_session_process(&wt, &home);

        // One routed mutation so there is durable state to inspect.
        let add = ApgCommand::new(&["node", "add", "requirements", "requirement", "survivor"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );

        // SIGKILL — deliberately NOT a graceful `end`.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
        );

        // (a) no half-written node file: the survivor parses.
        let nf = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "survivor")
            .unwrap();
        assert_eq!(nf.name, "survivor");
        // (b) no paired edge half mismatched (the store still pairs cleanly).
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        // (c) no process holds db.lbug: a direct read-write open succeeds now.
        let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.requirement.survivor"));
        drop(db);

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

    /// Phase-04 task-2 (acceptance): a REAL long-running `apg session start`,
    /// SIGKILLed (NOT gracefully ended) mid-life, loses nothing durable and
    /// leaves no partial store:
    ///
    /// (a) no node file left half-written — every expected file parses with the
    ///     right identity;
    /// (b) no paired edge half mismatched — the store still pairs and both the
    ///     source out-half and target in-half of the routed edge are present;
    /// (c) no process holds `db.lbug` — a direct read-write open succeeds;
    /// (d) the stale socket is reclaimed by the next `apg session start` with
    ///     no live process behind it.
    ///
    /// The routed mutations deliberately write a node AND an edge (both
    /// endpoint files), so a crash between the two halves of the edge would be
    /// caught by (b) — the node-only phase-03 regression cannot see that.
    #[test]
    #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
    fn acceptance_crash_durability_no_partial_files_no_db_holder_and_socket_reclaim() {
        let (repo, wt, wt_apg) = project_with_db("accept-crash");
        let home = repo.root.join("home");
        let session = start_session_process(&wt, &home);

        // Two routed node adds and the routed edge between them.
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

        // SIGKILL — deliberately NOT a graceful `end`.
        let pid = session.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let out = session.child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "the session was killed, not ended cleanly"
        );

        // (a) no half-written node file: every expected file parses with its
        // identity intact.
        for name in ["crash-a", "crash-b"] {
            let nf =
                layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", name).unwrap();
            assert_eq!(nf.name, name, "node file {name} must be complete");
        }

        // (b) no paired edge half mismatched: the store pairs cleanly AND both
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

        // (c) no process holds db.lbug: a direct read-write open succeeds now
        // (the SIGKILL released the OS lock).
        let db = apg::artifacts::ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.requirement.crash-a"));
        assert!(db.has_node("requirements.requirement.crash-b"));
        drop(db);

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
}
