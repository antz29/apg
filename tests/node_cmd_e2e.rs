mod common;

use apg::artifacts::ArtifactDb;
use apg::layers::{self, InEdge, Layer, NodeFile, OutEdge, fqn};
use apg::node_cmd::*;
use apg::specs;
use apg::testutil::{self, Repo, av, spawn_apg};
use common::{with_cwd, wt_commit};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MOD: &str = "fixture.mod";
const FILE: &str = "/abs/store.go";

/// Writes `content` to `<wt>/<rel>`, creating parent dirs.
fn wt_write(wt: &Path, rel: &str, content: &str) {
    let p = wt.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// A project context for node-file-only tests: a real git repo whose
/// worktree `foo` on branch `foo` hosts the durable `apg/layers` store, but
/// with NO seed code and NO scan — so no `db.lbug`/code graph and no
/// process-wide `CWD_LOCK` hold. `write_project` skips its projection leg
/// when `db.lbug` is absent, so the write/validate/commit path (and every
/// node-file read) behaves identically. Returns `(wt_apg_root, repo, wt_root)`.
fn node_store_fixture(tag: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = Repo::new(&format!("nodecmd-{tag}"));
    let wt = repo.start_project("foo");
    let wt_apg = wt.join(specs::LAYOUT);
    (wt_apg, repo, wt)
}

/// A real project context (R3/R4): a git repo whose worktree `foo` on
/// branch `foo` carries a real `apg/.trans/db.lbug` code graph built by
/// the hermetic scan fixture — the context every `apg node`/`apg edge`
/// mutation runs in. Returns `(wt_apg_root, repo, wt_root)`.
fn mutation_fixture(tag: &str) -> (PathBuf, Repo, PathBuf) {
    let (wt_apg, repo, wt) = node_store_fixture(tag);
    wt_write(
        &wt,
        "code/seed.scan.jsonl",
        &testutil::code_payload(MOD, FILE, &["Store"]),
    );
    wt_commit(&wt, &["code/seed.scan.jsonl"], "seed code");
    testutil::scan_checkout(&wt).unwrap();
    (wt_apg, repo, wt)
}

/// A bare node file with no edges — the exact shape `apg node add`
/// builds.
fn node(layer: &str, node_type: &str, name: &str) -> NodeFile {
    NodeFile {
        layer: layer.to_string(),
        node_type: node_type.to_string(),
        name: name.to_string(),
        body: String::new(),
        properties: BTreeMap::new(),
        out: Vec::new(),
        in_edges: Vec::new(),
    }
}

/// Attributes a failed real-CLI `apg` process to the lock it lost on, by
/// its stderr — the per-lock evidence the burst records: the lbug
/// `apg/.trans/db.lbug` read-write file lock, git's `.git/index.lock`, the
/// `apg/.trans/specs.lock` flock, or the unlocked node-file
/// read-modify-write (which surfaces as a pairing mismatch when one
/// process's hub write clobbers another's).
fn classify_lock(stderr: &str) -> &'static str {
    if stderr.contains("Could not set lock on file") {
        "lbug apg/.trans/db.lbug"
    } else if stderr.contains("the index is locked")
        || stderr.contains("index.lock")
        || stderr.contains("failed to lock")
    {
        "git .git/index.lock"
    } else if stderr.contains("specs.lock") || stderr.contains("could not acquire write lock") {
        "specs.lock flock"
    } else if stderr.contains("has no matching out edge") || stderr.contains("no matching in-half")
    {
        "node-file RMW (pairing mismatch)"
    } else {
        "other"
    }
}

/// Creates `hub` + `leaf-0..n` requirement nodes in a single durable
/// mutation. Setup only establishes the target nodes; the burst under test
/// is the edge/node work, so N+1 dispatched `cmd_node` calls (each its own
/// commit + DB projection) collapse into one `write_project`.
fn setup_hub_and_leaves(wt: &Path, n: usize) {
    let mut nodes = vec![node("requirements", "requirement", "hub")];
    for i in 0..n {
        nodes.push(node("requirements", "requirement", &format!("leaf-{i}")));
    }
    layers::write_project(&wt.join(specs::LAYOUT), &nodes, &[]).unwrap();
}

/// Starts N separate `apg edge add hub -> leaf-i` processes back-to-back,
/// waits for all, and returns `(failed, per-lock attribution)`.
fn run_edge_burst(wt: &Path, home: &Path, n: usize) -> (usize, BTreeMap<&'static str, usize>) {
    std::fs::create_dir_all(home).unwrap();
    let mut children = Vec::with_capacity(n);
    for i in 0..n {
        let to = format!("requirements.requirement.leaf-{i}");
        let child = testutil::ApgCommand::new(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.hub",
            to.as_str(),
        ])
        .cwd(wt)
        .env("HOME", home.to_str().unwrap())
        .spawn();
        children.push((i, child));
    }
    let mut failed = 0usize;
    let mut by_lock: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (i, child) in children {
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            failed += 1;
            *by_lock.entry(classify_lock(&stderr)).or_default() += 1;
            eprintln!("burst[{i}] FAILED ({}): {stderr}", classify_lock(&stderr));
        }
    }
    (failed, by_lock)
}

/// The shared hub's out-edge count in the durable node-file store.
fn hub_out_edges(wt_apg: &Path) -> usize {
    layers::read_node_file(wt_apg, Layer::Requirements, "requirement", "hub")
        .unwrap()
        .out
        .len()
}

/// Forwards a node mutation to the live session with a chosen client id (the
/// at-most-once replay primitive), panicking on transport errors.
fn session_forward_node(apg_root: &Path, client_id: &str, args: &[String]) -> String {
    apg::session::Coordinator::forward_mutation_with_id(apg_root, client_id, "node", args).unwrap()
}

/// Ends a live session and returns its (captured) output.
fn end_session(wt: &Path, session: testutil::SessionProcess) -> std::process::Output {
    let end = testutil::spawn_apg(&["session", "end"], wt);
    assert!(
        end.status.success(),
        "{}",
        String::from_utf8_lossy(&end.stderr)
    );
    session.child.wait_with_output().unwrap()
}

/// Records the per-lock outcome of EVERY burst child (successes included as
/// `"ok"`), so the acceptance test can assert zero lock errors on each named
/// lock rather than only counting failures. Starts N separate `apg edge add
/// hub -> leaf-i` processes back-to-back and classifies each by its stderr.
fn run_edge_burst_attributed(wt: &Path, home: &Path, n: usize) -> BTreeMap<&'static str, usize> {
    std::fs::create_dir_all(home).unwrap();
    let mut children = Vec::with_capacity(n);
    for i in 0..n {
        let to = format!("requirements.requirement.leaf-{i}");
        let child = testutil::ApgCommand::new(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.hub",
            to.as_str(),
        ])
        .cwd(wt)
        .env("HOME", home.to_str().unwrap())
        .spawn();
        children.push((i, child));
    }
    let mut hist: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (i, child) in children {
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let key = if out.status.success() {
            "ok"
        } else {
            let lock = classify_lock(&stderr);
            eprintln!("accept-burst[{i}] FAILED ({lock}): {stderr}");
            lock
        };
        *hist.entry(key).or_default() += 1;
    }
    hist
}

/// e2e tier -- real I/O: every test here drives real git repos, node-file
/// writes, `db.lbug` reads or spawned `apg` processes. Each is `#[ignore]`d,
/// so a plain `cargo test` never runs one; the only entry point is the named
/// guard `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    /// R17 VI (write half): `apg node add` and `apg edge add` never create or
    /// modify `apg/specs/` or `apg/notes/`. Pre-existing committed legacy
    /// files stay byte-identical through node and edge mutations, and no new
    /// file appears beside them — the mutations land in `apg/layers/` + the
    /// branch DB only.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_and_edge_mutations_leave_pre_existing_legacy_files_untouched() {
        let repo = Repo::new("nodecmd-vi-unwritten");
        let wt = repo.start_project("foo");
        let wt_apg = wt.join(specs::LAYOUT);
        // Committed legacy durable files with sentinel content.
        std::fs::create_dir_all(wt_apg.join("specs")).unwrap();
        std::fs::create_dir_all(wt_apg.join("notes")).unwrap();
        std::fs::write(wt_apg.join("specs").join("foo.jsonl"), "SENTINEL SPEC\n").unwrap();
        std::fs::write(
            wt_apg.join("notes").join("fixture.mod.jsonl"),
            "SENTINEL NOTE\n",
        )
        .unwrap();
        wt_write(
            &wt,
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        wt_commit(
            &wt,
            &[
                "code/seed.scan.jsonl",
                "apg/specs/foo.jsonl",
                "apg/notes/fixture.mod.jsonl",
            ],
            "seed code + legacy durable files",
        );
        testutil::scan_checkout(&wt).unwrap();

        // `apg node add` twice, then `apg edge add` once (the command shapes).
        layers::write_project(&wt_apg, &[node("requirements", "requirement", "r1")], &[]).unwrap();
        layers::write_project(&wt_apg, &[node("requirements", "requirement", "r2")], &[]).unwrap();
        let mut src =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        src.out.push(OutEdge {
            kind: "depends-on".to_string(),
            target: "requirements.requirement.r2".to_string(),
            properties: BTreeMap::new(),
        });
        let mut dst =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        dst.in_edges.push(InEdge {
            kind: "depends-on".to_string(),
            source: "requirements.requirement.r1".to_string(),
            properties: BTreeMap::new(),
        });
        layers::write_project(&wt_apg, &[src, dst], &[]).unwrap();

        // The legacy files are byte-identical, and nothing new appeared.
        assert_eq!(
            std::fs::read_to_string(wt_apg.join("specs").join("foo.jsonl")).unwrap(),
            "SENTINEL SPEC\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt_apg.join("notes").join("fixture.mod.jsonl")).unwrap(),
            "SENTINEL NOTE\n"
        );
        assert_eq!(
            std::fs::read_dir(wt_apg.join("specs")).unwrap().count(),
            1,
            "apg/specs must not gain files"
        );
        assert_eq!(
            std::fs::read_dir(wt_apg.join("notes")).unwrap().count(),
            1,
            "apg/notes must not gain files"
        );
        testutil::remove(&repo);
    }

    /// Phase-01 task-11: a durable `node`/`edge` mutation with NO live session
    /// REFUSES and names `apg session start` — durable mutations are
    /// mandatory-session (the direct path is gone), so the CLI must tell the
    /// caller how to open one rather than silently writing to disk. A refused
    /// mutation leaves the durable store (`apg/layers/**`) and the git history
    /// untouched: no node file written, no `apg/layers` directory created, and
    /// no commit.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn durable_mutation_requires_a_live_session() {
        let (wt_apg, repo, wt) = node_store_fixture("session-required");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let layers_dir = wt_apg.join(layers::LAYERS_DIR);
        let before_commits = testutil::commit_count(&wt);
        assert!(
            !layers_dir.exists(),
            "fixture must start with no apg/layers store"
        );

        // `apg node add …` with no live session: refuses, naming the fix.
        let node_add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "no-session",
            "--body",
            "must not land",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            !node_add.status.success(),
            "a durable node add must refuse without a live session"
        );
        let node_stderr = String::from_utf8_lossy(&node_add.stderr);
        assert!(
            node_stderr.contains("apg session start"),
            "the node refusal must name `apg session start`: {node_stderr}"
        );

        // `apg edge add …` with no live session: refuses the same way.
        let edge_add = testutil::ApgCommand::new(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.a",
            "requirements.requirement.b",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            !edge_add.status.success(),
            "a durable edge add must refuse without a live session"
        );
        let edge_stderr = String::from_utf8_lossy(&edge_add.stderr);
        assert!(
            edge_stderr.contains("apg session start"),
            "the edge refusal must name `apg session start`: {edge_stderr}"
        );

        // The durable store and git history are UNCHANGED: no node file
        // written, no `apg/layers` directory created, no commit.
        assert!(
            !layers_dir.exists(),
            "a refused mutation must not create apg/layers"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "no-session")
                .exists(),
            "a refused node add must not write a node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a refused mutation must not create a commit"
        );
        testutil::remove(&repo);
    }

    /// `apg node add` through a live session lands ONE file per node at
    /// `<layer>/<type>/<name>.json` at the session's single durability point
    /// (`apg session save`) — the file name IS the identity, the FQN is
    /// derived `<layer>.<type>.<name>`, visible in the branch DB, committed on
    /// the project branch, and never creating the legacy
    /// `apg/specs/`/`apg/notes/` paths.
    ///
    /// The durable add is ADMITTED into the session's write-back buffer (no
    /// `apg/layers/**` file, no commit), and only `apg session save` flushes
    /// it — one atomic node-file write plus one commit — the state the
    /// on-disk identity and branch-DB assertions below pin.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_add_lands_one_file_per_node_with_identity_and_branch_db_visibility() {
        let (wt_apg, repo, wt) = mutation_fixture("node-add");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // `apg node add requirements requirement timer` — admitted into the
        // write-back buffer, so nothing is durable yet.
        let add =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "timer"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&add.stdout).trim(),
            "Added node requirements.requirement.timer"
        );

        // The file name is the identity, and the add BUFFERS: no node file
        // appears before save (the durability point below).
        let path = wt_apg
            .join(layers::LAYERS_DIR)
            .join("requirements")
            .join("requirement")
            .join("timer.json");
        assert!(
            !path.exists(),
            "a buffered add must not write {} before save",
            path.display()
        );

        // The single durability point: `apg session save` flushes the buffer
        // with one atomic node-file write plus one commit.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved"
        );

        // One file per node at the derived path; the file name is the identity.
        assert!(path.exists(), "{} must exist", path.display());
        let back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "timer").unwrap();
        assert_eq!(back.name, "timer");
        assert_eq!(
            fqn(Layer::Requirements, "requirement", "timer"),
            "requirements.requirement.timer"
        );

        // Visible in the branch DB: a routed read sees the saved node while
        // the session is still live.
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.timer'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q.status.success(),
            "routed query: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the saved node must be visible in the branch DB"
        );

        // Never touches the legacy durable paths.
        assert!(
            !wt_apg.join("specs").exists(),
            "apg/specs must not be created by a node add"
        );
        assert!(
            !wt_apg.join("notes").exists(),
            "apg/notes must not be created by a node add"
        );

        // The node file auto-committed on the project branch (R8).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            head.tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/requirement/timer.json"))
                .is_ok(),
            "the node file must be committed on the project branch"
        );

        // The branch `db.lbug` reflects the saved node once the session ends.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.requirement.timer"));
        drop(db);

        testutil::remove(&repo);
    }

    /// Phase-01 task-16: `apg edge add` (the command shape) through a LIVE
    /// session writes BOTH endpoint files at the session's single durability
    /// point (`apg session save`) — the out half in the source's file, the
    /// matching in half in the target's, with identical properties — keeping
    /// the store pairing-consistent and landing the edge in the branch DB.
    ///
    /// The two endpoint adds and the edge add are ADMITTED into the session's
    /// write-back buffer (no `apg/layers/**` file, no commit); only
    /// `apg session save` flushes them — one atomic node-file write plus exactly
    /// one commit — the on-disk halves, pairing and commit count asserted here.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn edge_add_writes_both_endpoint_files() {
        let (wt_apg, repo, wt) = mutation_fixture("edge-add");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // `apg node add` × 2 and `apg edge add depends-on
        // requirements.requirement.r1 requirements.requirement.r2`, each routed
        // through the live session and admitted into the write-back buffer.
        let run = |args: &[&str], expected: &str| {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                expected,
                "apg {args:?}"
            );
        };
        let before_commits = testutil::commit_count(&wt);
        run(
            &["node", "add", "requirements", "requirement", "r1"],
            "Added node requirements.requirement.r1",
        );
        run(
            &["node", "add", "requirements", "requirement", "r2"],
            "Added node requirements.requirement.r2",
        );
        run(
            &[
                "edge",
                "add",
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ],
            "Added edge depends-on requirements.requirement.r1 -> requirements.requirement.r2",
        );

        // Buffered: neither endpoint file exists yet and no commit landed.
        for name in ["r1", "r2"] {
            assert!(
                !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", name).exists(),
                "a buffered add must not write `{name}` before save"
            );
        }
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered mutation must not create a commit before save"
        );

        // The single durability point: `apg session save` flushes the buffer
        // with one atomic node-file write plus one commit.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved"
        );

        // Both halves landed on disk: out in the source's file, in in the
        // target's, with matching kind/endpoints and identical properties.
        let src_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let dst_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        assert_eq!(src_back.out.len(), 1);
        assert_eq!(src_back.out[0].kind, "depends-on");
        assert_eq!(src_back.out[0].target, "requirements.requirement.r2");
        assert_eq!(dst_back.in_edges.len(), 1);
        assert_eq!(dst_back.in_edges[0].kind, "depends-on");
        assert_eq!(dst_back.in_edges[0].source, "requirements.requirement.r1");
        assert_eq!(
            src_back.out[0].properties, dst_back.in_edges[0].properties,
            "the out half and its matching in half must carry identical properties"
        );

        // The store stays pairing-consistent.
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();

        // Exactly ONE commit landed for the whole buffered set, and it moved
        // HEAD.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the buffered endpoints + edge must land in exactly one save commit"
        );
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // The edge is in the branch DB — assert after the session ends, against
        // the db.lbug the save left behind.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let out = db
        .q("MATCH (a:Requirement {fqn: 'requirements.requirement.r1'})-[:DependsOn]->(b:Requirement {fqn: 'requirements.requirement.r2'}) RETURN count(*)")
        .unwrap();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the DependsOn edge must be in the branch DB: {out}"
        );
        drop(db);
        testutil::remove(&repo);
    }

    /// Authored `uses` (Person→System) and `calls` (Service→Service) edges
    /// survive the write-through re-merge: `artifacts::edge_merge` maps both
    /// record kinds and the merge guard admits their rel pairs, so the branch
    /// DB shows them exactly like a full scan would. Regression for the
    /// `_ => None` arm that used to drop them.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn edge_add_lands_authored_uses_and_calls_in_the_branch_db() {
        let (wt_apg, repo, _wt) = mutation_fixture("authored-edges");
        // `apg node add` × 4: a Person + System (the `uses` pair) and two
        // Services (the `calls` pair).
        layers::write_project(
            &wt_apg,
            &[
                node("solution", "person", "alice"),
                node("solution", "system", "portal"),
                node("domain", "service", "svc-a"),
                node("domain", "service", "svc-b"),
            ],
            &[],
        )
        .unwrap();

        // `apg edge add uses solution.person.alice solution.system.portal` and
        // `apg edge add calls domain.service.svc-a domain.service.svc-b` —
        // both halves of each edge in one mutation.
        let mut person =
            layers::read_node_file(&wt_apg, Layer::Solution, "person", "alice").unwrap();
        person.out.push(OutEdge {
            kind: "uses".to_string(),
            target: "solution.system.portal".to_string(),
            properties: BTreeMap::new(),
        });
        let mut system =
            layers::read_node_file(&wt_apg, Layer::Solution, "system", "portal").unwrap();
        system.in_edges.push(InEdge {
            kind: "uses".to_string(),
            source: "solution.person.alice".to_string(),
            properties: BTreeMap::new(),
        });
        let mut svc_a = layers::read_node_file(&wt_apg, Layer::Domain, "service", "svc-a").unwrap();
        svc_a.out.push(OutEdge {
            kind: "calls".to_string(),
            target: "domain.service.svc-b".to_string(),
            properties: BTreeMap::new(),
        });
        let mut svc_b = layers::read_node_file(&wt_apg, Layer::Domain, "service", "svc-b").unwrap();
        svc_b.in_edges.push(InEdge {
            kind: "calls".to_string(),
            source: "domain.service.svc-a".to_string(),
            properties: BTreeMap::new(),
        });
        layers::write_project(&wt_apg, &[person, system, svc_a, svc_b], &[]).unwrap();

        // Both authored edges are in the branch DB after the write-through
        // re-merge.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let uses = db
        .q("MATCH (p:Person {fqn: 'solution.person.alice'})-[:Uses]->(s:System {fqn: 'solution.system.portal'}) RETURN count(*)")
        .unwrap();
        assert_eq!(
            uses.lines().last().map(str::trim),
            Some("1"),
            "the authored Uses edge must survive the write-through re-merge: {uses}"
        );
        let calls = db
        .q("MATCH (a:Service {fqn: 'domain.service.svc-a'})-[:Calls]->(b:Service {fqn: 'domain.service.svc-b'}) RETURN count(*)")
        .unwrap();
        assert_eq!(
            calls.lines().last().map(str::trim),
            Some("1"),
            "the authored Calls edge must survive the write-through re-merge: {calls}"
        );
        drop(db);
        testutil::remove(&repo);
    }

    /// Reads see the node files: `read_existing_nodes` returns every written
    /// node; `read_node_file`/`node_file_path` resolve the identity.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn reads_see_the_node_files() {
        let (wt_apg, repo, _wt) = node_store_fixture("reads");
        let mut ent = node("domain", "entity", "customer");
        ent.properties
            .insert("kind".to_string(), "entity".to_string());
        layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "r1"), ent],
            &[],
        )
        .unwrap();

        let all = layers::read_existing_nodes(&wt_apg).unwrap();
        assert_eq!(all.len(), 2);
        let fqns: BTreeSet<String> = all
            .iter()
            .map(|n| fqn(resolve_layer(&n.layer).unwrap(), &n.node_type, &n.name))
            .collect();
        assert!(fqns.contains("requirements.requirement.r1"));
        assert!(fqns.contains("domain.entity.customer"));
        let path = layers::node_file_path(&wt_apg, Layer::Domain, "entity", "customer");
        assert!(path.exists(), "{} must exist", path.display());
        let back = layers::read_node_file(&wt_apg, Layer::Domain, "entity", "customer").unwrap();
        assert_eq!(back.name, "customer");
        testutil::remove(&repo);
    }

    /// A failed multi-file mutation leaves NO partial state: the complete
    /// change is validated before anything is written, so an invalid second
    /// endpoint (allowlist-violating name, dangling authored edge, or a NEW
    /// constraint carrying an `attaches-to`) writes nothing — the pre-existing
    /// files stay byte-identical and no new file lands.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn failed_multi_file_mutation_leaves_no_partial_state() {
        let (wt_apg, repo, _wt) = mutation_fixture("atomicity");
        // Pre-existing state: one committed node file.
        layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "existing")],
            &[],
        )
        .unwrap();
        let existing_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "existing");
        let before = std::fs::read_to_string(&existing_path).unwrap();

        // (a) An invalid second endpoint (allowlist-violating name) — neither
        // file lands, the pre-existing state is untouched.
        let bad = node("requirements", "requirement", "Bad Name");
        let err = layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "fresh"), bad],
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("allowlist"), "{err}");
        let fresh_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "fresh");
        assert!(
            !fresh_path.exists(),
            "a failed mutation must not write the valid half"
        );
        assert_eq!(std::fs::read_to_string(&existing_path).unwrap(), before);

        // (b) A dangling authored edge endpoint — pairing fails validation,
        // nothing is written.
        let mut a = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "existing")
            .unwrap();
        a.out.push(OutEdge {
            kind: "depends-on".to_string(),
            target: "requirements.requirement.ghost".to_string(),
            properties: BTreeMap::new(),
        });
        let victim = node("requirements", "requirement", "victim");
        let err = layers::write_project(&wt_apg, &[a, victim], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("requirements.requirement.ghost"), "{msg}");
        let victim_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "victim");
        assert!(!victim_path.exists());
        assert_eq!(std::fs::read_to_string(&existing_path).unwrap(), before);

        // (c) A NEW constraint carrying an `attaches-to` — regardless of
        // resolution — is refused at write time (R2: a constraint's scope is
        // its layer), before anything lands; the positive control is a NEW
        // tier-scoped constraint with NO `attaches-to`.
        let mut c = node("requirements", "constraint", "law");
        c.properties.insert(
            layers::PROP_ATTACHES_TO.to_string(),
            "domain.entity.ghost".to_string(),
        );
        let err = layers::write_project(&wt_apg, &[c], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("attaches-to"), "{msg}");
        let law_path = layers::node_file_path(&wt_apg, Layer::Requirements, "constraint", "law");
        assert!(
            !law_path.exists(),
            "a refused constraint write must not land a file"
        );
        assert_eq!(std::fs::read_to_string(&existing_path).unwrap(), before);
        // A NEW constraint WITHOUT `attaches-to` is well-formed and re-merges.
        let c = node("domain", "constraint", "law");
        layers::write_project(&wt_apg, &[c], &[]).unwrap();
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("domain.constraint.law"));

        // The store still pairs cleanly throughout.
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        testutil::remove(&repo);
    }

    /// Strict node surface against a real repo/branch DB: `node add` refuses an
    /// existing FQN (naming update/rm) and writes nothing; `node update` is
    /// edge-preserving (body/properties merge, incident edges identical) and is
    /// refused when the node is absent.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_add_refuses_existing_and_node_update_preserves_edges() {
        let (wt_apg, repo, _wt) = node_store_fixture("node-strict");
        // Author r1 --depends-on--> r2 through the real CLI arms.
        node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap();
        node_add(&wt_apg, &av(&["requirements", "requirement", "r2"])).unwrap();
        edge_add(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap();
        let before =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(before.out.len(), 1);
        let path = layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "r1");
        let bytes_before = std::fs::read_to_string(&path).unwrap();

        // Re-adding r1 is refused, naming both follow-ups; nothing is written.
        let err = node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg node update"), "{msg}");
        assert!(msg.contains("apg node rm"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes_before,
            "a refused re-add must write nothing"
        );

        // node update: body + merged property; every incident edge is identical.
        node_update(
            &wt_apg,
            &av(&[
                "requirements",
                "requirement",
                "r1",
                "--body",
                "updated body",
                "--property",
                "a=1",
            ]),
        )
        .unwrap();
        let after =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(after.body, "updated body");
        assert_eq!(after.properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(
            after.out, before.out,
            "out-edges must survive a node update"
        );
        assert_eq!(
            after.in_edges, before.in_edges,
            "in-edges must survive a node update"
        );

        // Updating an absent node is refused.
        let err = node_update(&wt_apg, &av(&["requirements", "requirement", "ghost"])).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        testutil::remove(&repo);
    }

    /// Strict edge surface against a real repo/branch DB: `edge add` refuses an
    /// identical `(kind, from, to)`; `edge update` rewrites the source out-half
    /// AND the target in-half to the same MERGEd property map, with an explicit
    /// `--unset-property` the only way to drop a key.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn edge_add_refuses_duplicate_and_edge_update_merges_both_halves() {
        let (wt_apg, repo, _wt) = node_store_fixture("edge-strict");
        node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap();
        node_add(&wt_apg, &av(&["requirements", "requirement", "r2"])).unwrap();
        edge_add(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "a=0",
                "--property",
                "b=2",
            ]),
        )
        .unwrap();

        // A duplicate triple is refused, naming both follow-ups; both halves
        // still number exactly one.
        let err = edge_add(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg edge update"), "{msg}");
        assert!(msg.contains("apg edge rm"), "{msg}");
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        assert_eq!(r1.out.len(), 1, "the duplicate must not add an out-half");
        assert_eq!(
            r2.in_edges.len(),
            1,
            "the duplicate must not add an in-half"
        );

        // edge update --property a=1 MERGEs on BOTH halves: {a:1,b:2}.
        edge_update(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "a=1",
            ]),
        )
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        let expect = BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        assert_eq!(r1.out[0].properties, expect);
        assert_eq!(
            r2.in_edges[0].properties, expect,
            "the target in-half must carry the same map"
        );

        // --unset-property b drops exactly b on both halves: {a:1}.
        edge_update(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--unset-property",
                "b",
            ]),
        )
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        let expect = BTreeMap::from([("a".to_string(), "1".to_string())]);
        assert_eq!(r1.out[0].properties, expect);
        assert_eq!(r2.in_edges[0].properties, expect);

        // Omitting --unset-property keeps every key.
        edge_update(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "c=3",
            ]),
        )
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(r1.out[0].properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(r1.out[0].properties.get("c").map(String::as_str), Some("3"));

        // Updating an absent edge is refused.
        let err = edge_update(
            &wt_apg,
            &av(&[
                "drives",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        testutil::remove(&repo);
    }

    /// Phase-7 task-2 (E2E, top-level dispatch): `apg node update` is
    /// edge-preserving and MERGEs body/properties (only an explicit
    /// `--unset-property` drops a key); `apg edge update` rewrites BOTH the
    /// source out-half and the target in-half to the same MERGEd map; `apg
    /// edge add` refuses a duplicate `(kind, from, to)`.
    ///
    /// Phase-01 task-2 (ROOT-CAUSE characterization) — updated by phase-02 for
    /// the fixed tree: the node/edge funnel now takes the **extended**
    /// whole-durable-sequence flock at the `cmd_node`/`cmd_edge` dispatch
    /// entry, and its DB opens are exclusively the read-write projection class
    /// (validation never opens the DB; the staleness re-anchor is
    /// graph.jsonl-only).
    ///
    /// - The fix: `cmd_node`/`cmd_edge` acquire
    ///   `apg.artifacts.acquire_spec_lock` once at dispatch, **before any
    ///   node-file read**, and hold it across validate → write → the one-commit-
    ///   per-mutation git commit → the projection. The lock file
    ///   `apg/.trans/specs.lock` is created and observed below; it is released
    ///   when the command returns (the guard drops), so a later spawned process
    ///   can acquire it (step 5).
    /// - Read-only class: `apg query` opens with
    ///   `SystemConfig::default().read_only(true)` (`main.rs:963`).
    /// - Read-write class: `ArtifactDb::open` (default config), now reached
    ///   ONLY by the post-commit projection apply (`reingest_layers`) —
    ///   validation resolves code FQNs from `graph.jsonl` via
    ///   `code_universes_from_export` (no DB), and `git::reanchor_scan_meta`
    ///   no longer opens `db.lbug`.
    /// - WHICH fails: a second read-write open of the same DB **fails** across
    ///   processes (`Could not set lock on file … Resource temporarily
    ///   unavailable`); the read-only `apg query` **coexists** with a live
    ///   read-write handle (it is not the failing class).
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_edge_entry_takes_extended_spec_lock_and_db_open_taxonomy() {
        let (wt_apg, repo, wt) = mutation_fixture("lock-taxonomy");

        // (1) The fix: the node/edge entry acquires the extended
        // whole-durable-sequence flock, so the lock file `acquire_spec_lock`
        // creates exists — and each command releases it on return.
        with_cwd(&wt, || {
            cmd_node(&av(&["add", "requirements", "requirement", "gap-a"]))
        })
        .unwrap();
        with_cwd(&wt, || {
            cmd_node(&av(&["add", "requirements", "requirement", "gap-b"]))
        })
        .unwrap();
        with_cwd(&wt, || {
            cmd_edge(&av(&[
                "add",
                "depends-on",
                "requirements.requirement.gap-a",
                "requirements.requirement.gap-b",
            ]))
        })
        .unwrap();
        let spec_lock = wt_apg.join(specs::TRANS).join("specs.lock");
        assert!(
            spec_lock.exists(),
            "the node/edge entry must acquire the extended specs.lock flock: {} missing",
            spec_lock.display()
        );

        // (2) Read-only baseline: `apg query` succeeds with no writer.
        let baseline = spawn_apg(&["query", "MATCH (n:Requirement) RETURN count(n)"], &wt);
        assert!(
            baseline.status.success(),
            "apg query (read-only) must succeed with no concurrent writer: {}",
            String::from_utf8_lossy(&baseline.stderr)
        );

        // (3) Read-write class is exclusive cross-process: with a read-write
        // `ArtifactDb` held, a second read-write opener (the node/edge path's
        // DB open) fails outright — this is the class the burst loses on.
        let held = ArtifactDb::open(&wt_apg).unwrap();
        let loser = spawn_apg(
            &["node", "add", "requirements", "requirement", "rw-loser"],
            &wt,
        );
        assert!(
            !loser.status.success(),
            "a second read-write DB opener must fail while one is held"
        );
        let loser_stderr = String::from_utf8_lossy(&loser.stderr);
        assert!(
            loser_stderr.contains("Could not set lock on file"),
            "the read-write loser must fail on the lbug file lock: {loser_stderr}"
        );

        // (4) Read-only class coexists: `apg query` still succeeds while the
        // read-write handle is held — the read-only opener is NOT the failing
        // class in the pre-fix taxonomy.
        let ro = spawn_apg(&["query", "MATCH (n:Requirement) RETURN count(n)"], &wt);
        assert!(
            ro.status.success(),
            "read-only apg query must coexist with a live read-write handle: {}",
            String::from_utf8_lossy(&ro.stderr)
        );
        drop(held);

        // (5) Control: once the read-write handle is released, the same
        // mutation succeeds.
        let winner = spawn_apg(
            &["node", "add", "requirements", "requirement", "rw-winner"],
            &wt,
        );
        assert!(
            winner.status.success(),
            "the mutation must succeed once the read-write handle is released: {}",
            String::from_utf8_lossy(&winner.stderr)
        );

        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn strict_node_and_edge_update_preserve_edges_through_dispatch() {
        let (wt_apg, repo, wt) = node_store_fixture("dispatch-strict");

        // r1 carries its own properties + one in-edge (r0 -> r1) and one
        // out-edge (r1 -> r2) so a node update can be checked edge-for-edge.
        with_cwd(&wt, || {
            cmd_node(&av(&["add", "requirements", "requirement", "r0"]))
        })
        .unwrap();
        with_cwd(&wt, || {
            cmd_node(&av(&[
                "add",
                "requirements",
                "requirement",
                "r1",
                "--property",
                "a=0",
                "--property",
                "b=2",
            ]))
        })
        .unwrap();
        with_cwd(&wt, || {
            cmd_node(&av(&["add", "requirements", "requirement", "r2"]))
        })
        .unwrap();
        with_cwd(&wt, || {
            cmd_edge(&av(&[
                "add",
                "depends-on",
                "requirements.requirement.r0",
                "requirements.requirement.r1",
            ]))
        })
        .unwrap();
        let edge = av(&[
            "add",
            "depends-on",
            "requirements.requirement.r1",
            "requirements.requirement.r2",
        ]);
        with_cwd(&wt, || cmd_edge(&edge)).unwrap();

        let before =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(before.out.len(), 1, "one out-edge before the update");
        assert_eq!(before.in_edges.len(), 1, "one in-edge before the update");

        // `node update --body` + `--property a=1` MERGEs {a:1,b:2} and keeps
        // every incident edge identical.
        with_cwd(&wt, || {
            cmd_node(&av(&[
                "update",
                "requirements",
                "requirement",
                "r1",
                "--body",
                "updated",
                "--property",
                "a=1",
            ]))
        })
        .unwrap();
        let after =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(after.body, "updated");
        assert_eq!(after.properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(
            after.properties.get("b").map(String::as_str),
            Some("2"),
            "omitting --unset-property must never drop a key"
        );
        assert_eq!(after.out, before.out, "out-edges survive a node update");
        assert_eq!(
            after.in_edges, before.in_edges,
            "in-edges survive a node update"
        );

        // Only the explicit `--unset-property b` drops b.
        with_cwd(&wt, || {
            cmd_node(&av(&[
                "update",
                "requirements",
                "requirement",
                "r1",
                "--unset-property",
                "b",
            ]))
        })
        .unwrap();
        let unset =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(unset.properties.get("b"), None, "b was explicitly unset");
        assert_eq!(unset.properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(unset.out, before.out);
        assert_eq!(unset.in_edges, before.in_edges);

        // A duplicate `(kind, from, to)` is refused, naming update/rm; no half
        // is duplicated.
        let err = with_cwd(&wt, || cmd_edge(&edge).unwrap_err());
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg edge update"), "{msg}");
        assert!(msg.contains("apg edge rm"), "{msg}");
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        assert_eq!(r1.out.len(), 1, "the duplicate must not add an out-half");
        assert_eq!(
            r2.in_edges.len(),
            1,
            "the duplicate must not add an in-half"
        );

        // `edge update --property` MERGEs the same map onto BOTH halves.
        with_cwd(&wt, || {
            cmd_edge(&av(&[
                "update",
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "a=1",
                "--property",
                "b=2",
            ]))
        })
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        let expect = BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        assert_eq!(r1.out[0].properties, expect, "source out-half map");
        assert_eq!(
            r2.in_edges[0].properties, expect,
            "target in-half carries the identical map"
        );

        // The explicit unset reaches both halves too.
        with_cwd(&wt, || {
            cmd_edge(&av(&[
                "update",
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--unset-property",
                "b",
            ]))
        })
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        let expect = BTreeMap::from([("a".to_string(), "1".to_string())]);
        assert_eq!(r1.out[0].properties, expect);
        assert_eq!(r2.in_edges[0].properties, expect);

        testutil::remove(&repo);
    }

    /// Phase-03 task-15: with a live session, a parallel burst of N separate
    /// routed `apg edge add` processes is applied by the ONE coordinator in
    /// receive order — zero failures and the shared hub carries exactly N
    /// out-edges (no lost update), and the DB equals the serial application.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn live_session_applies_routed_mutations_in_receive_order_as_single_writer() {
        const N: usize = 10;
        let (wt_apg, repo, wt) = mutation_fixture("session-order");
        setup_hub_and_leaves(&wt, N);
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        let (failed, by_lock) = run_edge_burst(&wt, &home, N);
        assert_eq!(
            failed, 0,
            "routed burst lost {failed}/{N} mutations ({by_lock:?})"
        );
        assert_eq!(hub_out_edges(&wt_apg), N, "the single writer lost an edge");

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Store == serial application, now visible in the DB (session released).
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let q = db
        .q("MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:DependsOn]->(b) RETURN count(*)")
        .unwrap();
        assert_eq!(q.lines().last().map(str::trim), Some("10"), "{q}");
        drop(db);
        testutil::remove(&repo);
    }

    /// Phase-01 task-17: the session amortizes ONE DB open across N routed
    /// durable mutations (the observable open counter is materially fewer than
    /// N), every mutation is visible to a separate routed reader as soon as it
    /// returns (the session projects it into the live `db.lbug` at admission —
    /// there is no end-of-session flush), and the whole run stays BUFFERED —
    /// no node file, no commit — until `apg session save`, the single
    /// durability point. After save the buffered set is durable (every node
    /// file written in exactly one commit) and the buffer is cleared (a second
    /// save is a no-op); the session stays live throughout and ends cleanly.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn live_session_amortizes_the_db_open_and_keeps_every_mutation_visible() {
        const N: usize = 6;
        let (wt_apg, repo, wt) = mutation_fixture("session-amortize");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // Baseline: the buffered run below must not move git until save.
        let before_commits = testutil::commit_count(&wt);
        let before_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();

        for i in 0..N {
            let name = format!("amort-{i}");
            let add = testutil::ApgCommand::new(&[
                "node",
                "add",
                "requirements",
                "requirement",
                name.as_str(),
            ])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
            assert!(
                add.status.success(),
                "mutation {i}: {}",
                String::from_utf8_lossy(&add.stderr)
            );

            // A SEPARATE routed reader sees the mutation as soon as it returns
            // (projected at admission — no end-of-session flush).
            let query = format!(
                "MATCH (n:Requirement {{fqn: 'requirements.requirement.{name}'}}) RETURN count(n)"
            );
            let q = testutil::spawn_apg(&["query", query.as_str()], &wt);
            assert!(
                q.status.success(),
                "routed read {i}: {}",
                String::from_utf8_lossy(&q.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&q.stdout)
                    .lines()
                    .last()
                    .map(str::trim),
                Some("1"),
                "mutation {i} must be visible immediately (projected at admission)"
            );
        }

        // Buffered, not durable: no node file was written and git is unmoved.
        for i in 0..N {
            let name = format!("amort-{i}");
            assert!(
                !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", &name)
                    .exists(),
                "a buffered mutation must not write `{name}` before save"
            );
        }
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered mutation must not create a commit before save"
        );
        assert_eq!(
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string(),
            before_head,
            "git history must stay at the last saved state until save"
        );

        // The single durability point: one save flushes the whole buffer.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved"
        );
        for i in 0..N {
            let name = format!("amort-{i}");
            assert!(
                layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", &name).exists(),
                "save must write the `{name}` node file"
            );
        }
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the whole buffered set must land in exactly one commit"
        );

        // The buffer is cleared: a second save over it makes no new commit.
        let resave = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            resave.status.success(),
            "{}",
            String::from_utf8_lossy(&resave.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "a save over the cleared buffer must make no commit"
        );

        // The session stayed live through save …
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // … and ends cleanly.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // ONE amortized DB open across N mutations: the observable open marker
        // fires materially fewer than N times.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let opens = stderr.matches(apg::session::DB_OPEN_MARKER).count();
        assert!(opens >= 1, "the session must open the DB: {stderr}");
        assert!(
            opens < N,
            "the session must amortize the DB open: {opens} opens for {N} mutations\n{stderr}"
        );
        testutil::remove(&repo);
    }

    /// Phase-03 task-18: routing is transparent — the same command works with
    /// and without a live session, producing the identical message; the CLI
    /// surface is unchanged.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn routing_is_transparent_with_and_without_a_live_session() {
        let (_wt_apg, repo, wt) = mutation_fixture("session-transparent");
        let home = repo.root.join("home");
        // (a) No session: the serialized direct path.
        let direct =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "direct-1"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            direct.status.success(),
            "{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&direct.stdout).trim(),
            "Added node requirements.requirement.direct-1"
        );

        // (b) Live session: the same command is routed with the same output.
        let session = testutil::start_session_process(&wt, &home);
        let routed =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "routed-1"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            routed.status.success(),
            "{}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout).trim(),
            "Added node requirements.requirement.routed-1"
        );
        let edge = testutil::ApgCommand::new(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.routed-1",
            "requirements.requirement.direct-1",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            edge.status.success(),
            "{}",
            String::from_utf8_lossy(&edge.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&edge.stdout).trim(),
            "Added edge depends-on requirements.requirement.routed-1 -> requirements.requirement.direct-1"
        );

        // (c) The documented surface is unchanged.
        let help = testutil::spawn_apg(&["--help"], &wt);
        let text = String::from_utf8_lossy(&help.stdout);
        assert!(text.contains("apg node <sub>"), "{text}");
        assert!(text.contains("apg edge <sub>"), "{text}");

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// Phase-03 task-20: a forwarded mutation carries a client id and is applied
    /// AT MOST ONCE — replaying the same id returns the cached reply with no
    /// second commit — and a forward that cannot reach the coordinator ERRORS
    /// rather than silently falling back to the direct path.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn forwarded_mutations_apply_at_most_once_and_never_fall_back() {
        let (wt_apg, repo, wt) = mutation_fixture("session-at-most-once");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        let args = av(&["add", "requirements", "requirement", "once"]);
        let first = session_forward_node(&wt_apg, "dup-1", &args);
        assert_eq!(first, "Added node requirements.requirement.once");
        let before = testutil::commit_count(&wt);

        // Replay the SAME client id: cached reply, no re-apply.
        let replay = session_forward_node(&wt_apg, "dup-1", &args);
        assert_eq!(replay, first, "a replayed id must return the cached reply");
        assert_eq!(
            testutil::commit_count(&wt),
            before,
            "a replay must not create a second commit"
        );
        assert_eq!(
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "once")
                .unwrap()
                .name,
            "once"
        );

        // End the session. A forward now ERRORS — no direct-path fallback — and
        // nothing lands locally.
        let _ = apg::session::Coordinator::signal_end(&wt_apg);
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ghost = av(&["add", "requirements", "requirement", "ghost"]);
        let err = apg::session::Coordinator::forward_mutation_with_id(
            &wt_apg,
            "after-end",
            "node",
            &ghost,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("session forward failed"),
            "{err:#}"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "ghost").exists(),
            "a failed forward must not fall back to the direct path"
        );

        testutil::remove(&repo);
    }

    /// Phase-03 task-21: session lifecycle exclusivity — one session per
    /// worktree DB; a second start refuses; `apg scan` and `apg project merge`
    /// refuse while a session is live; routed reads keep working and a
    /// non-routing direct DB open is out of contract.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_lifecycle_is_exclusive_with_scan_and_merge() {
        let (wt_apg, repo, wt) = mutation_fixture("session-exclusive");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // (a) One session per DB: a second start refuses.
        let second = testutil::ApgCommand::new(&["session", "start"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!second.status.success(), "a second session must refuse");
        assert!(
            String::from_utf8_lossy(&second.stderr).contains("already live"),
            "{}",
            String::from_utf8_lossy(&second.stderr)
        );

        // (b) `apg scan` refuses while the session owns db.lbug.
        let scan = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!scan.status.success(), "scan must refuse a live session");
        assert!(
            String::from_utf8_lossy(&scan.stderr).contains("live `apg session`"),
            "{}",
            String::from_utf8_lossy(&scan.stderr)
        );

        // (c) `apg project merge` refuses while the session owns the branch DB.
        let merge = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!merge.status.success(), "merge must refuse a live session");
        assert!(
            String::from_utf8_lossy(&merge.stderr).contains("live `apg session`"),
            "{}",
            String::from_utf8_lossy(&merge.stderr)
        );

        // (d) Routed reads keep working; a non-routing direct open is OUT of
        // contract (lbug errors while the session holds the DB).
        let q = testutil::spawn_apg(&["query", "MATCH (n:Module) RETURN count(n)"], &wt);
        assert!(
            q.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert!(
            ArtifactDb::open(&wt_apg).is_err(),
            "a non-routing direct DB open must fail while the session holds it"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// Phase-05 task-13 (e2e): genuine cross-process read-your-writes. A spawned
    /// `apg node add requirements requirement foo` returns, then a NEW `apg
    /// query` process (a separate binary) resolves `foo` — no re-scan, no
    /// explicit flush, no session-end step. An in-process re-open does not prove
    /// it.
    ///
    /// Both variants are asserted explicitly:
    /// (a) **no live session** ⇒ the new query process opens `db.lbug` directly
    /// (the direct-path read-your-writes);
    /// (b) **live session** ⇒ it routes through the socket
    /// (read-access-during-session).
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn read_your_writes_cross_process_direct_and_session() {
        let (_wt_apg, repo, wt) = mutation_fixture("read-your-writes");
        let home = repo.root.join("home");

        // (a) No live session: the direct path projects write-through, and a
        // NEW query process reads it from a direct db.lbug open.
        let add = testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "foo"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert!(
            !apg::session::live_session(&wt.join(specs::LAYOUT)),
            "variant (a) must run with no live session"
        );
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.foo'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q.status.success(),
            "direct query: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a NEW process must read the mutation without a scan/flush"
        );

        // (b) Live session: the same add is routed, and a NEW query process
        // routes through the socket to the session-held DB.
        let session = testutil::start_session_process(&wt, &home);
        assert!(apg::session::live_session(&wt.join(specs::LAYOUT)));
        let add2 =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "bar"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            add2.status.success(),
            "{}",
            String::from_utf8_lossy(&add2.stderr)
        );
        let q2 = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.bar'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q2.status.success(),
            "routed query: {}",
            String::from_utf8_lossy(&q2.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q2.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a NEW process must read the routed mutation before session end"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// Phase-04 task-1 (acceptance): the cross-process parallel node/edge burst
    /// of N SEPARATE `apg` binaries completes with ZERO lock errors on every
    /// named lock — the lbug `apg/.trans/db.lbug` read-write open, the
    /// `apg/.trans/specs.lock` flock, and git's `.git/index.lock` — and the
    /// durable store equals the serial application.
    ///
    /// Cross-process by construction (`ApgCommand`/`spawn_apg`): `SPEC_LOCK`
    /// is a process-lifetime `OnceLock` flock, so an in-process thread burst
    /// would pass with the lock absent and false-green exactly the race this
    /// test exists to catch.
    ///
    /// Stage A (DB present) exercises all three locks; stage B (DB removed)
    /// isolates the `.git/index.lock` + specs.lock + the shared hub-file
    /// read-modify-write that a DB-only fix cannot reach; stage C drives the
    /// node path's own existence-check + write + commit burst on stage A's
    /// DB-present fixture (its `accept-` names do not collide with
    /// `hub`/`leaf-*`), so only two repos/scans are built.
    ///
    /// N=4 keeps genuine cross-process contention while halving the child
    /// fan-out (each child is a full 33 MB debug `apg`, and the whole e2e
    /// tier runs hundreds of them concurrently).
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn acceptance_cross_process_burst_has_zero_lock_errors_and_serial_store() {
        const N: usize = 4;
        const NAMED_LOCKS: [&str; 4] = [
            "lbug apg/.trans/db.lbug",
            "git .git/index.lock",
            "specs.lock flock",
            "node-file RMW (pairing mismatch)",
        ];

        // Stage A — the real project state (DB present).
        let (wt_apg, repo, wt) = mutation_fixture("accept-burst-db");
        setup_hub_and_leaves(&wt, N);
        let hist_a = run_edge_burst_attributed(&wt, &repo.root.join("home"), N);
        eprintln!("acceptance stage A per-lock: {hist_a:?}");
        assert_eq!(
            hist_a.get("ok"),
            Some(&N),
            "stage A must complete every mutation: {hist_a:?}"
        );
        for lock in NAMED_LOCKS {
            assert_eq!(hist_a.get(lock), None, "stage A hit {lock}: {hist_a:?}");
        }
        assert_eq!(
            hub_out_edges(&wt_apg),
            N,
            "stage A: the shared hub lost edges"
        );
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            let out = db
            .q("MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:DependsOn]->(b) RETURN count(*)")
            .unwrap();
            let expected = N.to_string();
            assert_eq!(
                out.lines().last().map(str::trim),
                Some(expected.as_str()),
                "stage A DB must equal the serial application: {out}"
            );
        }

        // Stage B — DB absent: isolates git `.git/index.lock`, the specs.lock
        // flock, and the shared hub-file read-modify-write.
        let (wt_apg_b, repo_b, wt_b) = mutation_fixture("accept-burst-nodb");
        std::fs::remove_file(wt_apg_b.join(specs::TRANS).join("db.lbug")).unwrap();
        setup_hub_and_leaves(&wt_b, N);
        let hist_b = run_edge_burst_attributed(&wt_b, &repo_b.root.join("home"), N);
        eprintln!("acceptance stage B per-lock: {hist_b:?}");
        assert_eq!(
            hist_b.get("ok"),
            Some(&N),
            "stage B must complete every mutation: {hist_b:?}"
        );
        for lock in NAMED_LOCKS {
            assert_eq!(hist_b.get(lock), None, "stage B hit {lock}: {hist_b:?}");
        }
        assert_eq!(
            hub_out_edges(&wt_apg_b),
            N,
            "stage B: the shared hub lost edges"
        );

        // Stage C — N separate `apg node add` processes: the node path's own
        // burst (existence check + write + commit) behind the same entry flock.
        // Reuses stage A's DB-present fixture; only the `accept-` names are
        // counted, so `hub`/`leaf-*` do not interfere.
        let home_c = repo.root.join("home");
        std::fs::create_dir_all(&home_c).unwrap();
        let mut hist_c: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut kids = Vec::with_capacity(N);
        for i in 0..N {
            let name = format!("accept-{i}");
            kids.push(
                testutil::ApgCommand::new(&[
                    "node",
                    "add",
                    "requirements",
                    "requirement",
                    name.as_str(),
                ])
                .cwd(&wt)
                .env("HOME", home_c.to_str().unwrap())
                .spawn(),
            );
        }
        for child in kids {
            let out = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            let key = if out.status.success() {
                "ok"
            } else {
                let lock = classify_lock(&stderr);
                eprintln!("accept-node-burst FAILED ({lock}): {stderr}");
                lock
            };
            *hist_c.entry(key).or_default() += 1;
        }
        eprintln!("acceptance stage C per-lock: {hist_c:?}");
        assert_eq!(
            hist_c.get("ok"),
            Some(&N),
            "stage C must complete every add: {hist_c:?}"
        );
        for lock in NAMED_LOCKS {
            assert_eq!(hist_c.get(lock), None, "stage C hit {lock}: {hist_c:?}");
        }
        let stored = layers::read_existing_nodes(&wt_apg)
            .unwrap()
            .iter()
            .filter(|n| {
                n.layer == "requirements"
                    && n.node_type == "requirement"
                    && n.name.starts_with("accept-")
            })
            .count();
        assert_eq!(stored, N, "stage C store must equal the serial adds");

        testutil::remove(&repo);
        testutil::remove(&repo_b);
    }

    /// Phase-02 task-7 (E2E): `apg node rm` on a node reviewed by an
    /// outstanding item WARNS (stderr) naming each open/actioned Feedback and
    /// its status, then removes the node anyway. The record set is
    /// authoritative: the Feedback records and their reviewed-target references
    /// survive in the requirements mirror, so `apg review list` still reports
    /// both items marked `(removed target)`. A node whose items are all
    /// `resolved`, and a node with no items, remove silently.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_rm_warns_on_outstanding_feedback_and_keeps_the_record() {
        let (_wt_apg, repo, wt) = mutation_fixture("node-rm-warn");

        // Run the real CLI, asserting success (dumping stderr on failure).
        let run = |args: &[&str]| -> std::process::Output {
            let out = spawn_apg(args, &wt);
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        // --- Outstanding feedback (one open, one actioned) → warn + proceed ---
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "gone",
            "--body",
            "x",
        ]);
        run(&[
            "review",
            "add",
            "requirements.requirement.gone",
            "--body",
            "open item",
            "--project",
            "foo",
        ]);
        run(&[
            "review",
            "add",
            "requirements.requirement.gone",
            "--body",
            "actioned item",
            "--project",
            "foo",
        ]);
        run(&["review", "action", "foo/feedback-2", "--fix"]);

        let removed = run(&["node", "rm", "requirements", "requirement", "gone"]);
        let stderr = String::from_utf8_lossy(&removed.stderr);
        assert!(
            stderr.contains("apg: warning: removing"),
            "the removal must warn about outstanding feedback: {stderr}"
        );
        assert!(
            stderr.contains("requirements.requirement.gone"),
            "the warning must name the removed FQN: {stderr}"
        );
        assert!(
            stderr.contains("foo/feedback-1 (open)"),
            "the warning must name the open item and its status: {stderr}"
        );
        assert!(
            stderr.contains("foo/feedback-2 (actioned)"),
            "the warning must name the actioned item and its status: {stderr}"
        );

        // The records survive the target's removal and still list, marked.
        let listed = run(&["review", "list"]);
        let out = String::from_utf8(listed.stdout).unwrap();
        assert!(
            out.lines()
                .any(|l| l == "foo/feedback-1,open,,requirements.requirement.gone (removed target)"),
            "the open item must survive and list against its removed target: {out}"
        );
        assert!(
            out.lines().any(|l| l
                == "foo/feedback-2,actioned,fixed,requirements.requirement.gone (removed target)"),
            "the actioned item must survive and list against its removed target: {out}"
        );

        // --- All resolved → no warning, removal still succeeds ---
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "calm",
            "--body",
            "x",
        ]);
        run(&[
            "review",
            "add",
            "requirements.requirement.calm",
            "--body",
            "resolve me",
            "--project",
            "foo",
        ]);
        run(&["review", "resolve", "foo/feedback-3"]);
        let removed = run(&["node", "rm", "requirements", "requirement", "calm"]);
        assert!(
            !String::from_utf8_lossy(&removed.stderr).contains("apg: warning: removing"),
            "all-resolved feedback must not warn"
        );

        // --- No feedback → no warning, removal still succeeds ---
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "bare",
            "--body",
            "x",
        ]);
        let removed = run(&["node", "rm", "requirements", "requirement", "bare"]);
        assert!(
            !String::from_utf8_lossy(&removed.stderr).contains("apg: warning: removing"),
            "an unreviewed node must not warn"
        );

        testutil::remove(&repo);
    }

    /// Phase-01 task-12: the live session's write-back buffer COMPOSES across
    /// mutations — a later routed mutation is built over the cumulative buffered
    /// state, not just the on-disk store. With a live session, an add followed
    /// by an UPDATE of the just-added node (over its buffered base) and then an
    /// edge whose target was only ever buffered must all succeed; a routed read
    /// observes the cumulative result (the updated body and the A→B edge).
    ///
    /// Throughout, `apg/layers/**` and the git history stay at the LAST SAVED
    /// state: after an initial save establishes the baseline, none of the later
    /// buffered mutations writes a node file or creates a commit — that is the
    /// single durability point of `apg session save`.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn write_back_buffer_composes_across_mutations() {
        let (wt_apg, repo, wt) = mutation_fixture("write-back-compose");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // Establish the LAST SAVED state: one buffered node flushed by `save`,
        // so the baseline the later mutations must NOT disturb is non-empty.
        let saved = testutil::ApgCommand::new(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "saved",
            "--body",
            "baseline",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            saved.status.success(),
            "{}",
            String::from_utf8_lossy(&saved.stderr)
        );
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let saved_commits = testutil::commit_count(&wt);
        let saved_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        let saved_store = layers::read_existing_nodes(&wt_apg).unwrap();
        assert!(
            saved_store.iter().any(|n| n.name == "saved"),
            "the baseline save must have written the `saved` node file"
        );

        // A routed sequence where the LATER mutations MUST compose over earlier
        // buffered ones: add A, UPDATE A over its buffered base, add B, then an
        // edge A→B whose target exists only in the buffer.
        let run = |args: &[&str], expected: &str| {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                expected,
                "apg {args:?}"
            );
        };
        run(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "a",
                "--body",
                "first",
            ],
            "Added node requirements.requirement.a",
        );
        // An update that does NOT compose over the buffered add would refuse
        // "node `requirements.requirement.a` does not exist".
        run(
            &[
                "node",
                "update",
                "requirements",
                "requirement",
                "a",
                "--body",
                "second",
            ],
            "Updated node requirements.requirement.a",
        );
        run(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "b",
                "--body",
                "third",
            ],
            "Added node requirements.requirement.b",
        );
        // The target `b` was only ever buffered — the edge composes.
        run(
            &[
                "edge",
                "add",
                "depends-on",
                "requirements.requirement.a",
                "requirements.requirement.b",
            ],
            "Added edge depends-on requirements.requirement.a -> requirements.requirement.b",
        );

        // A routed read observes the CUMULATIVE buffered state: A carries the
        // updated body and the A→B edge exists.
        let body = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.a'}) RETURN n.body",
            ],
            &wt,
        );
        assert!(
            body.status.success(),
            "routed body read: {}",
            String::from_utf8_lossy(&body.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&body.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("second"),
            "the routed read must see the update composed over the buffered add"
        );
        let edge = testutil::spawn_apg(
            &[
                "query",
                "MATCH (:Requirement {fqn: 'requirements.requirement.a'})-[:DependsOn]->(b) RETURN count(*)",
            ],
            &wt,
        );
        assert!(
            edge.status.success(),
            "routed edge read: {}",
            String::from_utf8_lossy(&edge.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&edge.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the routed read must see the edge whose target was only buffered"
        );

        // `apg/layers/**` and git stay at the LAST SAVED state: no node file for
        // the buffered A/B, the saved store byte-identical, no new commit.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "a").exists(),
            "a buffered add must not write a node file before save"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "b").exists(),
            "a buffered add must not write a node file before save"
        );
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            saved_store,
            "the durable store must stay at the last saved state until save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits,
            "a buffered mutation must not create a commit before save"
        );
        assert_eq!(
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string(),
            saved_head,
            "git history must stay at the last saved state until save"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// Phase-01 task-13: `apg session save` is the single durability point for
    /// the whole buffered set — it atomically writes every buffered node file
    /// under `apg/layers/**` with exactly ONE git commit, then clears the buffer
    /// (a second save over the now-clean buffer writes nothing and makes no
    /// commit). The session stays live throughout and ends cleanly afterwards.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn save_flushes_the_buffer_in_one_commit() {
        let (wt_apg, repo, wt) = mutation_fixture("save-flush");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // A run of N durable mutations admitted through the live session: three
        // nodes plus an edge whose endpoints are both buffered (neither is on
        // disk yet).
        let run = |args: &[&str], expected: &str| {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                expected,
                "apg {args:?}"
            );
        };
        run(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "alpha",
                "--body",
                "first",
            ],
            "Added node requirements.requirement.alpha",
        );
        run(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "beta",
                "--body",
                "second",
            ],
            "Added node requirements.requirement.beta",
        );
        run(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "gamma",
                "--body",
                "third",
            ],
            "Added node requirements.requirement.gamma",
        );
        run(
            &[
                "edge",
                "add",
                "depends-on",
                "requirements.requirement.alpha",
                "requirements.requirement.beta",
            ],
            "Added edge depends-on requirements.requirement.alpha -> requirements.requirement.beta",
        );

        // Before save nothing is durable: no node file, no commit.
        let before_commits = testutil::commit_count(&wt);
        let before_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        for name in ["alpha", "beta", "gamma"] {
            assert!(
                !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", name).exists(),
                "a buffered add must not write `{name}` before save"
            );
        }

        // The single durability point: one save flushes the whole buffer.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved"
        );

        // Every buffered node file now exists on disk under apg/layers/** …
        let store = layers::read_existing_nodes(&wt_apg).unwrap();
        for name in ["alpha", "beta", "gamma"] {
            assert!(
                layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", name).exists(),
                "save must write the `{name}` node file"
            );
            assert!(
                store.iter().any(|n| n.name.as_str() == name),
                "the durable store must contain `{name}` after save"
            );
        }
        // … including BOTH halves of the buffered edge.
        let alpha =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "alpha").unwrap();
        let beta =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "beta").unwrap();
        assert_eq!(
            alpha
                .out
                .iter()
                .filter(|e| e.kind == "depends-on"
                    && e.target.as_str() == "requirements.requirement.beta")
                .count(),
            1,
            "save must write the buffered out-edge"
        );
        assert_eq!(
            beta.in_edges
                .iter()
                .filter(|e| e.kind == "depends-on"
                    && e.source.as_str() == "requirements.requirement.alpha")
                .count(),
            1,
            "save must write the buffered edge's matching in-half"
        );

        // Exactly ONE new commit landed, and it moved HEAD.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the whole buffered set must land in exactly one commit"
        );
        let after_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        assert_ne!(after_head, before_head, "save must create a commit");
        assert_eq!(
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .parent_count(),
            1,
            "the save commit is a normal single-parent commit"
        );

        // The buffer is cleared: a second save over the clean buffer is a no-op
        // — no new commit, HEAD unchanged.
        let resave = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            resave.status.success(),
            "{}",
            String::from_utf8_lossy(&resave.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "a save over the cleared buffer must make no commit"
        );
        assert_eq!(
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string(),
            after_head,
            "a save over the cleared buffer must not move HEAD"
        );

        // The session stayed live through save …
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // … and ends cleanly.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// Phase-01 task-14: `apg session save` over a CLEAN buffer — a live session
    /// that has admitted NO durable mutation — is a pure no-op. It writes no
    /// node file, creates no commit, leaves HEAD and the whole on-disk
    /// `apg/layers/**` tree byte-identical, reports the clean-buffer outcome
    /// (the coordinator's `Session saved: no pending changes`), and leaves the
    /// session live. (The dirty counterpart — one save flushes the whole buffer
    /// in exactly one commit — is `save_flushes_the_buffer_in_one_commit`.)
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn save_on_a_clean_buffer_is_a_noop() {
        let (wt_apg, repo, wt) = mutation_fixture("clean-save-noop");
        let home = repo.root.join("home");

        // A pre-existing durable node file, committed through the direct path:
        // `apg/layers/**` is non-empty before the session starts, so "nothing
        // written or changed" is a real claim rather than a vacuous one.
        layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "existing")],
            &[],
        )
        .unwrap();

        // A live session that has admitted no mutation: the write-back buffer
        // is empty by construction.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // Baseline: commit count, HEAD, and the whole on-disk node-file tree.
        let before_commits = testutil::commit_count(&wt);
        let before_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        let snapshot = |apg_root: &Path| -> BTreeMap<String, Vec<u8>> {
            let mut out = BTreeMap::new();
            let mut stack = vec![apg_root.join(layers::LAYERS_DIR)];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        let rel = path
                            .strip_prefix(apg_root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        out.insert(rel, std::fs::read(&path).unwrap());
                    }
                }
            }
            out
        };
        let before_layers = snapshot(&wt_apg);
        assert!(
            before_layers.keys().any(|k| k.ends_with("existing.json")),
            "the seeded node file must be on disk before the clean save: {before_layers:?}"
        );

        // The save over the clean buffer.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&save.stdout).trim(),
            "Session saved",
            "the save's client reply"
        );

        // No new commit, HEAD unmoved.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a save over the clean buffer must not create a commit"
        );
        assert_eq!(
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string(),
            before_head,
            "a save over the clean buffer must not move HEAD"
        );

        // No node file written or changed: the whole tree is byte-identical.
        assert_eq!(
            snapshot(&wt_apg),
            before_layers,
            "a save over the clean buffer must not write or change any apg/layers file"
        );

        // The session stays live through the no-op save.
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // End cleanly; the coordinator's captured output reports the clean
        // buffer — the no-op/clean-buffer outcome.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let server_out = String::from_utf8_lossy(&out.stdout);
        assert!(
            server_out.contains("Session saved: no pending changes"),
            "the clean-buffer save must report the no-op outcome: {server_out}"
        );

        testutil::remove(&repo);
    }
}
