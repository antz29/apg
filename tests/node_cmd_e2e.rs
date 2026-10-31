mod common;

use apg::artifacts::ArtifactDb;
use apg::layers::{self, InEdge, Layer, NodeFile, OutEdge, fqn};
use apg::node_cmd::*;
use apg::schema::Record;
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
///
/// A `requirements.stakeholder.owner` requiring `hub` roots the hub, and (via
/// the burst's `contains hub -> leaf-i` edges) every leaf — so the seeded store
/// is whole-model-valid (SPEC §8.1).
fn setup_hub_and_leaves(wt: &Path, n: usize) {
    let mut owner = node("requirements", "stakeholder", "owner");
    owner.out = vec![testutil::out_edge(
        "requires",
        "requirements.requirement.hub",
    )];
    let mut hub = node("requirements", "requirement", "hub");
    hub.in_edges = vec![
        testutil::in_edge("requires", "requirements.stakeholder.owner"),
        testutil::in_edge("driven-by", "domain.service.svc"),
    ];

    // Whole-model completion (SPEC §8.1/§3.4): one Service driven-by the hub and
    // every leaf, contained by a leaf Group and realised by a Component that
    // is-role's a Role (applies-to=component) — so the seeded store is
    // whole-model-valid at the save boundary.
    let mut svc = node("domain", "service", "svc");
    svc.out.push(testutil::out_edge(
        "driven-by",
        "requirements.requirement.hub",
    ));
    svc.in_edges
        .push(testutil::in_edge("contains", "domain.group.grp"));
    svc.in_edges
        .push(testutil::in_edge("realises", "solution.component.cmp"));

    let mut grp = node("domain", "group", "grp");
    grp.out
        .push(testutil::out_edge("contains", "domain.service.svc"));

    let mut cmp = node("solution", "component", "cmp");
    cmp.out
        .push(testutil::out_edge("realises", "domain.service.svc"));
    cmp.out
        .push(testutil::out_edge("is-role", "solution.role.rl"));

    let mut rl = node("solution", "role", "rl");
    rl.properties
        .insert("applies-to".to_string(), "component".to_string());
    rl.in_edges
        .push(testutil::in_edge("is-role", "solution.component.cmp"));

    let mut nodes = vec![owner, hub, grp, cmp, rl];
    for i in 0..n {
        let leaf_fqn = format!("requirements.requirement.leaf-{i}");
        let mut leaf = node("requirements", "requirement", &format!("leaf-{i}"));
        leaf.in_edges
            .push(testutil::in_edge("driven-by", "domain.service.svc"));
        svc.out.push(testutil::out_edge("driven-by", &leaf_fqn));
        nodes.push(leaf);
    }
    nodes.push(svc);
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
            "contains",
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
    apg::session::Coordinator::forward_mutation_with_id(apg_root, client_id, "node", args)
        .unwrap()
        .output
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
            "contains",
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

/// Asserts a mutation refusal is the unified project-context one: the
/// `mutation refused — ` prefix, naming branch `main` (the repo's default
/// branch) and the `apg project start` fix, and NEVER any of `preempted` — the
/// downstream errors the positional guard must precede (the live-session
/// requirement, an absent node/edge).
fn assert_refused_on_main(out: &std::process::Output, argv: &[&str], preempted: &[&str]) {
    assert!(!out.status.success(), "{argv:?} must refuse on main");
    let msg = String::from_utf8_lossy(&out.stderr);
    // The CLI prefixes `apg: `; the message itself begins with the unified
    // refusal prefix.
    let refusal = msg.strip_prefix("apg: ").unwrap_or(&msg);
    assert!(
        refusal.starts_with("mutation refused — "),
        "{argv:?}: {msg}"
    );
    assert!(msg.contains("branch `main`"), "{argv:?}: {msg}");
    assert!(msg.contains("apg project start"), "{argv:?}: {msg}");
    for preempted in preempted {
        assert!(
            !msg.contains(preempted),
            "{argv:?} must refuse context, never `{preempted}`: {msg}"
        );
    }
}

/// Asserts a refused mutation left no durable trace: no file under
/// `apg/layers/` and no commit on `checkout`.
fn assert_no_durable_trace(apg_root: &Path, checkout: &Path, before_commits: usize) {
    let layers_dir = apg_root.join(layers::LAYERS_DIR);
    assert!(
        !layers_dir.exists() || std::fs::read_dir(&layers_dir).unwrap().next().is_none(),
        "a refused mutation must not write apg/layers"
    );
    assert_eq!(
        testutil::commit_count(checkout),
        before_commits,
        "a refused mutation must not create a commit"
    );
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
            kind: "contains".to_string(),
            target: "requirements.requirement.r2".to_string(),
            properties: BTreeMap::new(),
        });
        let mut dst =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        dst.in_edges.push(InEdge {
            kind: "contains".to_string(),
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
            "contains",
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

        // `apg node add global constraint timer` — admitted into the write-back
        // buffer, so nothing is durable yet. A lone `global.constraint` is an
        // unconstrained node type (no cardinality minimums), so the saved model
        // is whole-model-valid (SPEC §8.1).
        let add = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "timer"])
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
            "Added node global.constraint.timer"
        );

        // The file name is the identity, and the add BUFFERS: no node file
        // appears before save (the durability point below).
        let path = wt_apg
            .join(layers::LAYERS_DIR)
            .join("global")
            .join("constraint")
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
        let back = layers::read_node_file(&wt_apg, Layer::Global, "constraint", "timer").unwrap();
        assert_eq!(back.name, "timer");
        assert_eq!(
            fqn(Layer::Global, "constraint", "timer"),
            "global.constraint.timer"
        );

        // Visible in the branch DB: a routed read sees the saved node while
        // the session is still live.
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.timer'}) RETURN count(n)",
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
                .get_path(Path::new("apg/layers/global/constraint/timer.json"))
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
        assert!(db.has_node("global.constraint.timer"));
        drop(db);

        testutil::remove(&repo);
    }

    /// Regression (`session-serve-crash`): a client that disconnects before its
    /// reply is delivered must NOT kill the live session. The serve loop
    /// confines a broken-pipe reply write to that one connection, so a liveness
    /// probe that timed out — or any client killed mid-request — can never
    /// orphan a healthy coordinator behind a stale socket (which would then
    /// force a full rebuild on the next `start`/`save`/`end`).
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_client_that_disconnects_before_reading_its_reply_does_not_kill_the_session() {
        use std::io::Write as _;

        let (wt_apg, repo, wt) = mutation_fixture("session-client-disconnect");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // A client that sends a request and then drops WITHOUT reading the
        // reply: the coordinator's reply write hits a broken pipe. Repeated so
        // the disconnect lands both before and after the server reads the
        // request.
        let socket = apg::session::socket_path(&wt_apg);
        for _ in 0..5 {
            let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            stream.write_all(b"\"Ping\"\n").unwrap();
            // drop(stream) with the Pong unread.
        }

        // The coordinator must still be alive and serving.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            apg::session::live_session(&wt_apg),
            "a client that disconnected before reading its reply must not kill the session"
        );

        // And it still routes a real durable mutation.
        let add = testutil::spawn_apg(&["node", "add", "global", "constraint", "survivor"], &wt);
        assert!(
            add.status.success(),
            "a routed mutation after the disconnect must succeed: {}",
            String::from_utf8_lossy(&add.stderr)
        );

        // Flush the buffered add (a clean `end` refuses a dirty buffer), then end.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

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

        // `apg node add` × 2 and `apg edge add requires
        // requirements.stakeholder.owner requirements.requirement.r1`, each routed
        // through the live session and admitted into the write-back buffer. A
        // `requires` edge from a Stakeholder to a Requirement is the minimal
        // whole-model-valid authored edge (SPEC §8.1): it roots the Requirement
        // and satisfies the Stakeholder's `requires` minimum.
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
            &["node", "add", "requirements", "stakeholder", "owner"],
            "Added node requirements.stakeholder.owner",
        );
        run(
            &["node", "add", "requirements", "requirement", "r1"],
            "Added node requirements.requirement.r1",
        );
        run(
            &[
                "edge",
                "add",
                "requires",
                "requirements.stakeholder.owner",
                "requirements.requirement.r1",
            ],
            "Added edge requires requirements.stakeholder.owner -> requirements.requirement.r1",
        );
        // Whole-model completion (SPEC §8.1/§3.4): the Requirement `r1` must be
        // `driven-by` a Service at save, and that Service needs the domain/
        // solution context that satisfies the other cardinalities.
        run(
            &["node", "add", "domain", "service", "svc"],
            "Added node domain.service.svc",
        );
        run(
            &["node", "add", "domain", "group", "grp"],
            "Added node domain.group.grp",
        );
        run(
            &["node", "add", "solution", "component", "cmp"],
            "Added node solution.component.cmp",
        );
        run(
            &[
                "node",
                "add",
                "solution",
                "role",
                "rl",
                "--property",
                "applies-to=component",
            ],
            "Added node solution.role.rl",
        );
        run(
            &[
                "edge",
                "add",
                "driven-by",
                "domain.service.svc",
                "requirements.requirement.r1",
            ],
            "Added edge driven-by domain.service.svc -> requirements.requirement.r1",
        );
        run(
            &[
                "edge",
                "add",
                "contains",
                "domain.group.grp",
                "domain.service.svc",
            ],
            "Added edge contains domain.group.grp -> domain.service.svc",
        );
        run(
            &[
                "edge",
                "add",
                "realises",
                "solution.component.cmp",
                "domain.service.svc",
            ],
            "Added edge realises solution.component.cmp -> domain.service.svc",
        );
        run(
            &[
                "edge",
                "add",
                "is-role",
                "solution.component.cmp",
                "solution.role.rl",
            ],
            "Added edge is-role solution.component.cmp -> solution.role.rl",
        );

        // Buffered: neither endpoint file exists yet and no commit landed.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "stakeholder", "owner").exists(),
            "a buffered add must not write `owner` before save"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "r1").exists(),
            "a buffered add must not write `r1` before save"
        );
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
            layers::read_node_file(&wt_apg, Layer::Requirements, "stakeholder", "owner").unwrap();
        let dst_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(src_back.out.len(), 1);
        assert_eq!(src_back.out[0].kind, "requires");
        assert_eq!(src_back.out[0].target, "requirements.requirement.r1");
        assert_eq!(dst_back.in_edges.len(), 2);
        assert_eq!(dst_back.in_edges[0].kind, "requires");
        assert_eq!(
            dst_back.in_edges[0].source,
            "requirements.stakeholder.owner"
        );
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
        .q("MATCH (a:Stakeholder {fqn: 'requirements.stakeholder.owner'})-[:Requires]->(b:Requirement {fqn: 'requirements.requirement.r1'}) RETURN count(*)")
        .unwrap();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the Requires edge must be in the branch DB: {out}"
        );
        drop(db);
        testutil::remove(&repo);
    }

    /// Authored `uses` (Person→Component) and `calls` (Service→Service) edges
    /// survive the write-through re-merge: `artifacts::edge_merge` maps both
    /// record kinds and the merge guard admits their rel pairs, so the branch
    /// DB shows them exactly like a full scan would. Regression for the
    /// `_ => None` arm that used to drop them.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn edge_add_lands_authored_uses_and_calls_in_the_branch_db() {
        let (wt_apg, repo, _wt) = mutation_fixture("authored-edges");
        // `apg node add` × 4: a Person + Component (the `uses` pair) and two
        // Services (the `calls` pair).
        layers::write_project(
            &wt_apg,
            &[
                node("solution", "person", "alice"),
                node("solution", "component", "portal"),
                node("domain", "service", "svc-a"),
                node("domain", "service", "svc-b"),
            ],
            &[],
        )
        .unwrap();

        // `apg edge add uses solution.person.alice solution.component.portal`
        // and `apg edge add calls domain.service.svc-a domain.service.svc-b` —
        // both halves of each edge in one mutation.
        let mut person =
            layers::read_node_file(&wt_apg, Layer::Solution, "person", "alice").unwrap();
        person.out.push(OutEdge {
            kind: "uses".to_string(),
            target: "solution.component.portal".to_string(),
            properties: BTreeMap::new(),
        });
        let mut component =
            layers::read_node_file(&wt_apg, Layer::Solution, "component", "portal").unwrap();
        component.in_edges.push(InEdge {
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
        layers::write_project(&wt_apg, &[person, component, svc_a, svc_b], &[]).unwrap();

        // Both authored edges are in the branch DB after the write-through
        // re-merge.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let uses = db
        .q("MATCH (p:Person {fqn: 'solution.person.alice'})-[:Uses]->(s:Component {fqn: 'solution.component.portal'}) RETURN count(*)")
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
    /// endpoint (allowlist-violating name, or a dangling authored edge) writes
    /// nothing — the pre-existing files stay byte-identical and no new file
    /// lands.
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
            kind: "contains".to_string(),
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

        // (c) A tier-scoped constraint is prose-only and well-formed: a NEW
        // domain-tier constraint writes and re-merges (its layer declares its
        // scope; there is no attachment property).
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
        // Author r1 --contains--> r2 through the real CLI arms.
        node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap();
        node_add(&wt_apg, &av(&["requirements", "requirement", "r2"])).unwrap();
        edge_add(
            &wt_apg,
            &av(&[
                "contains",
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
                "contains",
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
                "contains",
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
                "contains",
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
                "contains",
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
                "contains",
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
                "driven-by",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        testutil::remove(&repo);
    }

    /// Phase-01 task-30 (rewritten from the phase-02 direct-path taxonomy):
    /// under mandatory-session admission the extended whole-durable-sequence
    /// flock and the DB open belong to a **live session**, not to a direct
    /// `cmd_node`/`cmd_edge` path.
    ///
    /// What still holds, and is asserted below:
    ///
    /// - The extended `apg/.trans/specs.lock` flock is taken by the SESSION
    ///   (`Coordinator::start` → `acquire_spec_lock`) and held for the session's
    ///   life, so the lock file exists while the session is live (and does not
    ///   exist before a session — no direct command takes it any more).
    /// - With NO live session, a durable `node`/`edge` mutation REFUSES and names
    ///   `apg session start` (the direct path is gone).
    /// - A routed durable mutation forwards to the session and is admitted into
    ///   the write-back buffer; it does NOT open `db.lbug` itself.
    /// - The read-write DB class is exclusive: while the session holds the DB, a
    ///   second read-write opener fails on the lbug file lock
    ///   (`Could not set lock on file …`).
    /// - The read-only class coexists: a routed `apg query` succeeds while the
    ///   session holds the read-write handle.
    ///
    /// Dropped from the old direct-path taxonomy (no longer a real system
    /// property under the session contract): the assertion that a direct `apg
    /// node add` **fails** on the lbug lock while a read-write handle is held —
    /// `cmd_node`/`cmd_edge` no longer open the DB at all (they forward to the
    /// session), and with no session they refuse before any DB open. The control
    /// step ("the same mutation succeeds once the handle is released") is
    /// likewise dropped: there is no direct mutation left; after `session end` a
    /// direct read-write open succeeds instead, and a durable mutation still
    /// refuses (mandatory session).
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_edge_entry_takes_extended_spec_lock_and_db_open_taxonomy() {
        let (wt_apg, repo, wt) = mutation_fixture("lock-taxonomy");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let spec_lock = wt_apg.join(specs::TRANS).join("specs.lock");

        // (1) No live session: a durable mutation refuses, naming the fix. The
        // direct path (which used to take the extended flock itself) is gone, so
        // no command has created the lock file.
        let refused = testutil::ApgCommand::new(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "no-session",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            !refused.status.success(),
            "a durable node add must refuse without a live session"
        );
        let refused_stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            refused_stderr.contains("apg session start"),
            "the refusal must name `apg session start`: {refused_stderr}"
        );
        assert!(
            !spec_lock.exists(),
            "no direct command may take the extended flock: {} must not exist before a session",
            spec_lock.display()
        );

        // (2) The live session — not the direct command — holds the extended
        // whole-durable-sequence flock for its life, so `acquire_spec_lock`'s
        // lock file exists.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );
        assert!(
            spec_lock.exists(),
            "the live session must hold the extended specs.lock flock: {} missing",
            spec_lock.display()
        );

        // (3) Durable node/edge mutations route through the live session (they
        // never open db.lbug themselves) and are admitted into the write-back
        // buffer: they succeed, and nothing is durable before save. A
        // Stakeholder `requires` a Requirement — the minimal whole-model-valid
        // authored edge (SPEC §8.1).
        for (node_type, name) in [("stakeholder", "gap-a"), ("requirement", "gap-b")] {
            let out = testutil::ApgCommand::new(&["node", "add", "requirements", node_type, name])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "routed node add {name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                format!("Added node requirements.{node_type}.{name}")
            );
            assert!(
                !layers::node_file_path(&wt_apg, Layer::Requirements, node_type, name).exists(),
                "a buffered add must not write `{name}` before save"
            );
        }
        let edge = testutil::ApgCommand::new(&[
            "edge",
            "add",
            "requires",
            "requirements.stakeholder.gap-a",
            "requirements.requirement.gap-b",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            edge.status.success(),
            "routed edge add: {}",
            String::from_utf8_lossy(&edge.stderr)
        );

        // Whole-model completion (SPEC §8.1/§3.4): the Requirement `gap-b` must
        // be `driven-by` a Service at save, and that Service needs the domain/
        // solution context satisfying the other cardinalities.
        for args in [
            &["node", "add", "domain", "service", "svc"][..],
            &["node", "add", "domain", "group", "grp"][..],
            &["node", "add", "solution", "component", "cmp"][..],
            &[
                "node",
                "add",
                "solution",
                "role",
                "rl",
                "--property",
                "applies-to=component",
            ][..],
            &[
                "edge",
                "add",
                "driven-by",
                "domain.service.svc",
                "requirements.requirement.gap-b",
            ][..],
            &[
                "edge",
                "add",
                "contains",
                "domain.group.grp",
                "domain.service.svc",
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
                "solution.role.rl",
            ][..],
        ] {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "routed {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // (4) Read-write DB class is exclusive while the session owns the DB: a
        // second read-write opener fails on the lbug file lock. (The old direct
        // `apg node add` loser is dropped — the routed entry never opens the DB.)
        let loser = ArtifactDb::open(&wt_apg);
        assert!(
            loser.is_err(),
            "a second read-write DB opener must fail while the session owns the DB"
        );
        let loser_err = format!("{}", loser.err().expect("the second open must fail"));
        assert!(
            loser_err.contains("Could not set lock on file"),
            "the read-write loser must fail on the lbug file lock: {loser_err}"
        );

        // Read-only class coexists: a routed `apg query` succeeds while the
        // session holds the read-write handle.
        let ro = spawn_apg(&["query", "MATCH (n:Requirement) RETURN count(n)"], &wt);
        assert!(
            ro.status.success(),
            "read-only apg query must coexist with the session's read-write handle: {}",
            String::from_utf8_lossy(&ro.stderr)
        );

        // (5) The single durability point: `apg session save` flushes the
        // buffered mutations; only then is the durable state asserted.
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
            layers::node_file_path(&wt_apg, Layer::Requirements, "stakeholder", "gap-a").exists(),
            "save must write the `gap-a` node file"
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "gap-b").exists(),
            "save must write the `gap-b` node file"
        );
        let a =
            layers::read_node_file(&wt_apg, Layer::Requirements, "stakeholder", "gap-a").unwrap();
        assert_eq!(a.out.len(), 1, "gap-a's out-half must be durable");
        assert_eq!(a.out[0].target, "requirements.requirement.gap-b");
        let b =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "gap-b").unwrap();
        assert_eq!(b.in_edges.len(), 2, "gap-b's in-half must be durable");
        assert_eq!(b.in_edges[0].source, "requirements.stakeholder.gap-a");

        // (6) After the session ends the read-write DB is free again, but a
        // durable mutation still refuses (mandatory session).
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let reopened = ArtifactDb::open(&wt_apg);
        assert!(
            reopened.is_ok(),
            "a read-write open must succeed once the session released the DB: {:?}",
            reopened.err()
        );
        drop(reopened);

        let post = testutil::ApgCommand::new(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "post-session",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            !post.status.success(),
            "a durable node add must still refuse with no live session"
        );

        testutil::remove(&repo);
    }

    /// Phase-01 task-31: the strict node/edge UPDATE sweep through the top-level
    /// `cmd_node`/`cmd_edge` DISPATCH, under mandatory-session admission.
    ///
    /// The durable mutations now route through a live session's write-back
    /// buffer (no `apg/layers/**` write, no commit until save), so the test
    /// SETS UP the r0 -> r1 -> r2 fixture directly (the layer primitive, as
    /// `setup_hub_and_leaves` does — setup is not the subject), opens a live
    /// session, performs the strict node/edge updates through the dispatch
    /// (buffered), and, at the session's single durability point
    /// (`apg session save`), asserts r1's incident edges SURVIVE the updates.
    /// Every update is edge-preserving: the node update MERGEs
    /// body/properties without dropping a key (only an explicit
    /// `--unset-property` drops one) and keeps r1's out- and in-halves; the
    /// edge update MERGEs/un-sets the same map onto BOTH halves.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn strict_node_and_edge_update_preserve_edges_through_dispatch() {
        let (wt_apg, repo, wt) = node_store_fixture("dispatch-strict");
        let home = repo.root.join("home");

        // r1 carries its own properties + one in-edge (r0 -> r1) and one
        // out-edge (r1 -> r2) so a node update can be checked edge-for-edge.
        // A Stakeholder requiring r0 roots the whole r0 -> r1 -> r2 requirement
        // chain (via `contains` decomposition), keeping the seeded model
        // whole-model-valid (SPEC §8.1). Setup is written directly through the
        // layer primitive; the strict updates under test are the dispatch calls
        // below.
        let mut owner = node("requirements", "stakeholder", "owner");
        owner.out = vec![OutEdge {
            kind: "requires".to_string(),
            target: "requirements.requirement.r0".to_string(),
            properties: BTreeMap::new(),
        }];
        let mut r0 = node("requirements", "requirement", "r0");
        r0.out = vec![OutEdge {
            kind: "contains".to_string(),
            target: "requirements.requirement.r1".to_string(),
            properties: BTreeMap::new(),
        }];
        r0.in_edges = vec![
            InEdge {
                kind: "requires".to_string(),
                source: "requirements.stakeholder.owner".to_string(),
                properties: BTreeMap::new(),
            },
            InEdge {
                kind: "driven-by".to_string(),
                source: "domain.service.svc".to_string(),
                properties: BTreeMap::new(),
            },
        ];
        let mut r1 = node("requirements", "requirement", "r1");
        r1.properties = BTreeMap::from([
            ("a".to_string(), "0".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        r1.out = vec![OutEdge {
            kind: "contains".to_string(),
            target: "requirements.requirement.r2".to_string(),
            properties: BTreeMap::new(),
        }];
        r1.in_edges = vec![
            InEdge {
                kind: "contains".to_string(),
                source: "requirements.requirement.r0".to_string(),
                properties: BTreeMap::new(),
            },
            InEdge {
                kind: "driven-by".to_string(),
                source: "domain.service.svc".to_string(),
                properties: BTreeMap::new(),
            },
        ];
        let mut r2 = node("requirements", "requirement", "r2");
        r2.in_edges = vec![
            InEdge {
                kind: "contains".to_string(),
                source: "requirements.requirement.r1".to_string(),
                properties: BTreeMap::new(),
            },
            InEdge {
                kind: "driven-by".to_string(),
                source: "domain.service.svc".to_string(),
                properties: BTreeMap::new(),
            },
        ];
        // Whole-model completion (SPEC §8.1/§3.4): a Service driven-by each of
        // r0/r1/r2, contained by a leaf Group and realised by a Component that
        // is-role's a Role (applies-to=component).
        let mut svc = node("domain", "service", "svc");
        svc.out = vec![
            OutEdge {
                kind: "driven-by".to_string(),
                target: "requirements.requirement.r0".to_string(),
                properties: BTreeMap::new(),
            },
            OutEdge {
                kind: "driven-by".to_string(),
                target: "requirements.requirement.r1".to_string(),
                properties: BTreeMap::new(),
            },
            OutEdge {
                kind: "driven-by".to_string(),
                target: "requirements.requirement.r2".to_string(),
                properties: BTreeMap::new(),
            },
        ];
        svc.in_edges = vec![
            InEdge {
                kind: "contains".to_string(),
                source: "domain.group.grp".to_string(),
                properties: BTreeMap::new(),
            },
            InEdge {
                kind: "realises".to_string(),
                source: "solution.component.cmp".to_string(),
                properties: BTreeMap::new(),
            },
        ];
        let mut grp = node("domain", "group", "grp");
        grp.out = vec![OutEdge {
            kind: "contains".to_string(),
            target: "domain.service.svc".to_string(),
            properties: BTreeMap::new(),
        }];
        let mut cmp = node("solution", "component", "cmp");
        cmp.out = vec![
            OutEdge {
                kind: "realises".to_string(),
                target: "domain.service.svc".to_string(),
                properties: BTreeMap::new(),
            },
            OutEdge {
                kind: "is-role".to_string(),
                target: "solution.role.rl".to_string(),
                properties: BTreeMap::new(),
            },
        ];
        let mut rl = node("solution", "role", "rl");
        rl.properties
            .insert("applies-to".to_string(), "component".to_string());
        rl.in_edges = vec![InEdge {
            kind: "is-role".to_string(),
            source: "solution.component.cmp".to_string(),
            properties: BTreeMap::new(),
        }];
        layers::write_project(&wt_apg, &[owner, r0, r1, r2, svc, grp, cmp, rl], &[]).unwrap();
        // A session always holds a database, so build one from the committed
        // baseline before opening it — the same write-then-scan pattern the
        // crash/reclaim test uses. The DB's reconstructed base carries r1's
        // incident edges, so the edge-preservation assertions still bite.
        testutil::scan_checkout(&wt).unwrap();

        let before =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(before.out.len(), 1, "one out-edge before the update");
        assert_eq!(before.in_edges.len(), 2, "two in-edges before the update");

        // Durable mutations are mandatory-session: open one so the strict
        // node/edge updates below route through its write-back buffer
        // (projected at admission, NON-durable until `apg session save`).
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the strict updates must run under a live session"
        );

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

        // A duplicate `(kind, from, to)` is refused, naming update/rm; no half
        // is duplicated. The setup edge is on disk, and the buffered node
        // updates held it, so the buffered edge build sees it through the
        // overlay and refuses.
        let edge = av(&[
            "add",
            "contains",
            "requirements.requirement.r1",
            "requirements.requirement.r2",
        ]);
        let err = with_cwd(&wt, || cmd_edge(&edge).unwrap_err());
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg edge update"), "{msg}");
        assert!(msg.contains("apg edge rm"), "{msg}");

        // `edge update --property` MERGEs the same map onto BOTH halves.
        with_cwd(&wt, || {
            cmd_edge(&av(&[
                "update",
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "a=1",
                "--property",
                "b=2",
            ]))
        })
        .unwrap();
        // The explicit unset reaches both halves too.
        with_cwd(&wt, || {
            cmd_edge(&av(&[
                "update",
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--unset-property",
                "b",
            ]))
        })
        .unwrap();

        // The whole strict sweep is BUFFERED: the durable store is identical
        // to the pre-update state until the save below.
        assert_eq!(
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap(),
            before,
            "a buffered update must not touch the node files before save"
        );

        // The single durability point: `apg session save` flushes the buffer.
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

        // The node update landed: body/properties merged — only the explicit
        // unset dropped a key — and r1's incident edges SURVIVED it.
        let after =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(after.body, "updated");
        assert_eq!(after.properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(
            after.properties.get("b"),
            None,
            "b was explicitly unset; omitting --unset-property never drops a key"
        );
        assert_eq!(after.out.len(), 1, "out-edge survives a node update");
        assert_eq!(after.out[0].kind, before.out[0].kind);
        assert_eq!(after.out[0].target, before.out[0].target);
        assert_eq!(after.in_edges.len(), 2, "in-edges survive a node update");
        assert_eq!(after.in_edges[0].kind, before.in_edges[0].kind);
        assert_eq!(after.in_edges[0].source, before.in_edges[0].source);

        // The refused duplicate added no half; the edge update MERGEd
        // {a:1,b:2} then unset b, so BOTH halves carry the identical {a:1}.
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        assert_eq!(r1.out.len(), 1, "the duplicate must not add an out-half");
        assert_eq!(
            r2.in_edges.len(),
            2,
            "the duplicate must not add an in-half"
        );
        let expect = BTreeMap::from([("a".to_string(), "1".to_string())]);
        assert_eq!(r1.out[0].properties, expect, "source out-half map");
        assert_eq!(
            r2.in_edges[0].properties, expect,
            "target in-half carries the identical map"
        );

        // End the session cleanly.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-01 task-23: with a live session, a parallel burst of N separate
    /// routed `apg edge add` processes is applied by the ONE coordinator in
    /// receive order — zero failures, and no lost update. The write-back buffer
    /// is projected into the live DB at admission but stays NON-durable (no node
    /// file, no commit) until `apg session save`, which lands the whole burst in
    /// exactly ONE commit; after save the shared hub carries exactly N distinct
    /// out-edges on disk and the DB equals the serial application.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn live_session_applies_routed_mutations_in_receive_order_as_single_writer() {
        const N: usize = 10;
        let (wt_apg, repo, wt) = mutation_fixture("session-order");
        setup_hub_and_leaves(&wt, N);
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // Baseline: the buffered burst below must not move git until save.
        let before_commits = testutil::commit_count(&wt);
        let before_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();

        // A parallel burst of N separate routed `apg edge add` processes: zero
        // failures is the single-writer / no-lost-update evidence.
        let (failed, by_lock) = run_edge_burst(&wt, &home, N);
        assert_eq!(
            failed, 0,
            "routed burst lost {failed}/{N} mutations ({by_lock:?})"
        );

        // Still buffered: a routed read observes the projected intention (all
        // N edges), but the on-disk store and git stay at the last saved state.
        let routed = testutil::spawn_apg(
            &[
                "query",
                "MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:Contains]->(b) RETURN count(*)",
            ],
            &wt,
        );
        assert!(
            routed.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("10"),
            "the buffered burst must be visible to a routed read at admission"
        );
        assert_eq!(
            hub_out_edges(&wt_apg),
            0,
            "a buffered burst must not write the hub node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered burst must not create a commit before save"
        );

        // The single durability point: one save lands the whole burst.
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

        // Durable now: the hub carries exactly N distinct out-edges — one per
        // leaf, no duplicate and no lost update — in the received order, and the
        // whole burst landed in exactly ONE commit at save.
        let hub =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "hub").unwrap();
        assert_eq!(hub.out.len(), N, "the single writer lost an edge");
        let mut targets: Vec<String> = hub
            .out
            .iter()
            .filter(|e| e.kind == "contains")
            .map(|e| e.target.clone())
            .collect();
        targets.sort();
        let mut expected: Vec<String> = (0..N)
            .map(|i| format!("requirements.requirement.leaf-{i}"))
            .collect();
        expected.sort();
        assert_eq!(
            targets, expected,
            "the saved burst must carry exactly one edge to each leaf"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the whole burst must land in exactly one save commit"
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

        // End the session cleanly.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Store == serial application, now visible in the DB (session released).
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let q = db
        .q("MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:Contains]->(b) RETURN count(*)")
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
            let add =
                testutil::ApgCommand::new(&["node", "add", "global", "constraint", name.as_str()])
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
            let query =
                format!("MATCH (n:Constraint {{fqn: 'global.constraint.{name}'}}) RETURN count(n)");
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
                !layers::node_file_path(&wt_apg, Layer::Global, "constraint", &name).exists(),
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
                layers::node_file_path(&wt_apg, Layer::Global, "constraint", &name).exists(),
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

    /// Phase-01 task-22: a forwarded mutation carries an explicit client id and
    /// is applied AT MOST ONCE — replaying the same id returns the cached reply
    /// with no second apply (the buffered state is unchanged) — and nothing it
    /// buffers is durable until `apg session save` lands it in exactly ONE
    /// commit. A forward that cannot reach the coordinator ERRORS rather than
    /// silently falling back to the direct path.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn forwarded_mutations_apply_at_most_once_and_never_fall_back() {
        let (wt_apg, repo, wt) = mutation_fixture("session-at-most-once");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        let before_commits = testutil::commit_count(&wt);

        // Forward a node add with an explicit client id. It is admitted into the
        // write-back buffer (and projected into the live DB), but is NOT durable:
        // no node file is on disk and no commit is made.
        let args = av(&["add", "global", "constraint", "once"]);
        let first = session_forward_node(&wt_apg, "dup-1", &args);
        assert_eq!(first, "Added node global.constraint.once");
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "once").exists(),
            "a buffered forward must not write the node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered forward must not create a commit before save"
        );

        // Replay the SAME client id: the coordinator returns the cached reply
        // and NEVER re-applies. A re-apply of the strict add would refuse (the
        // node already exists in the buffer), so the cached reply is the
        // at-most-once evidence; the buffered state is unchanged.
        let replay = session_forward_node(&wt_apg, "dup-1", &args);
        assert_eq!(replay, first, "a replayed id must return the cached reply");
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a replay must not create a second commit"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "once").exists(),
            "a replay must not write anything to the durable store"
        );

        // The buffered mutation is observable through a routed read, and exactly
        // once — the replay added no second copy to the live projection.
        let count = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.once'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            count.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&count.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&count.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the replayed mutation must be present exactly once"
        );

        // `apg session save` is the single durability point: the one buffered
        // forward lands as the node file plus exactly ONE commit.
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
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "once").exists(),
            "save must write the forwarded node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the buffered forward must land in exactly one commit"
        );

        // End the session cleanly. A forward now ERRORS — no direct-path
        // fallback — and nothing lands locally.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ghost = av(&["add", "global", "constraint", "ghost"]);
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
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "ghost").exists(),
            "a failed forward must not fall back to the direct path"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "a failed forward must not create a commit"
        );

        testutil::remove(&repo);
    }

    /// Session lifecycle exclusivity (phase-03 task-21, extended phase-02
    /// task-12): one session per worktree DB; a second start refuses; `apg scan`
    /// and `apg project merge` refuse while a session is live, naming the fix
    /// (`apg session save`, then `apg session end`); a dirty `session end`
    /// refuses — it reports the pending change, releases nothing, and the
    /// session stays live with the scan gate still closed; `apg session save`
    /// flushes the buffer in exactly one commit and `session end` then
    /// releases; and once ended the scan/merge gate is clear. Routed reads keep
    /// working and a non-routing direct DB open is out of contract.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_lifecycle_is_exclusive_with_scan_and_merge() {
        let (wt_apg, repo, wt) = mutation_fixture("session-exclusive");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);
        let before_commits = testutil::commit_count(&wt);

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

        // (b) `apg scan` refuses while the session owns db.lbug, naming the
        // fix: `apg session save` then `apg session end`.
        let scan = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!scan.status.success(), "scan must refuse a live session");
        let scan_err = String::from_utf8_lossy(&scan.stderr);
        assert!(
            scan_err.contains("live `apg session`"),
            "scan must name the live session: {scan_err}"
        );
        assert!(
            scan_err.contains("apg session save") && scan_err.contains("apg session end"),
            "scan must name `apg session save`/`apg session end`: {scan_err}"
        );

        // (c) `apg project merge` refuses while the session owns the branch DB,
        // naming the same fix.
        let merge = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!merge.status.success(), "merge must refuse a live session");
        let merge_err = String::from_utf8_lossy(&merge.stderr);
        assert!(
            merge_err.contains("live `apg session`"),
            "merge must name the live session: {merge_err}"
        );
        assert!(
            merge_err.contains("apg session save") && merge_err.contains("apg session end"),
            "merge must name `apg session save`/`apg session end`: {merge_err}"
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

        // (e) A routed durable mutation dirties the write-back buffer: still no
        // node file and no commit (the refused scan/merge above wrote nothing
        // durable either).
        let add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "global",
            "constraint",
            "pending",
            "--body",
            "buffered",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "pending").exists(),
            "a buffered add must not write a node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered add (and the refused scan/merge) must not create a commit"
        );

        // (f) `apg session end` over the dirty buffer REFUSES: it reports the
        // pending change, releases nothing, and the session stays live with the
        // exclusivity gate still closed.
        let end = testutil::ApgCommand::new(&["session", "end"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !end.status.success(),
            "session end must refuse a dirty buffer"
        );
        let end_err = String::from_utf8_lossy(&end.stderr);
        assert!(
            end_err.contains("session end refused"),
            "end must report the refusal: {end_err}"
        );
        assert!(
            end_err.contains("1 pending change(s)"),
            "end must report the pending-change count: {end_err}"
        );
        assert!(
            end_err.contains("write global.constraint.pending"),
            "end must name the pending change: {end_err}"
        );
        assert!(
            apg::session::live_session(&wt_apg),
            "a refused end must not release the session"
        );

        // The still-live dirty session keeps `apg scan` refused.
        let scan_still = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !scan_still.status.success()
                && String::from_utf8_lossy(&scan_still.stderr).contains("live `apg session`"),
            "a refused end must keep the scan gate closed: {}",
            String::from_utf8_lossy(&scan_still.stderr)
        );

        // (g) `apg session save` flushes the buffer in exactly one commit;
        // `apg session end` then releases the session.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "pending").exists(),
            "save must write the buffered node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "save must flush the buffer in exactly one commit"
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "session end must release the saved session"
        );

        // (h) With the session gone the scan/merge gate is clear: `apg scan`
        // no longer refuses (save re-anchored scan_meta, so the unchanged tree
        // hits the freshness fast path and the scan succeeds), and the merge
        // path is no longer blocked by the session gate.
        let scan_after = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            scan_after.status.success(),
            "scan must run after save+end: {}",
            String::from_utf8_lossy(&scan_after.stderr)
        );
        let merge_after = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !String::from_utf8_lossy(&merge_after.stderr).contains("live `apg session`"),
            "merge must not be blocked by the session gate after end: {}",
            String::from_utf8_lossy(&merge_after.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-01 task-18 (e2e): genuine cross-process read-your-writes under the
    /// buffered session contract. A routed `apg node add` is admitted into the
    /// live session's write-back buffer and projected into the session-held
    /// `db.lbug`; a SEPARATE `apg query` process (a different binary) routed
    /// through the socket observes it as soon as the add returns — while the
    /// mutation is still UNSAVED (no node file, no commit). After `apg session
    /// save` makes the buffer durable and the session ends cleanly, a fresh
    /// query process with NO live session opens `db.lbug` directly and reads the
    /// same saved state.
    ///
    /// Both halves are asserted explicitly:
    /// (a) **live session** ⇒ the new query process routes through the socket
    /// and sees the still-unsaved mutation (read-your-writes through the
    /// session);
    /// (b) **no live session** (after save + end) ⇒ the new query process reads
    /// the last saved state directly from `db.lbug`, never opening a session.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn read_your_writes_cross_process_direct_and_session() {
        let (wt_apg, repo, wt) = mutation_fixture("read-your-writes");
        let home = repo.root.join("home");

        // (a) A live session admits the mutation into its write-back buffer and
        // projects it into the session-held DB; it is NOT yet durable.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "variant (a) must run with a live session"
        );
        let before_commits = testutil::commit_count(&wt);
        let add = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "foo"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );

        // Still buffered: no node file written, no commit made.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "foo").exists(),
            "a buffered add must not write a node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered add must not create a commit before save"
        );

        // A SEPARATE query process routes through the live session and sees the
        // mutation before save/end — read-your-writes through the session.
        let query = "MATCH (n:Constraint {fqn: 'global.constraint.foo'}) RETURN count(n)";
        let q = testutil::spawn_apg(&["query", query], &wt);
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
            "a NEW process must read the routed mutation before save/end"
        );

        // (b) `apg session save` is the durability point; then the session ends
        // cleanly, leaving NO live session for the direct read.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "foo").exists(),
            "save must write the buffered node file"
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "variant (b) must run with no live session"
        );

        // With no live session the DB is directly openable, and a fresh query
        // process opens `db.lbug` directly (no session) and reads the last
        // saved state.
        assert!(
            ArtifactDb::open(&wt_apg).is_ok(),
            "with no live session the DB must be directly openable"
        );
        let q2 = testutil::spawn_apg(&["query", query], &wt);
        assert!(
            q2.status.success(),
            "direct query: {}",
            String::from_utf8_lossy(&q2.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q2.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "with no live session a NEW process must read the saved state directly"
        );

        testutil::remove(&repo);
    }

    /// Phase-01 task-29 (acceptance): the cross-process parallel burst of N
    /// SEPARATE `apg edge add` binaries routes through ONE live session — the
    /// single writer — and completes with ZERO lock errors on every named lock:
    /// the lbug `apg/.trans/db.lbug` read-write open, the
    /// `apg/.trans/specs.lock` flock, and git's `.git/index.lock`. The buffered
    /// burst is projected into the session-held DB and is observable to a
    /// routed read as the serial application; the on-disk store equals that
    /// serial application once `apg session save` makes the whole burst durable
    /// in exactly ONE commit, and the session ends cleanly.
    ///
    /// Cross-process by construction (`ApgCommand`/`spawn_apg`): `SPEC_LOCK`
    /// is a process-lifetime `OnceLock` flock, so an in-process thread burst
    /// would pass with the lock absent and false-green exactly the race this
    /// test exists to catch. Routing the whole burst through the single live
    /// session means no two children ever contend for the DB/specs/git locks
    /// directly — the session is the ONE writer that serialises them.
    ///
    /// N=4 keeps genuine cross-process contention over the shared hub's
    /// read-modify-write while keeping the child fan-out modest (each child is
    /// a full 33 MB debug `apg`, and the whole e2e tier runs hundreds of them
    /// concurrently).
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

        let (wt_apg, repo, wt) = mutation_fixture("accept-burst-session");
        setup_hub_and_leaves(&wt, N);
        let home = repo.root.join("home");

        // ONE live session is the single writer for the whole burst.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the acceptance burst must run against one live session"
        );

        // Baseline: the buffered burst below must not move git until save.
        let before_commits = testutil::commit_count(&wt);

        // The cross-process burst: N SEPARATE `apg edge add` binaries, each
        // routed through the live session's socket to the ONE writer.
        let hist = run_edge_burst_attributed(&wt, &home, N);
        eprintln!("acceptance burst per-lock: {hist:?}");
        assert_eq!(
            hist.get("ok"),
            Some(&N),
            "the routed burst must complete every mutation: {hist:?}"
        );
        for lock in NAMED_LOCKS {
            assert_eq!(
                hist.get(lock),
                None,
                "the routed burst hit {lock}: {hist:?}"
            );
        }

        // Serial store, buffered: a routed read observes the projected intention
        // (all N edges), while the on-disk store and git stay at the last saved
        // state.
        let routed = testutil::spawn_apg(
            &[
                "query",
                "MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:Contains]->(b) RETURN count(*)",
            ],
            &wt,
        );
        assert!(
            routed.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("4"),
            "the buffered burst must be visible to a routed read as the serial application"
        );
        assert_eq!(
            hub_out_edges(&wt_apg),
            0,
            "a buffered burst must not write the hub node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered burst must not create a commit before save"
        );

        // The single durability point: one save lands the whole burst.
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

        // Durable now: the shared hub carries exactly N distinct out-edges — the
        // serial store — landed in exactly ONE commit at save.
        assert_eq!(
            hub_out_edges(&wt_apg),
            N,
            "the saved burst must be the serial store (one edge per leaf)"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the whole burst must land in exactly one save commit"
        );

        // End the session cleanly.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the session must have ended"
        );

        // Store == serial application, now visible in the DB with the session
        // gone and no live writer.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            let q = db
            .q("MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:Contains]->(b) RETURN count(*)")
            .unwrap();
            assert_eq!(q.lines().last().map(str::trim), Some("4"), "{q}");
        }

        testutil::remove(&repo);
    }

    /// Phase-03 task-13: `apg node rm` on a node reviewed by outstanding
    /// (open/actioned) Feedback PROCEEDS — the writer decides — and the
    /// write-time warning naming each unresolved item
    /// (`apg: warning: removing `<fqn>` — it is reviewed by unresolved
    /// feedback: …`) rides the session reply and reaches the CLIENT's stderr
    /// while the removal completes. The Feedback records are authoritative and
    /// survive the target's removal; `apg review list` still reports both items
    /// marked `(removed target)`. A node whose items are all `resolved`, and a
    /// node with no items, remove cleanly with NO warning.
    ///
    /// Durable mutations are mandatory-session, so every node add/rm runs under
    /// a live `apg session start` with `apg session save` as the single
    /// durability point; the transient `apg review …` writes take the same
    /// extended flock as a live session, so they run only between sessions.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_rm_warns_on_outstanding_feedback_and_keeps_the_record() {
        let (wt_apg, repo, wt) = mutation_fixture("node-rm-warn");
        let home = repo.root.join("home");

        // A whole-model-valid base (SPEC §8.1): a Stakeholder requiring an
        // anchor Requirement, driven-by a Service contained by a leaf Group and
        // realised by a Component that is-role's a Role. Each reviewed
        // Requirement added below is rooted by a routed `requires` edge from
        // `owner` and driven-by the seeded Service, so `node rm` of the reviewed
        // node leaves `owner` and the Service still satisfying their minimums.
        let mut owner = node("requirements", "stakeholder", "owner");
        owner.out = vec![testutil::out_edge(
            "requires",
            "requirements.requirement.anchor",
        )];
        let mut anchor = node("requirements", "requirement", "anchor");
        anchor.in_edges = vec![
            testutil::in_edge("requires", "requirements.stakeholder.owner"),
            testutil::in_edge("driven-by", "domain.service.svc"),
        ];
        let mut svc = node("domain", "service", "svc");
        svc.out = vec![testutil::out_edge(
            "driven-by",
            "requirements.requirement.anchor",
        )];
        svc.in_edges = vec![
            testutil::in_edge("contains", "domain.group.grp"),
            testutil::in_edge("realises", "solution.component.cmp"),
        ];
        let mut grp = node("domain", "group", "grp");
        grp.out = vec![testutil::out_edge("contains", "domain.service.svc")];
        let mut cmp = node("solution", "component", "cmp");
        cmp.out = vec![
            testutil::out_edge("realises", "domain.service.svc"),
            testutil::out_edge("is-role", "solution.role.rl"),
        ];
        let mut rl = node("solution", "role", "rl");
        rl.properties
            .insert("applies-to".to_string(), "component".to_string());
        rl.in_edges = vec![testutil::in_edge("is-role", "solution.component.cmp")];
        layers::write_project(&wt_apg, &[owner, anchor, svc, grp, cmp, rl], &[]).unwrap();

        // Run a durable `args` under a fresh live session, make the buffered
        // change durable with `save`, then end the session cleanly. A node ADD
        // also roots the new Requirement under the seeded `owner` and drives it
        // from the seeded Service.
        let mutate = |args: &[&str]| -> std::process::Output {
            let session = testutil::start_session_process(&wt, &home);
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            if args.first().is_some_and(|s| *s == "node")
                && args.get(1).is_some_and(|s| *s == "add")
            {
                let add_rooted = testutil::ApgCommand::new(&[
                    "edge",
                    "add",
                    "requires",
                    "requirements.stakeholder.owner",
                    &format!("requirements.requirement.{}", args[4]),
                ])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
                assert!(
                    add_rooted.status.success(),
                    "rooting edge for {}: {}",
                    args[4],
                    String::from_utf8_lossy(&add_rooted.stderr)
                );
                let add_driven = testutil::ApgCommand::new(&[
                    "edge",
                    "add",
                    "driven-by",
                    "domain.service.svc",
                    &format!("requirements.requirement.{}", args[4]),
                ])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
                assert!(
                    add_driven.status.success(),
                    "driven-by edge for {}: {}",
                    args[4],
                    String::from_utf8_lossy(&add_driven.stderr)
                );
            }
            let save = testutil::spawn_apg(&["session", "save"], &wt);
            assert!(
                save.status.success(),
                "session save failed: {}",
                String::from_utf8_lossy(&save.stderr)
            );
            let end = end_session(&wt, session);
            assert!(
                end.status.success(),
                "session end failed: {}",
                String::from_utf8_lossy(&end.stderr)
            );
            out
        };
        // A direct CLI run — the transient review writes, which run only while
        // no session holds the extended flock.
        let run = |args: &[&str]| -> std::process::Output {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "apg {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        // --- Outstanding feedback (one open, one actioned) → removal proceeds ---
        mutate(&[
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

        // The routed warning names each unresolved item on the CLIENT's stderr,
        // while the removal completes (mutate already asserts success).
        let rm = mutate(&["node", "rm", "requirements", "requirement", "gone"]);
        let stderr = String::from_utf8_lossy(&rm.stderr);
        assert!(
            stderr.contains("apg: warning: removing `requirements.requirement.gone`"),
            "the warning must name the removed node on the client's stderr: {stderr}"
        );
        assert!(
            stderr.contains("it is reviewed by unresolved feedback:"),
            "the warning must announce the unresolved feedback: {stderr}"
        );
        assert!(
            stderr.contains("foo/feedback-1 (open)"),
            "the warning must name the open item: {stderr}"
        );
        assert!(
            stderr.contains("foo/feedback-2 (actioned)"),
            "the warning must name the actioned item: {stderr}"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "gone").exists(),
            "the removed node must not survive as a node file after save"
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

        // --- All resolved → removal still succeeds ---
        mutate(&[
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
        // An all-resolved node removes without the outstanding-feedback warning.
        let rm = mutate(&["node", "rm", "requirements", "requirement", "calm"]);
        let stderr = String::from_utf8_lossy(&rm.stderr);
        assert!(
            !stderr.contains("apg: warning:"),
            "an all-resolved node must remove without a warning: {stderr}"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "calm").exists(),
            "the all-resolved node's removal must land"
        );

        // --- No feedback → removal still succeeds ---
        mutate(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "bare",
            "--body",
            "x",
        ]);
        // An unreviewed node removes without a warning too.
        let rm = mutate(&["node", "rm", "requirements", "requirement", "bare"]);
        let stderr = String::from_utf8_lossy(&rm.stderr);
        assert!(
            !stderr.contains("apg: warning:"),
            "an unreviewed node must remove without a warning: {stderr}"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "bare").exists(),
            "the unreviewed node's removal must land"
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
        // so the baseline the later mutations must NOT disturb is non-empty. A
        // lone `global.constraint` keeps the baseline whole-model-valid (SPEC
        // §8.1 — no cardinality minimums).
        let saved = testutil::ApgCommand::new(&[
            "node",
            "add",
            "global",
            "constraint",
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
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
            ],
            "Added edge contains requirements.requirement.a -> requirements.requirement.b",
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
                "MATCH (:Requirement {fqn: 'requirements.requirement.a'})-[:Contains]->(b) RETURN count(*)",
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

        // End WITHOUT a dirty end: `apg session end` now refuses to release a
        // session whose buffer is non-empty (phase-02 task-3). Discard the
        // buffered set with `apg session abort` instead — the test's intent is
        // that these later mutations never become durable (the only save is the
        // baseline above), so abort leaves `apg/layers/**` and git exactly at
        // the last saved state. The abort releases the session and the serve
        // loop returns, so the child exits cleanly.
        let abort = testutil::spawn_apg(&["session", "abort"], &wt);
        assert!(
            abort.status.success(),
            "{}",
            String::from_utf8_lossy(&abort.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&abort.stdout).trim(),
            "Session aborted"
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("apg session: aborted"),
            "the coordinator must report the abort"
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "abort must release the session"
        );
        testutil::remove(&repo);
    }

    /// Phase-02 task-10: `apg session save` is the single durability point for
    /// the database's pending state. Each admitted mutation projects into the
    /// held `db.lbug` and marks exactly the node rows it changes dirty, so a run
    /// of mutations followed by one save writes EXACTLY those marked rows under
    /// `apg/layers/**` in one atomic write plus exactly ONE git commit — every
    /// node file no mutation touched stays byte-identical, and no file is
    /// written for an untouched identity — and then clears the marks, so a
    /// second save over the emptied pending set writes nothing and makes no
    /// commit. The session stays live throughout and ends cleanly afterwards.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn save_flushes_the_buffer_in_one_commit() {
        let (wt_apg, repo, wt) = mutation_fixture("save-flush");
        let home = repo.root.join("home");

        // A cardinality-consistent base (SPEC §4): a Stakeholder requiring an
        // anchor Requirement, driven-by a Service contained by a leaf Group and
        // realised by a Component that is-role's a Role. Each added Requirement
        // below is rooted by a `requires` edge from `owner` and driven-by the
        // seeded Service.
        let mut owner = node("requirements", "stakeholder", "owner");
        owner.out = vec![testutil::out_edge(
            "requires",
            "requirements.requirement.anchor",
        )];
        let mut anchor = node("requirements", "requirement", "anchor");
        anchor.in_edges = vec![
            testutil::in_edge("requires", "requirements.stakeholder.owner"),
            testutil::in_edge("driven-by", "domain.service.svc"),
        ];
        let mut svc = node("domain", "service", "svc");
        svc.out = vec![testutil::out_edge(
            "driven-by",
            "requirements.requirement.anchor",
        )];
        svc.in_edges = vec![
            testutil::in_edge("contains", "domain.group.grp"),
            testutil::in_edge("realises", "solution.component.cmp"),
        ];
        let mut grp = node("domain", "group", "grp");
        grp.out = vec![testutil::out_edge("contains", "domain.service.svc")];
        let mut cmp = node("solution", "component", "cmp");
        cmp.out = vec![
            testutil::out_edge("realises", "domain.service.svc"),
            testutil::out_edge("is-role", "solution.role.rl"),
        ];
        let mut rl = node("solution", "role", "rl");
        rl.properties
            .insert("applies-to".to_string(), "component".to_string());
        rl.in_edges = vec![testutil::in_edge("is-role", "solution.component.cmp")];
        layers::write_project(&wt_apg, &[owner, anchor, svc, grp, cmp, rl], &[]).unwrap();

        // The baseline the save is measured against: the six-node base on disk
        // (repo-relative keys, so it compares against the save commit's paths),
        // the node-file path of an identity, and the base's commit count + HEAD.
        let snapshot = |root: &Path| -> BTreeMap<String, Vec<u8>> {
            let mut out = BTreeMap::new();
            let mut stack = vec![root.join(specs::LAYOUT).join(layers::LAYERS_DIR)];
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
                            .strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        out.insert(rel, std::fs::read(&path).unwrap());
                    }
                }
            }
            out
        };
        let layer_path = |layer: Layer, node_type: &str, name: &str| -> String {
            layers::node_file_path(&wt_apg, layer, node_type, name)
                .strip_prefix(&wt)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let saved_layers = snapshot(&wt);
        assert_eq!(
            saved_layers.len(),
            6,
            "the six-node base must be on disk before the run: {saved_layers:?}"
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

        let session = testutil::start_session_process(&wt, &home);

        // A run of durable mutations admitted through the live session: three
        // new nodes, each rooted by a durable `requires` rewrite of `owner` and
        // driven-by a rewrite of `svc`, plus one edge between two of them.
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
        for name in ["alpha", "beta", "gamma"] {
            run(
                &[
                    "edge",
                    "add",
                    "requires",
                    "requirements.stakeholder.owner",
                    &format!("requirements.requirement.{name}"),
                ],
                &format!(
                    "Added edge requires requirements.stakeholder.owner -> requirements.requirement.{name}"
                ),
            );
        }
        // Each added Requirement is also driven-by the seeded Service (SPEC
        // §3.4 — every Requirement is driven-by at least one Service in the
        // durable model).
        for name in ["alpha", "beta", "gamma"] {
            run(
                &[
                    "edge",
                    "add",
                    "driven-by",
                    "domain.service.svc",
                    &format!("requirements.requirement.{name}"),
                ],
                &format!(
                    "Added edge driven-by domain.service.svc -> requirements.requirement.{name}"
                ),
            );
        }
        run(
            &[
                "edge",
                "add",
                "contains",
                "requirements.requirement.alpha",
                "requirements.requirement.beta",
            ],
            "Added edge contains requirements.requirement.alpha -> requirements.requirement.beta",
        );

        // Nothing the run admitted is durable: every projection lives only in
        // the held database, so the node-file tree is still the base's and no
        // commit landed.
        assert_eq!(
            snapshot(&wt),
            saved_layers,
            "an admission must not write a node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits,
            "an admission must not create a commit before save"
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

        // The single durability point: one save writes the database's marked
        // rows — the three new Requirements plus the two base rows the run's
        // edges rewrote.
        let marked: BTreeSet<String> = [
            layer_path(Layer::Requirements, "requirement", "alpha"),
            layer_path(Layer::Requirements, "requirement", "beta"),
            layer_path(Layer::Requirements, "requirement", "gamma"),
            layer_path(Layer::Requirements, "stakeholder", "owner"),
            layer_path(Layer::Domain, "service", "svc"),
        ]
        .into_iter()
        .collect();
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

        // EXACTLY the marked rows were written: the tree differs from the saved
        // baseline at those paths alone, so every unchanged node file is still
        // byte-identical and no file appeared for an untouched identity.
        let after_layers = snapshot(&wt);
        let changed: BTreeSet<&str> = after_layers
            .iter()
            .filter(|(path, bytes)| saved_layers.get(*path) != Some(*bytes))
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(
            changed,
            marked.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            "save must write exactly the marked rows and leave every unchanged node file byte-identical"
        );

        // The durable store holds every added identity …
        let store = layers::read_existing_nodes(&wt_apg).unwrap();
        for name in ["alpha", "beta", "gamma"] {
            assert!(
                store.iter().any(|n| n.name.as_str() == name),
                "the durable store must contain `{name}` after save"
            );
        }
        // … including BOTH halves of the marked edge.
        let alpha =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "alpha").unwrap();
        let beta =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "beta").unwrap();
        assert_eq!(
            alpha
                .out
                .iter()
                .filter(|e| e.kind == "contains"
                    && e.target.as_str() == "requirements.requirement.beta")
                .count(),
            1,
            "save must write the marked row's out-edge"
        );
        assert_eq!(
            beta.in_edges
                .iter()
                .filter(|e| e.kind == "contains"
                    && e.source.as_str() == "requirements.requirement.alpha")
                .count(),
            1,
            "save must write the marked edge's matching in-half"
        );

        // The whole marked set landed in ONE commit: the commit's own delta is
        // exactly those paths, and it moved HEAD by one normal commit.
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits + 1,
            "the marked write set must land in exactly one commit"
        );
        let git = git2::Repository::open(&wt).unwrap();
        let after_commit = git.head().unwrap().peel_to_commit().unwrap();
        let after_head = after_commit.id().to_string();
        assert_ne!(after_head, saved_head, "save must create a commit");
        assert_eq!(
            after_commit.parent_count(),
            1,
            "the save commit is a normal single-parent commit"
        );
        let before_commit = after_commit.parent(0).unwrap();
        assert_eq!(
            before_commit.id().to_string(),
            saved_head,
            "the save commit's parent must be the last saved state"
        );
        let committed: BTreeSet<String> = git
            .diff_tree_to_tree(
                Some(&before_commit.tree().unwrap()),
                Some(&after_commit.tree().unwrap()),
                None,
            )
            .unwrap()
            .deltas()
            .filter_map(|delta| {
                delta
                    .new_file()
                    .path()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .collect();
        assert_eq!(
            committed, marked,
            "the one save commit must carry exactly the marked rows"
        );

        // The marks are drained: a second save over the emptied pending set
        // writes nothing, makes no commit, and leaves the session live.
        let resave = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            resave.status.success(),
            "{}",
            String::from_utf8_lossy(&resave.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits + 1,
            "a save over the emptied pending set must make no commit"
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
            "a save over the emptied pending set must not move HEAD"
        );
        assert_eq!(
            snapshot(&wt),
            after_layers,
            "a save over the emptied pending set must not write or change any apg/layers file"
        );

        // The session stayed live through both saves …
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // … and ends cleanly, its own report naming the marked set the first
        // save wrote and the emptied pending set the second read.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let reported = String::from_utf8_lossy(&out.stdout);
        assert!(
            reported.contains(&format!(
                "Session saved: {} change(s) in one commit",
                marked.len()
            )),
            "save must report the marked rows it wrote: {reported}"
        );
        assert!(
            reported.contains("Session saved: no pending changes"),
            "the second save must report the emptied pending set: {reported}"
        );
        testutil::remove(&repo);
    }

    /// Phase-02 task-13: `apg session save` is the structure-and-edge-pairing
    /// boundary, not a whole-model one. A model with a Component that has no
    /// Role — and no `implemented-by` claim — saves successfully: the boundary
    /// validates the dirty rows' structure and the held set's edge pairing,
    /// while the whole-model minimums are deferred to `apg spec lint` and the
    /// merge strength check. The dirty set lands in exactly one commit and the
    /// Component's absent `implemented-by` claim stays deferred.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_save_defers_whole_model_minimums_to_lint_and_merge_and_lands_the_dirty_set_in_one_commit()
     {
        let (wt_apg, repo, wt) = mutation_fixture("save-whole-model");
        let home = repo.root.join("home");

        // A complete, cardinality-consistent durable base (SPEC §4), written
        // through the direct path before the session starts.
        testutil::seed_valid_model(&wt_apg);
        let session = testutil::start_session_process(&wt, &home);
        let before = testutil::commit_count(&wt);

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
        let component_path =
            layers::node_file_path(&wt_apg, Layer::Solution, "component", "refund-handler");

        // A new Component that is owned and realises a Service but carries NO
        // Role and no `implemented-by` claim. Both are whole-model minimums
        // (SPEC §4.4) the save boundary no longer judges: admission accepts
        // each mutation and the save below accepts the resulting set.
        run(
            &["node", "add", "solution", "component", "refund-handler"],
            "Added node solution.component.refund-handler",
        );
        run(
            &[
                "edge",
                "add",
                "contains",
                "solution.container.order-api",
                "solution.component.refund-handler",
            ],
            "Added edge contains solution.container.order-api -> solution.component.refund-handler",
        );
        run(
            &[
                "edge",
                "add",
                "realises",
                "solution.component.refund-handler",
                "domain.service.place-order",
            ],
            "Added edge realises solution.component.refund-handler -> domain.service.place-order",
        );

        // The save boundary judges the dirty rows' structure and the held
        // set's edge pairing — nothing more — so this Component-without-Role
        // model saves. (The retired `session save refused` / `` `is-role` 0
        // Role(s) `` refusal assertion belonged to the whole-model pass, which
        // lives on in `apg spec lint` and the merge strength check.)
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
        assert!(
            component_path.exists(),
            "the save must write the component row's node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before + 1,
            "the dirty set must land in exactly one commit"
        );

        // The deferred claim stays deferred: the saved Component carries no
        // `implemented-by` out-edge, and the save did not require one.
        let saved_component =
            layers::read_node_file(&wt_apg, Layer::Solution, "component", "refund-handler")
                .unwrap();
        assert!(
            saved_component
                .out
                .iter()
                .all(|e| e.kind != "implemented-by"),
            "the Component's implemented-by claim must stay deferred to lint and merge"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The coordinator's own report names the three marked rows the save
        // wrote: the Component plus the two endpoint rows its two edges landed on.
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .contains("Session saved: 3 change(s) in one commit"),
            "the save must report the dirty rows it wrote: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        testutil::remove(&repo);
    }

    /// Phase-02 task-11: `apg session save` reads the session's pending state
    /// from the held database — the marked rows plus the pending-removal ledger
    /// — never a buffer, so a session carrying none saves nothing. A live
    /// session that has admitted NO durable mutation writes no node file,
    /// creates no commit, leaves HEAD and the whole on-disk `apg/layers/**`
    /// tree byte-identical, reports the coordinator's `Session saved: no
    /// pending changes`, and stays live. The same holds for a session whose
    /// only admitted mutation was saved already: that save cleared the marks,
    /// so the next save reads an empty pending set and repeats the no-op. (The
    /// dirty counterpart — one save writes the marked rows in exactly one
    /// commit — is `save_flushes_the_buffer_in_one_commit`.)
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn save_on_a_clean_buffer_is_a_noop() {
        let (wt_apg, repo, wt) = mutation_fixture("clean-save-noop");
        let home = repo.root.join("home");

        // A pre-existing durable node file, committed through the direct path:
        // `apg/layers/**` is non-empty before the session starts, so "nothing
        // written or changed" is a real claim rather than a vacuous one. The
        // base is a lone `global.constraint` — no edge, no cardinality
        // minimum — so the one save below writes the single node file its one
        // admitted mutation adds.
        layers::write_project(&wt_apg, &[node("global", "constraint", "existing")], &[]).unwrap();

        // A live session that has admitted no mutation: its held database
        // carries no dirty row and its pending-removal ledger holds no
        // identity, by construction.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // Baseline: commit count, HEAD, and the whole on-disk node-file tree.
        let head = || -> String {
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        };
        let before_commits = testutil::commit_count(&wt);
        let before_head = head();
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
            "the seeded node file must be on disk before the first save: {before_layers:?}"
        );

        // (1) A session that admitted nothing: the save is a pure no-op.
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
            "a save over the empty pending set must not create a commit"
        );
        assert_eq!(
            head(),
            before_head,
            "a save over the empty pending set must not move HEAD"
        );

        // No node file written or changed: the whole tree is byte-identical.
        assert_eq!(
            snapshot(&wt_apg),
            before_layers,
            "a save over the empty pending set must not write or change any apg/layers file"
        );

        // The session stays live through the no-op save.
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // (2) The session's ONLY admitted mutation: nothing is durable before
        // the save (the admission projected into the held database alone), and
        // that save clears the marks, so the save after it repeats the no-op.
        let add = testutil::spawn_apg(&["node", "add", "global", "constraint", "added"], &wt);
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&add.stdout).trim(),
            "Added node global.constraint.added"
        );
        assert_eq!(
            snapshot(&wt_apg),
            before_layers,
            "an admission must not write a node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "an admission must not create a commit before save"
        );

        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "added").exists(),
            "the save must write the one admitted node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the one admitted node file must land in exactly one commit"
        );
        let saved_head = head();
        assert_ne!(saved_head, before_head, "the save must create a commit");
        let saved_layers = snapshot(&wt_apg);

        // The save over the now-empty pending set: the drained marks mean
        // nothing is written, no commit lands, and HEAD stays at the save.
        let resave = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            resave.status.success(),
            "{}",
            String::from_utf8_lossy(&resave.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&resave.stdout).trim(),
            "Session saved",
            "the re-save's client reply"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "a save over the emptied pending set must make no commit"
        );
        assert_eq!(
            head(),
            saved_head,
            "a save over the emptied pending set must not move HEAD"
        );
        assert_eq!(
            snapshot(&wt_apg),
            saved_layers,
            "a save over the emptied pending set must not write or change any apg/layers file"
        );
        assert!(
            apg::session::live_session(&wt_apg),
            "session save must not end the live session"
        );

        // End cleanly; the coordinator's captured output reports each save it
        // served: the clean session's no-op, the one-commit write of the one
        // admitted row, then the no-op the drained marks leave behind.
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let server_out = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            server_out
                .matches("Session saved: no pending changes")
                .count(),
            2,
            "both saves over an empty pending set must report it: {server_out}"
        );
        assert!(
            server_out.contains("Session saved: 1 change(s) in one commit"),
            "the admitted mutation's save must report the one row it wrote: {server_out}"
        );

        testutil::remove(&repo);
    }

    /// Phase-02 task-10 (cli-session-crash-recovery): a SIGKILLed session is an
    /// UNCLEAN EXIT. After the kill the `db.lbug` on disk still carries the
    /// phantom projection of the buffer's admitted-but-unsaved mutation, and the
    /// process died before it could checkpoint, leaving a `db.lbug.wal` sidecar
    /// beside a present-but-unconnectable session socket. The next `apg query`
    /// must NOT serve that stale index: it detects the stale socket, reclaims
    /// it, forces the full rebuild (which discards `db.lbug` + its `.wal`/`.shm`
    /// sidecars + `graph.jsonl`) from the durable `apg/layers/**` node files, and
    /// serves ONLY the last-saved state — the buffered change is gone.
    ///
    /// The two seeded durable nodes are committed directly through
    /// `layers::write_project` BEFORE the session starts, so the fixture is a
    /// SAIDI-free last-saved state at HEAD: the crash-recovery scan sees a clean
    /// tree (no frontend spawn, no monkeypatching of HEAD), rebuilds the index
    /// from the node files, and the read-your-writes assertion is exact.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn killed_session_discards_the_dirty_buffer_and_repairs_the_db() {
        let (wt_apg, repo, wt) = node_store_fixture("killed-dirty-repair");
        let home = repo.root.join("home");

        // The durable baseline: two committed node files, projected into a real
        // `db.lbug` — the LAST SAVED state the recovery must serve. The write
        // and the projection both run before any session exists, so HEAD sits
        // on a clean tree (the scan below takes its full path, not the
        // content-identity fast path).
        layers::write_project(
            &wt_apg,
            &[
                node("requirements", "requirement", "keep-a"),
                node("requirements", "requirement", "keep-b"),
            ],
            &[],
        )
        .unwrap();
        testutil::scan_checkout(&wt).unwrap();

        // A live session owns the DB; its buffer then admits one routed durable
        // mutation and PROJECTS it into the held `db.lbug` at admission.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );
        let add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "phantom",
            "--body",
            "buffered-but-never-durable",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );

        // The buffer is dirty: nothing durable was written for `phantom`, and
        // while the session lives its routed read DOES see the buffered change.
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "phantom")
                .exists(),
            "the buffered add must not write a node file before save"
        );
        let routed = testutil::spawn_apg(
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

        // Baseline for the durable-state assertions: commit count, HEAD, and the
        // whole on-disk node-file store.
        let before_commits = testutil::commit_count(&wt);
        let before_head = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();

        // SIGKILL — deliberately NOT a graceful `end`. The unsaved buffer dies
        // with the process while its phantom projection stays in `db.lbug`, and
        // the file left behind is a present-but-unreachable socket.
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

        // The next `apg query` must reclaim the stale socket, discard the stale
        // index + its WAL sidecar, force the full rebuild from `apg/layers/**`,
        // and serve ONLY the last-saved state.
        let q = testutil::spawn_apg(
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
        // Header row + exactly the two durable requirements, in order.
        assert_eq!(
            rows,
            vec![
                "n.fqn",
                "requirements.requirement.keep-a",
                "requirements.requirement.keep-b"
            ],
            "the recovered query must reflect only the last-saved state"
        );
        assert!(
            !String::from_utf8_lossy(&q.stdout).contains("phantom"),
            "the recovered query must never serve the phantom projection"
        );
        // The stale socket was reclaimed and the recovery left no live session.
        assert!(
            !socket.exists(),
            "the recovery must reclaim the stale socket"
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the recovery leaves no live session behind"
        );

        // The durable node files are unchanged: the buffered change was
        // discarded, never written, never committed.
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
            testutil::commit_count(&wt),
            before_commits,
            "a discarded buffer creates no commit"
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
            "a discarded buffer must not move HEAD"
        );

        testutil::remove(&repo);
    }

    /// Phase-02 task-14: `apg session end` is gated on the HELD DATABASE's
    /// pending state, never an in-memory buffer: a dirty row or a
    /// pending-removal ledger identity is an admitted mutation whose projection
    /// landed in `db.lbug` while no `apg/layers/**` node file backs it, so
    /// `end` REFUSES to release the session, reports the pending count and
    /// lists each pending `write` and each pending `delete` identity, and stays
    /// LIVE — a routed read keeps serving the pending projection. `apg session
    /// abort` then discards that pending set and releases the session; because
    /// the database carried pending state it also discards the derived index
    /// and rebuilds it from the durable node files, so no unsaved projection is
    /// ever served again: the pending write is gone and the pending delete is
    /// back. Nothing became durable — no `apg/layers/**` file, no commit, HEAD
    /// unmoved. The recall command sequence (save, abort, then a clean `end`)
    /// still releases the session.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn end_refuses_on_a_dirty_buffer_and_abort_discards() {
        let (wt_apg, repo, wt) = mutation_fixture("dirty-end-abort");
        let home = repo.root.join("home");

        // A durable baseline written and projected BEFORE the session starts: a
        // node to REMOVE, so the pending set carries a ledger DELETE identity
        // beside the dirty WRITE identity admitted below. Durability is keyed on
        // this pre-session row.
        layers::write_project(&wt_apg, &[node("global", "constraint", "doomed")], &[]).unwrap();

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // Baseline: the LAST SAVED state the abort must leave untouched — the
        // whole `apg/layers/**` tree (repo-relative keys), the commit count and
        // HEAD.
        let snapshot = |root: &Path| -> BTreeMap<String, Vec<u8>> {
            let mut out = BTreeMap::new();
            let mut stack = vec![root.join(specs::LAYOUT).join(layers::LAYERS_DIR)];
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
                            .strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        out.insert(rel, std::fs::read(&path).unwrap());
                    }
                }
            }
            out
        };
        let head = || -> String {
            git2::Repository::open(&wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        };
        let before_commits = testutil::commit_count(&wt);
        let before_head = head();
        let saved_layers = snapshot(&wt);
        let pending_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "pending");
        let doomed_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "doomed");
        assert!(
            doomed_path.exists(),
            "the durable baseline must exist before the run"
        );
        assert!(
            !pending_path.exists(),
            "the pending name must not exist before the mutation"
        );

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
        // Reads the held database through the live session (or, once it is
        // gone, the rebuilt index) and returns the single scalar it projects.
        let routed_count = |query: &str| -> String {
            let out = testutil::spawn_apg(&["query", query], &wt);
            assert!(
                out.status.success(),
                "routed query `{query}`: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .to_string()
        };

        // A routed write and a routed delete are admitted into the held
        // database's working state: still no node file for either, no commit.
        run(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "pending",
                "--body",
                "unsaved",
            ],
            "Added node global.constraint.pending",
        );
        run(
            &["node", "rm", "global", "constraint", "doomed"],
            "Removed node global.constraint.doomed",
        );
        assert!(
            !pending_path.exists(),
            "an admission must not write the pending node file before save"
        );
        assert!(
            doomed_path.exists(),
            "an admission must not remove the delete's durable node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "an admission must not create a commit before save"
        );

        // `apg session end` over that pending state REFUSES: non-zero, reports
        // the pending count, lists the dirty WRITE and the ledger DELETE, and
        // releases nothing.
        let end = testutil::ApgCommand::new(&["session", "end"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !end.status.success(),
            "session end must refuse a database carrying pending state"
        );
        let end_err = String::from_utf8_lossy(&end.stderr);
        assert!(
            end_err.contains("session end refused"),
            "end must report the refusal: {end_err}"
        );
        assert!(
            end_err.contains("2 pending change(s)"),
            "end must count the dirty rows plus the ledger identities: {end_err}"
        );
        assert!(
            end_err.contains("write global.constraint.pending"),
            "end must list each pending write: {end_err}"
        );
        assert!(
            end_err.contains("delete global.constraint.doomed"),
            "end must list each pending delete identity: {end_err}"
        );

        // The refusal released nothing: the session stays LIVE and its index
        // serves the pending write, with the pending delete already absent.
        assert!(
            apg::session::live_session(&wt_apg),
            "a refused end must not release the session"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.pending'}) RETURN count(*)"),
            "1",
            "the still-live session must serve the pending write"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN count(*)"),
            "0",
            "the still-live session must drop the pending delete's row"
        );

        // `apg session abort` discards the pending set and releases the session;
        // over pending state it also discards the derived index and rebuilds it
        // from the durable node files.
        let abort = testutil::ApgCommand::new(&["session", "abort"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            abort.status.success(),
            "{}",
            String::from_utf8_lossy(&abort.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&abort.stdout).trim(),
            "Session aborted"
        );

        // The serve loop returned after abort: the process exits cleanly, and
        // its own report proves the discard-and-rebuild completed (a failed
        // rebuild reports `abort failed` instead).
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("apg session: aborted"),
            "the coordinator must report the abort"
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "abort must release the session"
        );

        // The rebuilt index serves ONLY the durable node files: the pending
        // write is gone and the pending delete is back, so no unsaved projection
        // survives the abort.
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.pending'}) RETURN count(*)"),
            "0",
            "the rebuilt index must never serve the discarded write"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN count(*)"),
            "1",
            "the rebuilt index must carry the discarded delete's durable node"
        );

        // Nothing became durable: no file for the discarded write, the delete's
        // file still there, the whole `apg/layers/**` tree byte-identical, no
        // commit, HEAD unmoved.
        assert!(
            !pending_path.exists(),
            "an aborted admission must never write a node file"
        );
        assert!(
            doomed_path.exists(),
            "an aborted delete must never remove the durable node file"
        );
        assert_eq!(
            snapshot(&wt),
            saved_layers,
            "abort must leave `apg/layers/**` byte-identical to its last saved state"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "abort must not create a commit"
        );
        assert_eq!(head(), before_head, "abort must not move HEAD");

        // The recall command sequence still releases a session: save makes the
        // one pending write durable in one commit, abort over the now-empty
        // pending set releases without a discard, and a clean `end` confirms no
        // session is left to release.
        let session = testutil::start_session_process(&wt, &home);
        run(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "recalled",
                "--body",
                "recalled",
            ],
            "Added node global.constraint.recalled",
        );
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
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "recalled").exists(),
            "the recall sequence's save must write the one pending node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the recall sequence's save must land in exactly one commit"
        );
        let abort = testutil::spawn_apg(&["session", "abort"], &wt);
        assert!(
            abort.status.success(),
            "{}",
            String::from_utf8_lossy(&abort.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&abort.stdout).trim(),
            "Session aborted",
            "the abort over the emptied pending set releases the session"
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("apg session: aborted"),
            "the coordinator must report the abort"
        );
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&end.stdout).trim(),
            "no live session to end",
            "a clean end after abort finds the session already released"
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the recall sequence must leave no live session"
        );

        testutil::remove(&repo);
    }

    /// Phase-02 task-11: `apg scan` and `apg project merge` are exclusive with
    /// a live session — each REFUSES while the session owns the worktree DB,
    /// naming the fix (`apg session save`, then `apg session end`). Once the
    /// session is saved and ended, the exclusivity gate is gone: `apg scan`
    /// RUNS (the unchanged tree hits the freshness fast path and the scan
    /// succeeds without a frontend spawn), and the merge path is no longer
    /// refused by the session gate (it proceeds past exclusivity to the
    /// fixture's unrelated missing-plan refusal). No refusal, save, end or
    /// post-end scan creates a commit.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn scan_and_merge_run_after_save_then_end() {
        let (wt_apg, repo, wt) = mutation_fixture("scan-merge-session-exclusive");
        let home = repo.root.join("home");
        let before_commits = testutil::commit_count(&wt);

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the session must be live"
        );

        // (a) While the session is live, `apg scan` refuses and names both the
        // fix commands (`apg session save`, `apg session end`).
        let scan = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!scan.status.success(), "scan must refuse a live session");
        let scan_err = String::from_utf8_lossy(&scan.stderr);
        assert!(
            scan_err.contains("a live `apg session`"),
            "scan must name the live session: {scan_err}"
        );
        assert!(
            scan_err.contains("apg session save") && scan_err.contains("apg session end"),
            "scan must name `apg session save`/`apg session end`: {scan_err}"
        );

        // (b) While the session is live, `apg project merge` refuses the same
        // way (from the clean main checkout).
        let merge = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!merge.status.success(), "merge must refuse a live session");
        let merge_err = String::from_utf8_lossy(&merge.stderr);
        assert!(
            merge_err.contains("a live `apg session`"),
            "merge must name the live session: {merge_err}"
        );
        assert!(
            merge_err.contains("apg session save") && merge_err.contains("apg session end"),
            "merge must name `apg session save`/`apg session end`: {merge_err}"
        );

        // A refused scan/merge writes nothing durable.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a refused scan/merge must not create a commit"
        );

        // (c) `apg session save` (clean buffer — a no-op) then `apg session
        // end` releases the exclusivity gate.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "session end must release the session"
        );

        // (d) With the session gone, `apg scan` RUNS: the unchanged tree hits
        // the content-identity freshness fast path and the scan succeeds (no
        // frontend spawn, and no commit).
        let scan_after = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            scan_after.status.success(),
            "scan must run after save+end: {}",
            String::from_utf8_lossy(&scan_after.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a post-end scan must not create a commit"
        );

        // (e) The merge path is no longer blocked by the session gate: it
        // proceeds past exclusivity to the fixture's unrelated missing-plan
        // refusal (a full merge needs a plan/branch graph and is covered by
        // the merge/plan e2e crates).
        let merge_after = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !String::from_utf8_lossy(&merge_after.stderr).contains("a live `apg session`"),
            "merge must not be blocked by the session gate after end: {}",
            String::from_utf8_lossy(&merge_after.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-03 task-11: a durable `apg node add` routed through a live session
    /// returns its write-time wording advisory in the session reply, and the
    /// caller prints it on ITS OWN stderr at the mutation (`apg: warning: …`)
    /// while the write's result is left intact — the add succeeds, prints its
    /// success message, buffers, and lands on save. A routed mutation whose
    /// `--body` carries no likely-flagged wording prints NO warning, so the
    /// advisory is body-driven rather than unconditional. Covers the
    /// routed-warning carrier of `rust.apg.session.Coordinator.handle_mutation`
    /// and its print in `rust.apg.node_cmd.cmd_node`.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn routed_warning_reaches_the_client() {
        let (wt_apg, repo, wt) = node_store_fixture("routed-warning");
        let home = repo.root.join("home");
        let path = layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "demo-val");

        // A session always holds a database, so seed a committed baseline and
        // scan it before opening the session — the same write-then-scan pattern
        // the crash/reclaim test uses. The base is a complete, whole-model-valid
        // durable model (SPEC §8.1), so the `demo-val` requirement added below
        // can be rooted and driven-by'd onto it at save.
        testutil::seed_valid_model(&wt_apg);
        testutil::scan_checkout(&wt).unwrap();

        // Durable mutations are mandatory-session: one live `apg session start`
        // owns the DB AND the write-back buffer, so the `node` writes below are
        // forwarded to it and staged (NON-durable until `apg session save`).
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the durable mutation must run under a live session"
        );

        let run = |args: &[&str]| -> std::process::Output {
            testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output()
        };

        // A routed `node add` whose body carries time-relative wording (`was`)
        // produces the write-time advisory. The write SUCCEEDS (the advisory
        // never blocks it), its success message is printed, and the advisory
        // reaches the CALLER's stderr, naming the mutated node FQN.
        let out = run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "demo-val",
            "--body",
            "The old flow was synchronous.",
        ]);
        assert!(
            out.status.success(),
            "the advisory must not block the routed add: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Added node requirements.requirement.demo-val",
            "the write's result must be intact"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("apg: warning: requirements.requirement.demo-val:"),
            "the warning must name the mutated node on the caller's stderr: {stderr}"
        );
        assert!(
            stderr.contains(apg::spec_lint::WORDING_ADVISORY),
            "the wording advisory must reach the caller's stderr: {stderr}"
        );

        // A routed `node update` whose supplied `--body` carries no
        // likely-flagged wording emits NO warning — the advisory is
        // body-driven, not unconditional — and still succeeds.
        let out = run(&[
            "node",
            "update",
            "requirements",
            "requirement",
            "demo-val",
            "--body",
            "The service stores the record.",
        ]);
        assert!(
            out.status.success(),
            "a plain routed update must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("apg: warning:"),
            "plain wording must not print a warning: {stderr}"
        );

        // Root the new requirement under the seeded Stakeholder and drive it
        // from the seeded Service so the buffered model stays whole-model-valid
        // at the save boundary (SPEC §8.1/§3.4).
        for edge in [
            &[
                "edge",
                "add",
                "requires",
                "requirements.stakeholder.owner",
                "requirements.requirement.demo-val",
            ][..],
            &[
                "edge",
                "add",
                "driven-by",
                "domain.service.place-order",
                "requirements.requirement.demo-val",
            ][..],
        ] {
            let out = run(edge);
            assert!(
                out.status.success(),
                "routed edge add: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // The write's result is intact: the buffered state is durable on save
        // and carries the plain body (not the flagged one).
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            path.exists(),
            "the flagged-but-advisory add must still land its node file"
        );
        let back = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "demo-val")
            .unwrap();
        assert_eq!(back.body, "The service stores the record.");

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the session must be ended cleanly"
        );

        testutil::remove(&repo);
    }

    /// Phase-02 task-4: a worktree with no `apg/.trans/db.lbug` cannot host a
    /// session — a spawned candidate `apg session start` refuses, naming
    /// `apg scan` as the remedy, and the refusal leaves nothing behind: no
    /// socket bound and no extended `specs.lock` flock held. The proof is that
    /// a subsequent hermetic scan (the test stand-in for `apg scan`) succeeds,
    /// and a subsequent `apg session start` then succeeds and ends cleanly.
    /// The refusal is raised by the DB open, which precedes the socket bind, so
    /// a refused start is a side-effect-free no-op on the worktree.
    ///
    /// project-context-guard phase-07.task-6: the project-context guard ADMITS
    /// this project worktree, so the refusal is pinned to the DB cause ALONE —
    /// its stderr carries NEITHER the guard's `mutation refused` prefix NOR the
    /// `project context` wording, proving the absent-`db.lbug` refusal still
    /// follows the (admitting) guard unchanged.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_start_refuses_without_db_naming_scan() {
        let (wt_apg, repo, wt) = node_store_fixture("session-no-db");
        let home = repo.root.join("home");

        // A durable node gives the later scan authored content to ingest.
        // `write_project` commits the node file but skips its projection leg
        // when `db.lbug` is absent, so the fixture stays DB-less — the exact
        // state under test.
        layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "seeded")],
            &[],
        )
        .unwrap();

        let db = wt_apg.join(specs::TRANS).join("db.lbug");
        let socket = apg::session::socket_path(&wt_apg);
        assert!(!db.exists(), "fixture must start with no db.lbug");

        // (a) The refusal: the candidate binary exits non-zero and its stderr
        // names `apg scan`.
        let refused = testutil::ApgCommand::new(&["session", "start"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !refused.status.success(),
            "session start must refuse an absent db.lbug"
        );
        let err = String::from_utf8_lossy(&refused.stderr);
        assert!(
            err.contains("apg scan"),
            "the refusal must name `apg scan`: {err}"
        );
        // The guard admits this worktree, so the DB cause is the ONLY cause on
        // stderr — never the guard's project-context wording.
        assert!(
            !err.contains("mutation refused"),
            "the guard must admit the project worktree: {err}"
        );
        assert!(
            !err.contains("project context"),
            "the guard must admit the project worktree: {err}"
        );

        // (b) Nothing left behind: no DB conjured, no socket bound, no live
        // session.
        assert!(!db.exists(), "a refused start must not create db.lbug");
        assert!(
            !socket.exists(),
            "a refused start must leave no socket behind"
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "a refused start leaves no live session"
        );

        // (c) The extended flock is not still held: a subsequent scan (the
        // hermetic stand-in for `apg scan`) succeeds and creates the DB the
        // session needs.
        testutil::scan_checkout(&wt).unwrap();
        assert!(db.exists(), "the scan must create db.lbug");

        // (d) A subsequent `apg session start` then succeeds — re-acquiring the
        // extended flock proves the refused start released it — and ends
        // cleanly.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the post-refusal session must be live"
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the session must be ended cleanly"
        );

        testutil::remove(&repo);
    }

    /// project-context-guard phase-07.task-3 (E2E): `apg session start` on the
    /// MAIN checkout refuses for project context BEFORE it reclaims a stale
    /// socket or takes `db.lbug`.
    ///
    /// (i) With NO `db.lbug` the refusal carries the project-context wording —
    /// naming branch `main` (the repo's default branch) and the `apg project
    /// start` fix — and NEVER `apg scan`: a guard placed after `open_owned_db`
    /// would report the absent-DB remedy instead. No socket is bound.
    ///
    /// (ii) After a hermetic scan (so the DB exists) and with a stale socket
    /// planted at the session socket path holding known bytes, the second start
    /// refuses identically and the socket survives byte-identically: a guard
    /// placed after `reclaim_stale_socket` would delete it, and one after
    /// `acquire_spec_lock`/`open_owned_db` would take the DB. No flock assertion
    /// is possible from outside the process, so the socket survival plus the
    /// absent-DB wording are the cross-process discriminators.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/db/process); run via cargo test-e2e"]
    fn session_start_on_main_refuses_context_before_reclaim_lock_and_db() {
        let repo = Repo::new("session-start-main");
        let apg_root = repo.apg_root();
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let socket = apg::session::socket_path(&apg_root);
        let db = apg_root.join(specs::TRANS).join("db.lbug");

        let start = || {
            testutil::ApgCommand::new(&["session", "start"])
                .cwd(&repo.root)
                .env("HOME", home.to_str().unwrap())
                .output()
        };
        let assert_refusal = |out: &std::process::Output| {
            assert!(!out.status.success(), "session start must refuse on main");
            let msg = String::from_utf8_lossy(&out.stderr);
            let refusal = msg.strip_prefix("apg: ").unwrap_or(&msg);
            assert!(refusal.starts_with("mutation refused — "), "{msg}");
            for needle in [
                "branch `main`",
                "the repo's default branch",
                "apg project start",
            ] {
                assert!(
                    msg.contains(needle),
                    "the refusal must name `{needle}`: {msg}"
                );
            }
            assert!(
                !msg.contains("apg scan"),
                "the guard must precede the DB refusal: {msg}"
            );
            assert!(
                !apg::session::live_session(&apg_root),
                "a refused start leaves no live session"
            );
        };

        // (i) Absent DB: the project-context refusal, not the `apg scan` remedy.
        assert!(!db.exists(), "fixture must start with no db.lbug");
        assert_refusal(&start());
        assert!(
            !socket.exists(),
            "a refused start on main must bind no socket"
        );

        // (ii) The DB now exists, and a stale socket holds known bytes.
        testutil::scan_checkout(&repo.root).unwrap();
        assert!(db.exists(), "the scan must create db.lbug");
        let stale: &[u8] = b"stale session socket bytes\n";
        std::fs::write(&socket, stale).unwrap();

        let before_commits = testutil::commit_count(&repo.root);
        assert_refusal(&start());
        assert!(
            socket.exists(),
            "a refused start must leave a stale session.sock in place"
        );
        assert_eq!(
            std::fs::read(&socket).unwrap().as_slice(),
            stale,
            "the stale socket must survive byte-identically (the guard precedes reclaim)"
        );
        assert_no_durable_trace(&apg_root, &repo.root, before_commits);

        testutil::remove(&repo);
    }

    /// project-context-guard phase-07.task-4 (E2E): `apg session start`
    /// refuses a checkout with no project context, and binds no socket.
    ///
    /// (i) A DETACHED HEAD (at the scanned commit) holds no branch, so the
    /// refusal states exactly that and carries the one `apg project start` fix
    /// line — never the `apg scan` DB remedy and never `already live`.
    ///
    /// (ii) A plain directory (no git repository) carrying a real `db.lbug`
    /// refuses naming `not inside a git repository`, `holds no branch`,
    /// `git init` and `apg project start`; neither case binds a socket or
    /// writes a node file.
    #[test]
    #[ignore = "e2e tier: real I/O (fs/git/db/process); run via cargo test-e2e"]
    fn session_start_refuses_detached_head_and_non_git() {
        // (i) Detached HEAD at the scanned commit.
        let repo = Repo::new("session-start-detached");
        let apg_root = repo.apg_root();
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        testutil::scan_checkout(&repo.root).unwrap();
        {
            let git_repo = git2::Repository::open(&repo.root).unwrap();
            let head = git_repo.head().unwrap().peel_to_commit().unwrap().id();
            git_repo.set_head_detached(head).unwrap();
            assert!(
                git_repo.head_detached().unwrap(),
                "HEAD must be detached for this scenario"
            );
        }

        let out = testutil::ApgCommand::new(&["session", "start"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !out.status.success(),
            "a detached HEAD must refuse session start"
        );
        let msg = String::from_utf8_lossy(&out.stderr);
        let refusal = msg.strip_prefix("apg: ").unwrap_or(&msg);
        assert!(refusal.starts_with("mutation refused — "), "{msg}");
        assert!(msg.contains("holds no branch"), "{msg}");
        assert_eq!(
            msg.matches("Fix:").count(),
            1,
            "exactly one Fix: line: {msg}"
        );
        assert!(msg.contains("apg project start"), "{msg}");
        assert!(!msg.contains("apg scan"), "{msg}");
        assert!(!msg.contains("already live"), "{msg}");
        let socket = apg::session::socket_path(&apg_root);
        assert!(!socket.exists(), "a refused start must bind no socket");
        assert!(
            !apg::session::live_session(&apg_root),
            "a refused start leaves no live session"
        );
        testutil::remove(&repo);

        // (ii) A non-git directory carrying a real db.lbug.
        let dir =
            std::env::temp_dir().join(format!("apg-project-{}-session-nongit", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let apg_root = dir.join(specs::LAYOUT);
        std::fs::create_dir_all(apg_root.join(specs::TRANS)).unwrap();
        testutil::touch_db(&apg_root);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let out = testutil::ApgCommand::new(&["session", "start"])
            .cwd(&dir)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !out.status.success(),
            "a non-git directory must refuse session start"
        );
        let msg = String::from_utf8_lossy(&out.stderr);
        for needle in [
            "not inside a git repository",
            "holds no branch",
            "git init",
            "apg project start",
        ] {
            assert!(
                msg.contains(needle),
                "the refusal must name `{needle}`: {msg}"
            );
        }
        assert!(
            !apg::session::socket_path(&apg_root).exists(),
            "a refused start must bind no socket: {msg}"
        );
        assert!(
            !apg::session::live_session(&apg_root),
            "a refused start leaves no live session"
        );
        let layers_dir = apg_root.join(layers::LAYERS_DIR);
        assert!(
            !layers_dir.exists() || std::fs::read_dir(&layers_dir).unwrap().next().is_none(),
            "a refused start must write nothing under apg/layers"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// feedback-18 / phase-01 task-8: a DURABLE node that is first updated and
    /// then removed in the same unsaved run keeps its identity on the
    /// pending-removal ledger, so `save` removes its node file. Durability is
    /// keyed on FIRST admission — the earlier update cannot make the remove
    /// drop the delete — and admission detaches the row at once, so BEFORE the
    /// save a routed read reports the node absent while the ledger holds its
    /// identity. The save then removes exactly that node file in one commit and
    /// leaves the untouched neighbour's file alone.
    ///
    /// The ledger's post-save drain is `session save`'s own contract, pinned by
    /// `session_save_flushes_buffered_writes_and_delete_as_is_in_one_commit` —
    /// deliberately not asserted here.
    ///
    /// phase-01 task-15: the update admission's dirty mark is pinned POSITIVELY
    /// as well — between the routed update and the remove, the held DB's own
    /// `Constraint` row for the updated identity reads `updated = 1` while the
    /// untouched neighbour's row still reads the clean base's `0` — so dropping
    /// or widening `Coordinator::apply_mutation`'s `mark_dirty(&change.writes)`
    /// fails here, not only at the direct `mark_dirty` unit proof.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_rm_of_a_saved_node_that_was_updated_leaves_no_file_and_one_commit() {
        // The untouched neighbour is a durable node file written BEFORE the
        // scan, so its row loads as the clean base (`updated = 0` — the only
        // state an admission ever marks). A node created through the session
        // would instead carry its own admission's mark, so it could not
        // witness that admission marks exactly the change's write set.
        let (wt_apg, repo, wt) = node_store_fixture("session-rm-marker");
        layers::write_project(&wt_apg, &[node("global", "constraint", "keep")], &[]).unwrap();
        testutil::scan_checkout(&wt).unwrap();
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

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
        // Reads the held database through the live session and returns the
        // single scalar the query projects.
        let routed_count = |query: &str| -> String {
            let out = testutil::spawn_apg(&["query", query], &wt);
            assert!(
                out.status.success(),
                "routed query `{query}`: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .to_string()
        };

        // A durable baseline: a node to remove, beside the untouched neighbour
        // the fixture already wrote (both `global.constraint`s, so the baseline
        // is whole-model-valid — SPEC §8.1 carries no cardinality minimums).
        run(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "doomed",
                "--body",
                "first",
            ],
            "Added node global.constraint.doomed",
        );
        let first_save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            first_save.status.success(),
            "{}",
            String::from_utf8_lossy(&first_save.stderr)
        );
        let doomed_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "doomed");
        let keep_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "keep");
        assert!(
            doomed_path.exists(),
            "the baseline save must have written the `doomed` node file"
        );
        let keep_bytes = std::fs::read(&keep_path).unwrap();
        let commits_before = testutil::commit_count(&wt);

        // An update (buffering the durable node) followed by a remove in the
        // same unsaved run, then the single durability point.
        run(
            &[
                "node",
                "update",
                "global",
                "constraint",
                "doomed",
                "--body",
                "second",
            ],
            "Updated node global.constraint.doomed",
        );
        // The update admission's POSITIVE dirty mark, read through the session
        // before the save: exactly the row of the identity this change rewrote
        // carries `updated = 1`, and the untouched neighbour's row still reads
        // the clean base's `0`. The projection alone would leave the rewritten
        // row clean (`MERGE` re-creates it with `updated` NULL), and a widened
        // mark would drag the neighbour in — so neither dropping nor widening
        // `Coordinator::apply_mutation`'s `mark_dirty(&change.writes)` passes.
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN n.updated"),
            "1",
            "the update admission must mark exactly the updated identity's row dirty"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.keep'}) RETURN n.updated"),
            "0",
            "the update admission must leave the untouched neighbour's row clean"
        );

        run(
            &["node", "rm", "global", "constraint", "doomed"],
            "Removed node global.constraint.doomed",
        );

        // BEFORE the save, read the held database through the live session: the
        // removed identity is ledger-held, while an ordinary read of the node's
        // own table reports it absent (admission detached the row at once).
        assert_eq!(
            routed_count(
                "MATCH (n:PendingRemoval {fqn: 'global.constraint.doomed'}) RETURN count(*)"
            ),
            "1",
            "the removed durable identity must be ledger-held before the save"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN count(*)"),
            "0",
            "an ordinary session read must report the removed node absent before the save"
        );

        // Nothing durable has moved yet: the last saved file is still there,
        // the untouched neighbour is byte-identical, and no commit has landed.
        assert!(
            doomed_path.exists(),
            "a pending removal must not touch `apg/layers/**` before save"
        );
        assert_eq!(
            std::fs::read(&keep_path).unwrap(),
            keep_bytes,
            "an untouched node file must stay byte-identical before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before,
            "a pending removal must not create a commit before save"
        );

        // The single durability point removes exactly the ledger identity's
        // file, in exactly one commit, and leaves the neighbour alone.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            !doomed_path.exists(),
            "save must remove the durable `doomed` node file"
        );
        assert!(
            keep_path.exists(),
            "save must leave the untouched `keep` node file in place"
        );
        assert_eq!(
            std::fs::read(&keep_path).unwrap(),
            keep_bytes,
            "save must leave the untouched node file byte-identical"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 1,
            "save must land exactly one new commit"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// feedback-18 (negative half) / phase-01 task-12: a node CREATED and then
    /// REMOVED in the same unsaved run was never durable, so it must net to
    /// nothing on the identity-only pending set — no node file, no
    /// `PendingRemoval` ledger identity and no dirty row. The save reads its
    /// write set from the database's dirty rows and its delete set from the
    /// ledger, so that empty set short-circuits: nothing is written and no
    /// commit lands. The delete marker is keyed on FIRST-admission durability,
    /// so an identity the session only ever created is never ledger-held, and
    /// its row is detached at admission.
    ///
    /// A session read BEFORE and AFTER the save reports the node absent.
    ///
    /// phase-01 task-14: the mirror arm — a node that was SAVED, then removed
    /// and re-added in one unsaved run. The re-add clears the removed
    /// identity's ledger row at admission, so the pending set is the re-added
    /// write alone: the save writes the re-added file (last admitted content)
    /// instead of deleting the durable file the ledger held.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_rm_of_a_node_created_in_the_same_run_leaves_no_file() {
        let (wt_apg, repo, wt) = mutation_fixture("session-rm-created");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);
        let commits_before = testutil::commit_count(&wt);

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
        // Reads the held database through the live session and returns the
        // single scalar the query projects.
        let routed_count = |query: &str| -> String {
            let out = testutil::spawn_apg(&["query", query], &wt);
            assert!(
                out.status.success(),
                "routed query `{query}`: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .to_string()
        };

        run(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "ephemeral",
                "--body",
                "transient",
            ],
            "Added node requirements.requirement.ephemeral",
        );
        run(
            &["node", "rm", "requirements", "requirement", "ephemeral"],
            "Removed node requirements.requirement.ephemeral",
        );

        let node_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "ephemeral");

        // BEFORE the save: the never-durable identity is neither on the ledger
        // nor dirty, and an ordinary session read reports it absent (admission
        // detached the row at once).
        assert_eq!(
            routed_count(
                "MATCH (n:PendingRemoval {fqn: 'requirements.requirement.ephemeral'}) RETURN count(*)"
            ),
            "0",
            "a created-then-removed identity must never be ledger-held"
        );
        assert_eq!(
            routed_count("MATCH (n:Requirement) WHERE n.updated = 1 RETURN count(*)"),
            "0",
            "a created-then-removed identity must leave no dirty row"
        );
        assert_eq!(
            routed_count(
                "MATCH (n:Requirement {fqn: 'requirements.requirement.ephemeral'}) RETURN count(*)"
            ),
            "0",
            "an ordinary session read must report the removed node absent before the save"
        );
        assert!(
            !node_path.exists(),
            "an add-then-rm must never write a node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before,
            "a pending mutation must not create a commit before save"
        );

        // The single durability point over an empty pending set — no marked row
        // and no ledger identity — writes nothing and commits nothing; the
        // coordinator's own report after the run pins that it read the empty
        // pending set rather than taking the write path.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );

        // AFTER the save: still no file, no ledger identity, no dirty row, no
        // commit — and the session read still reports the node absent.
        assert!(
            !node_path.exists(),
            "save must not materialize the created-then-removed node"
        );
        assert_eq!(
            routed_count(
                "MATCH (n:PendingRemoval {fqn: 'requirements.requirement.ephemeral'}) RETURN count(*)"
            ),
            "0",
            "save must leave no ledger identity for the created-then-removed node"
        );
        assert_eq!(
            routed_count("MATCH (n:Requirement) WHERE n.updated = 1 RETURN count(*)"),
            "0",
            "save must leave no dirty row for the created-then-removed node"
        );
        assert_eq!(
            routed_count(
                "MATCH (n:Requirement {fqn: 'requirements.requirement.ephemeral'}) RETURN count(*)"
            ),
            "0",
            "the session read after the save must still report the node absent"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before,
            "a no-op save must create no commit"
        );

        // The mirror arm: a DURABLE baseline node (`global.constraint` carries
        // no cardinality minimums, so the baseline model is whole-model-valid),
        // saved once, then removed and re-added in one unsaved run.
        run(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "reborn",
                "--body",
                "first",
            ],
            "Added node global.constraint.reborn",
        );
        let reborn_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "reborn");
        let baseline_save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            baseline_save.status.success(),
            "{}",
            String::from_utf8_lossy(&baseline_save.stderr)
        );
        assert!(
            reborn_path.exists(),
            "the baseline save must write the `reborn` node file"
        );
        let saved_commits = testutil::commit_count(&wt);

        run(
            &["node", "rm", "global", "constraint", "reborn"],
            "Removed node global.constraint.reborn",
        );
        run(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "reborn",
                "--body",
                "second",
            ],
            "Added node global.constraint.reborn",
        );

        // BEFORE the save: the re-add cleared the removed identity's ledger row
        // at admission, so the identity is not pending, an ordinary session read
        // reports it present again, and `apg/layers/**` still carries the last
        // SAVED content — the re-add is durable only at the save.
        assert_eq!(
            routed_count(
                "MATCH (n:PendingRemoval {fqn: 'global.constraint.reborn'}) RETURN count(*)"
            ),
            "0",
            "a removed-then-re-added identity must not stay ledger-held"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.reborn'}) RETURN count(*)"),
            "1",
            "the session read before the save must report the re-added node present"
        );
        assert_eq!(
            layers::read_node_file(&wt_apg, Layer::Global, "constraint", "reborn")
                .unwrap()
                .body,
            "first",
            "a pending re-add must not touch `apg/layers/**` before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits,
            "a pending re-add must not create a commit before save"
        );

        // AFTER the save: the file is present with the LAST ADMITTED content
        // rather than deleted, in exactly one commit, and the session read
        // still reports the node present.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert!(
            reborn_path.exists(),
            "save must write the re-added node file, never delete it"
        );
        assert_eq!(
            layers::read_node_file(&wt_apg, Layer::Global, "constraint", "reborn")
                .unwrap()
                .body,
            "second",
            "save must write the last admitted content"
        );
        assert_eq!(
            routed_count(
                "MATCH (n:PendingRemoval {fqn: 'global.constraint.reborn'}) RETURN count(*)"
            ),
            "0",
            "save must leave no ledger identity for the re-added node"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.reborn'}) RETURN count(*)"),
            "1",
            "the session read after the save must still report the node present"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits + 1,
            "the re-add save must land exactly one new commit"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The coordinator's reports pin the no-op: the created-then-removed
        // identity's save read an empty pending set, never a dirty row to write
        // or a ledger identity to delete.
        let server_out = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            server_out
                .matches("Session saved: no pending changes")
                .count(),
            1,
            "the created-then-removed identity's save must report no pending changes: {server_out}"
        );
        testutil::remove(&repo);
    }

    /// Phase-05 task-12: a routed durable mutation touches ONLY its own rows.
    ///
    /// With two committed-and-scanned durable requirements (`keep` and `move`)
    /// in the held DB, plus a pre-existing transient
    /// `Feedback -[:Reviews]-> requirements.requirement.keep` pairing seeded
    /// into `.trans` BEFORE `apg session start` (so the start seed — not the
    /// scan — projects it into the held DB), one routed update of `move` must:
    ///
    ///   1. leave the untouched `keep` row — fqn/id/title/body/feature and the
    ///      full serialized `properties` — **byte-for-byte unchanged**, and the
    ///      `Reviews` edge and its Feedback record unchanged; and
    ///   2. move only `move`'s row: its body reflects the mutation (with its
    ///      preserved properties).
    ///
    /// The before/after captures go through the live session (routed
    /// `apg query`), so they observe the held DB's projection at admission.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn routed_mutation_touches_only_its_own_rows() {
        const KEEP_ROW: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.keep'}) \
             RETURN n.fqn, n.id, n.title, n.body, n.feature, n.properties";
        const MOVE_ROW: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.move'}) \
             RETURN n.fqn, n.id, n.title, n.body, n.feature, n.properties";
        const REVIEWS_EDGE: &str = "MATCH (f:Feedback)-[:Reviews]->\
             (n:Requirement {fqn: 'requirements.requirement.keep'}) RETURN f.fqn, n.fqn";
        const FEEDBACK_ROW: &str = "MATCH (f:Feedback {fqn: 'foo/feedback-1'}) \
             RETURN f.fqn, f.body, f.status, f.disposition";

        let (repo, wt, wt_apg) = testutil::project_with_db("mutation-isolation");
        let home = repo.root.join("home");

        // Two durable authored requirements, committed and scanned into the DB.
        // `keep` carries an arbitrary key and an empty-valued key so its
        // serialized `properties` column is non-trivial: the row must not move
        // at all.
        let mut keep = node("requirements", "requirement", "keep");
        keep.body = "keep body".to_string();
        keep.properties
            .insert("feature".to_string(), "isolation".to_string());
        keep.properties.insert("empty".to_string(), String::new());
        layers::write_node(&wt_apg, &keep).unwrap();

        let mut moved = node("requirements", "requirement", "move");
        moved.body = "before the mutation".to_string();
        moved
            .properties
            .insert("feature".to_string(), "isolation".to_string());

        // A Stakeholder requiring both requirements roots them, and a Service
        // driven-by both keeps the seeded model whole-model-valid (SPEC §8.1/§3.4).
        let mut owner = node("requirements", "stakeholder", "owner");
        owner.out = vec![
            testutil::out_edge("requires", "requirements.requirement.keep"),
            testutil::out_edge("requires", "requirements.requirement.move"),
        ];
        keep.in_edges.push(testutil::in_edge(
            "requires",
            "requirements.stakeholder.owner",
        ));
        keep.in_edges
            .push(testutil::in_edge("driven-by", "domain.service.svc"));
        moved.in_edges.push(testutil::in_edge(
            "requires",
            "requirements.stakeholder.owner",
        ));
        moved
            .in_edges
            .push(testutil::in_edge("driven-by", "domain.service.svc"));

        let mut svc = node("domain", "service", "svc");
        svc.out = vec![
            testutil::out_edge("driven-by", "requirements.requirement.keep"),
            testutil::out_edge("driven-by", "requirements.requirement.move"),
        ];
        svc.in_edges = vec![
            testutil::in_edge("contains", "domain.group.grp"),
            testutil::in_edge("realises", "solution.component.cmp"),
        ];
        let mut grp = node("domain", "group", "grp");
        grp.out = vec![testutil::out_edge("contains", "domain.service.svc")];
        let mut cmp = node("solution", "component", "cmp");
        cmp.out = vec![
            testutil::out_edge("realises", "domain.service.svc"),
            testutil::out_edge("is-role", "solution.role.rl"),
        ];
        let mut rl = node("solution", "role", "rl");
        rl.properties
            .insert("applies-to".to_string(), "component".to_string());
        rl.in_edges = vec![testutil::in_edge("is-role", "solution.component.cmp")];

        for n in [&owner, &keep, &moved, &svc, &grp, &cmp, &rl] {
            layers::write_node(&wt_apg, n).unwrap();
        }

        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable nodes");
        testutil::scan_checkout(&wt).unwrap();

        // Seed a pre-existing `Feedback -[:Reviews]-> keep` pairing into the
        // worktree's `.trans` requirements mirror BEFORE the session starts: the
        // scan above saw no mirror, so the START SEED is what projects it into
        // the held DB. Both halves of the relationship live in the one file.
        let mirror = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        specs::write_jsonl(
            &mirror,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "review of keep".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: "requirements.requirement.keep".to_string(),
                },
            ],
        )
        .unwrap();

        // Precondition (no session yet): both durable nodes are in the DB and
        // the Reviews edge is NOT — the start seed is what will add it.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.requirement.keep"));
            assert!(db.has_node("requirements.requirement.move"));
            assert_eq!(
                db.q("MATCH (:Feedback)-[:Reviews]->\
                     (:Requirement {fqn: 'requirements.requirement.keep'}) RETURN count(*)")
                    .unwrap()
                    .lines()
                    .last()
                    .map(str::trim),
                Some("0"),
                "the Reviews edge must not be in the DB before the session seed"
            );
        }

        // The start seed projects the transient Feedback + Reviews pair into the
        // held DB, before the socket is bound.
        let session = testutil::start_session_process(&wt, &home);

        // Routed reads observe the held DB's projection at admission.
        let routed = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "routed query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        // The seed really landed the edge; capture the "before" state.
        let reviews_before = routed(REVIEWS_EDGE);
        assert!(
            reviews_before.contains("foo/feedback-1")
                && reviews_before.contains("requirements.requirement.keep"),
            "the start seed must project Feedback -[:Reviews]-> keep into the \
             held DB: {reviews_before}"
        );
        let keep_before = routed(KEEP_ROW);
        let move_before = routed(MOVE_ROW);
        let feedback_before = routed(FEEDBACK_ROW);
        assert!(
            keep_before.contains("isolation") && keep_before.contains("empty"),
            "the untouched node's full properties must be present in its row: {keep_before}"
        );

        // Admit ONE routed mutation that changes ONLY `move`.
        let args = av(&[
            "update",
            "requirements",
            "requirement",
            "move",
            "--body",
            "moved body",
        ]);
        assert_eq!(
            session_forward_node(&wt_apg, "isolation-1", &args),
            "Updated node requirements.requirement.move"
        );

        // (1) The untouched identity is byte-for-byte unchanged — its full row
        // and the transient Feedback/Reviews pair seeded against it.
        let keep_after = routed(KEEP_ROW);
        let reviews_after = routed(REVIEWS_EDGE);
        let feedback_after = routed(FEEDBACK_ROW);
        assert_eq!(
            keep_after, keep_before,
            "the unaffected node's row must be byte-for-byte unchanged by the mutation"
        );
        assert_eq!(
            reviews_after, reviews_before,
            "the pre-existing Feedback -[:Reviews]-> keep edge must be unchanged"
        );
        assert_eq!(
            feedback_after, feedback_before,
            "the Feedback record must be unchanged"
        );

        // (2) Only `move` moved: its body reflects the mutation and its
        // preserved properties survive.
        let move_after = routed(MOVE_ROW);
        assert_ne!(
            move_after, move_before,
            "the mutated node's row must change"
        );
        assert!(
            move_after.contains("moved body"),
            "the mutated node's body must reflect the mutation: {move_after}"
        );
        assert!(
            !move_before.contains("moved body"),
            "the pre-mutation row must not already carry the new body: {move_before}"
        );
        assert!(
            move_after.contains("isolation"),
            "the mutated node's preserved properties must survive: {move_after}"
        );

        // Clean release: save (the single durability point) then end.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-13 (make-or-break for feedback-20): DB-only admission
    /// restores a transient `Feedback -[:Reviews]-> durable-node` pairing FROM
    /// THE HELD DB with `.trans` destroyed.
    ///
    /// A committed-and-scanned durable requirement (`reviewed`) plus a
    /// `Feedback` record and its `Reviews` edge seeded into the requirements
    /// tier mirror BEFORE `apg session start` (so the start seed — not the scan
    /// — projects the pair into the held DB). While the session is live, EVERY
    /// `.trans` plan file and tier mirror is DELETED, and a corrupt probe file
    /// is planted under `.trans/plans/*.jsonl` so that any surviving
    /// `.trans`-enumerating read at admission (`append_transient_records` /
    /// `plan_files`) would fail loud. Then:
    ///
    ///   1. A routed **update** of `reviewed` SUCCEEDS — admission performs no
    ///      `.trans` read — and the `Feedback -[:Reviews]-> reviewed` pairing is
    ///      **restored from the DB** (the update's detach drops it; the
    ///      incident-edge restore re-merges it), with the Feedback record
    ///      intact.
    ///   2. A routed **rm** of `reviewed` drops the now-dangling `Reviews` edge
    ///      (its target is gone) while the `Feedback` record survives — the
    ///      state a full rebuild from an intact `.trans` would produce.
    ///
    /// The corrupt probe is a DIFFERENT file name from the project's own
    /// `.trans/plans/foo.jsonl`, because `node rm`'s best-effort
    /// outstanding-feedback warning reads only the project's six transient
    /// files (`project_transient_files`) — a corrupt `foo.jsonl` would fail
    /// that advisory read and refuse an otherwise-valid rm. The probe still
    /// proves the admission path performs no `.trans` enumeration: the removed
    /// `append_transient_records` read globbed `.trans/plans/*.jsonl`, so it
    /// would have read the probe and failed.
    ///
    /// A rebuild-equivalence leg is deliberately omitted: with `.trans`
    /// destroyed, a full scan reads an empty transient leg and cannot re-project
    /// the Feedback record at all, so it could not reproduce the live-DB final
    /// state (Reviews gone, Feedback present). The task permits this omission
    /// when it materially complicates the test.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn admission_restores_reviews_from_the_db_with_trans_destroyed() {
        const REVIEWS_EDGE: &str = "MATCH (f:Feedback)-[:Reviews]->\
             (n:Requirement {fqn: 'requirements.requirement.reviewed'}) RETURN f.fqn, n.fqn";
        const REVIEWS_COUNT: &str = "MATCH (:Feedback)-[:Reviews]->\
             (:Requirement {fqn: 'requirements.requirement.reviewed'}) RETURN count(*)";
        const FEEDBACK_ROW: &str = "MATCH (f:Feedback {fqn: 'foo/feedback-1'}) \
             RETURN f.fqn, f.body, f.status, f.disposition";
        const REVIEWED_BODY: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.reviewed'}) \
             RETURN n.body";

        let (repo, wt, wt_apg) = testutil::project_with_db("admission-db-only-trans");
        let home = repo.root.join("home");

        // A committed durable requirement, scanned into the branch DB.
        let mut reviewed = node("requirements", "requirement", "reviewed");
        reviewed.body = "before .trans destruction".to_string();
        layers::write_node(&wt_apg, &reviewed).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed node");
        testutil::scan_checkout(&wt).unwrap();

        // Seed the Feedback + Reviews pair into the requirements tier mirror
        // BEFORE the session starts: the scan above saw no mirror, so the START
        // SEED (not the scan) is what projects it into the held DB. Both halves
        // of the relationship live in the one file.
        let mirror = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        specs::write_jsonl(
            &mirror,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "review of reviewed".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: "requirements.requirement.reviewed".to_string(),
                },
            ],
        )
        .unwrap();

        // Precondition (no session yet): the durable node is in the DB and the
        // Reviews edge is NOT — the start seed is what adds it.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.requirement.reviewed"));
            assert_eq!(
                db.q(REVIEWS_COUNT).unwrap().lines().last().map(str::trim),
                Some("0"),
                "the Reviews edge must not be in the DB before the session seed"
            );
        }

        // The start seed projects the transient Feedback + Reviews pair into the
        // held DB, before the socket is bound.
        let session = testutil::start_session_process(&wt, &home);

        // Routed reads observe the held DB's projection at admission.
        let routed = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "routed query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        let reviews_before = routed(REVIEWS_EDGE);
        assert!(
            reviews_before.contains("foo/feedback-1")
                && reviews_before.contains("requirements.requirement.reviewed"),
            "the start seed must project Feedback -[:Reviews]-> reviewed into the held DB: \
             {reviews_before}"
        );
        let feedback_before = routed(FEEDBACK_ROW);

        // Destroy `.trans` while the session is live: delete every plan file and
        // every tier mirror, then plant a corrupt probe plan file that any
        // surviving `.trans`-enumerating admission read would have to parse.
        for f in specs::plan_files(&wt_apg) {
            std::fs::remove_file(&f).unwrap();
        }
        for f in specs::trans_mirror_files(&wt_apg) {
            std::fs::remove_file(&f).unwrap();
        }
        assert!(
            !mirror.exists(),
            "the requirements tier mirror must be destroyed"
        );
        let probe = wt_apg
            .join(specs::TRANS)
            .join("plans")
            .join("zzz-corrupt-probe.jsonl");
        std::fs::create_dir_all(probe.parent().unwrap()).unwrap();
        std::fs::write(&probe, "this is not a jsonl record\n").unwrap();
        assert!(
            specs::read_jsonl(&probe).is_err(),
            "the corrupt probe must be unparseable, so any read of it fails loud"
        );

        // (1) A routed UPDATE of the reviewed node SUCCEEDS: admission reads no
        // `.trans` (the corrupt probe would fail loud if it did), and the
        // incident Reviews edge is restored FROM THE DB.
        let args = av(&[
            "update",
            "requirements",
            "requirement",
            "reviewed",
            "--body",
            "updated after .trans destroyed",
        ]);
        assert_eq!(
            session_forward_node(&wt_apg, "db-only-trans-1", &args),
            "Updated node requirements.requirement.reviewed"
        );

        let reviews_after = routed(REVIEWS_EDGE);
        assert_eq!(
            reviews_after, reviews_before,
            "the Feedback -[:Reviews]-> reviewed pairing must be restored from the DB by the update"
        );
        let feedback_after = routed(FEEDBACK_ROW);
        assert_eq!(
            feedback_after, feedback_before,
            "the Feedback record must survive the update"
        );
        let body_after = routed(REVIEWED_BODY);
        assert!(
            body_after.contains("updated after .trans destroyed"),
            "the routed update must be projected into the live DB: {body_after}"
        );

        // (2) A routed rm of the reviewed node drops the now-dangling Reviews
        // edge while the Feedback record itself survives. (`node rm`'s warning
        // reader looks only at `.trans/<project>.jsonl` — absent here — so the
        // corrupt probe, a different file name, does not trip it.)
        let rm = av(&["rm", "requirements", "requirement", "reviewed"]);
        assert_eq!(
            session_forward_node(&wt_apg, "db-only-trans-2", &rm),
            "Removed node requirements.requirement.reviewed"
        );
        assert_eq!(
            routed(REVIEWS_COUNT).lines().last().map(str::trim),
            Some("0"),
            "the dangling Reviews edge must be gone after the rm"
        );
        let feedback_after_rm = routed(FEEDBACK_ROW);
        assert!(
            feedback_after_rm.contains("foo/feedback-1")
                && feedback_after_rm.contains("review of reviewed"),
            "the Feedback record must survive the target's removal: {feedback_after_rm}"
        );

        // Clean release: save (the single durability point) then end.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-24 (a)+(b): a routed `node rm`'s outstanding-feedback
    /// warning is sourced from the held DB, so it survives the session's OWN
    /// `.trans/<project>.jsonl` mirrors being CORRUPT and then ABSENT.
    ///
    /// Two committed-and-scanned durable requirements (`reviewed-a`,
    /// `reviewed-b`) each carry a `Feedback -[:Reviews]-> node` pair seeded into
    /// the requirements tier mirror BEFORE `apg session start`, so the start seed
    /// (not the scan) projects both pairs into the held DB. While the session is
    /// live:
    ///
    ///   (a) the session's own `.trans` files are OVERWRITTEN with garbage
    ///       bytes: a routed rm of `reviewed-a` still ADMITS with the
    ///       DB-sourced warning, a routed query shows `reviewed-a` is gone, and
    ///       both corrupt files are left byte-for-byte unchanged (admission did
    ///       not read, rewrite, or delete them); and
    ///   (b) the session's own `.trans` files are ABSENT: a routed rm of the
    ///       fresh `reviewed-b` still ADMITS with the DB-sourced warning and the
    ///       files stay absent (admission did not recreate them).
    ///
    /// The corrupt plan file is a strong probe: it is a `.trans/plans/*.jsonl`
    /// file the removed per-admission plan-store enumeration
    /// (`append_transient_records` / `plan_files`) used to glob, so any
    /// surviving `.trans` read at admission would fail loud on it.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn routed_rm_warns_from_the_db_with_corrupt_and_absent_own_trans() {
        const REVIEWS_A: &str = "MATCH (f:Feedback)-[:Reviews]->\
             (n:Requirement {fqn: 'requirements.requirement.reviewed-a'}) RETURN f.fqn, n.fqn";
        const COUNT_A: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.reviewed-a'}) \
             RETURN count(n)";
        const COUNT_B: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.reviewed-b'}) \
             RETURN count(n)";
        const FEEDBACK_A: &str = "MATCH (f:Feedback {fqn: 'foo/feedback-1'}) \
             RETURN f.fqn, f.body, f.status";
        const REVIEWS_COUNT: &str = "MATCH (:Feedback)-[:Reviews]->(:Requirement) RETURN count(*)";

        let (repo, wt, wt_apg) = testutil::project_with_db("routed-rm-db-warn-trans");
        let home = repo.root.join("home");

        // Two committed durable requirements, scanned into the branch DB.
        for name in ["reviewed-a", "reviewed-b"] {
            let mut reviewed = node("requirements", "requirement", name);
            reviewed.body = format!("body of {name}");
            layers::write_node(&wt_apg, &reviewed).unwrap();
        }
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed nodes");
        testutil::scan_checkout(&wt).unwrap();

        // Seed both Feedback + Reviews pairs into the requirements tier mirror
        // BEFORE the session starts: the scan above saw no mirror, so the START
        // SEED (not the scan) projects them into the held DB. Both halves of
        // each relationship live in the one file.
        let mirror = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        specs::write_jsonl(
            &mirror,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "review of reviewed-a".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: "requirements.requirement.reviewed-a".to_string(),
                },
                Record::Feedback {
                    fqn: "foo/feedback-2".to_string(),
                    body: "review of reviewed-b".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-2".to_string(),
                    to: "requirements.requirement.reviewed-b".to_string(),
                },
            ],
        )
        .unwrap();

        // Precondition (no session yet): the durable nodes are in the DB and no
        // Reviews edge is — the start seed is what adds them.
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.requirement.reviewed-a"));
            assert!(db.has_node("requirements.requirement.reviewed-b"));
            assert_eq!(
                db.q(REVIEWS_COUNT).unwrap().lines().last().map(str::trim),
                Some("0"),
                "no Reviews edge may be in the DB before the session seed"
            );
        }

        // The start seed projects the transient Feedback + Reviews pairs into
        // the held DB, before the socket is bound.
        let session = testutil::start_session_process(&wt, &home);

        // Routed reads observe the held DB's projection at admission.
        let routed = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "routed query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        // The seed really landed the edge; capture the "before" state.
        let reviews_before = routed(REVIEWS_A);
        assert!(
            reviews_before.contains("foo/feedback-1")
                && reviews_before.contains("requirements.requirement.reviewed-a"),
            "the start seed must project Feedback -[:Reviews]-> reviewed-a into the held DB: \
             {reviews_before}"
        );

        // (a) Corrupt the session's OWN `.trans/<project>.jsonl` files: the
        // requirements mirror that carries the feedback, and the plan file.
        // Capture the exact bytes so the test can prove admission touched
        // neither.
        let corrupt_mirror: Vec<u8> = b"\x00not a jsonl record\x01\nfor sure not jsonl\n".to_vec();
        let corrupt_plan: Vec<u8> = b"garbage plan bytes {[(]} not jsonl\n".to_vec();
        let plan_mirror = specs::plan_jsonl_path(&wt_apg, "foo");
        std::fs::write(&mirror, &corrupt_mirror).unwrap();
        std::fs::create_dir_all(plan_mirror.parent().unwrap()).unwrap();
        std::fs::write(&plan_mirror, &corrupt_plan).unwrap();

        // A routed rm of `reviewed-a` ADMITS: the warning is sourced from the
        // held DB, not the corrupt mirror.
        let rm_a =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed-a"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            rm_a.status.success(),
            "a routed rm with corrupt own .trans must still admit: {}",
            String::from_utf8_lossy(&rm_a.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&rm_a.stdout).trim(),
            "Removed node requirements.requirement.reviewed-a"
        );
        let stderr_a = String::from_utf8_lossy(&rm_a.stderr);
        assert!(
            stderr_a.contains("apg: warning: removing `requirements.requirement.reviewed-a`"),
            "the DB-sourced warning must reach the client's stderr: {stderr_a}"
        );
        assert!(
            stderr_a.contains("foo/feedback-1 (open)"),
            "the warning must name reviewed-a's outstanding feedback: {stderr_a}"
        );

        // (a)(i) The mutation was actually buffered/admitted: a routed query
        // through the live session shows the removed node is gone.
        assert_eq!(
            routed(COUNT_A).lines().last().map(str::trim),
            Some("0"),
            "the routed rm must be projected into the live DB"
        );

        // (a)(ii) The corrupt own-.trans files were left exactly as they were,
        // byte-for-byte: admission did not read, rewrite, or delete them.
        assert_eq!(
            std::fs::read(&mirror).unwrap(),
            corrupt_mirror,
            "admission must leave the corrupt tier mirror byte-for-byte unchanged"
        );
        assert_eq!(
            std::fs::read(&plan_mirror).unwrap(),
            corrupt_plan,
            "admission must leave the corrupt plan file byte-for-byte unchanged"
        );

        // (b) The session's own `.trans` files are ABSENT: a routed rm of the
        // fresh `reviewed-b` still admits with the DB-sourced warning, and the
        // absent files stay absent.
        std::fs::remove_file(&mirror).unwrap();
        std::fs::remove_file(&plan_mirror).unwrap();
        assert!(!mirror.exists() && !plan_mirror.exists());

        let rm_b =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed-b"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            rm_b.status.success(),
            "a routed rm with absent own .trans must still admit: {}",
            String::from_utf8_lossy(&rm_b.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&rm_b.stdout).trim(),
            "Removed node requirements.requirement.reviewed-b"
        );
        let stderr_b = String::from_utf8_lossy(&rm_b.stderr);
        assert!(
            stderr_b.contains("apg: warning: removing `requirements.requirement.reviewed-b`"),
            "the DB-sourced warning must reach the client's stderr: {stderr_b}"
        );
        assert!(
            stderr_b.contains("foo/feedback-2 (open)"),
            "the warning must name reviewed-b's outstanding feedback: {stderr_b}"
        );
        assert_eq!(
            routed(COUNT_B).lines().last().map(str::trim),
            Some("0"),
            "the routed rm must be projected into the live DB"
        );
        assert!(
            !mirror.exists() && !plan_mirror.exists(),
            "admission must not recreate the absent .trans files"
        );

        // The Feedback records themselves survive (they are authoritative), so
        // `apg review list` still reports the removed targets.
        assert!(
            routed(FEEDBACK_A).contains("foo/feedback-1"),
            "the Feedback record must survive the target's removal"
        );

        // Clean release: save (the single durability point) then end.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-24 (c): a routed `node rm`'s DB-sourced warning is SCOPED
    /// to the session's own project, matching the direct path's
    /// `specs::project_transient_files(project)` scope.
    ///
    /// A committed-and-scanned durable requirement (`reviewed`) carries TWO
    /// `Feedback -[:Reviews]-> reviewed` pairings seeded into `.trans` before
    /// `apg session start`: one in the session's OWN project (`foo/feedback-1`,
    /// from `.trans/requirements/foo.jsonl`) and one in a SECOND project
    /// (`other/feedback-1`, from `.trans/requirements/other.jsonl`). The start
    /// seed loads BOTH into the held DB. Then a routed rm of `reviewed` must:
    ///
    ///   1. NOT name `other/feedback-1` — the admission read is scoped to the
    ///      session's project (`<project>/…`); and
    ///   2. yield a warning that MATCHES, byte-for-byte, the warning
    ///      `node_cmd::build_change` (the direct-path builder over the INTACT
    ///      `.trans`) yields for the same node — the same outstanding-feedback
    ///      warning, not merely the absence of the other project's feedback.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn routed_rm_feedback_warning_is_scoped_to_the_session_project() {
        const REVIEWED: &str = "requirements.requirement.reviewed";
        const BOTH_REVIEWS: &str = "MATCH (f:Feedback)-[:Reviews]->\
             (n:Requirement {fqn: 'requirements.requirement.reviewed'}) RETURN f.fqn ORDER BY f.fqn";

        let (repo, wt, wt_apg) = testutil::project_with_db("routed-rm-feedback-scope");
        let home = repo.root.join("home");

        // A committed durable requirement, scanned into the branch DB.
        let mut reviewed = node("requirements", "requirement", "reviewed");
        reviewed.body = "scoped review".to_string();
        layers::write_node(&wt_apg, &reviewed).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed node");
        testutil::scan_checkout(&wt).unwrap();

        // Project `foo`'s Feedback + Reviews pair (the session's own project).
        let foo_mirror = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        specs::write_jsonl(
            &foo_mirror,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "foo review".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: REVIEWED.to_string(),
                },
            ],
        )
        .unwrap();
        // A SECOND project's Feedback + Reviews pair reviewing the SAME node.
        let other_mirror = specs::transient_feedback_path(&wt_apg, "other", Layer::Requirements);
        specs::write_jsonl(
            &other_mirror,
            &[
                Record::Feedback {
                    fqn: "other/feedback-1".to_string(),
                    body: "other review".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "other/feedback-1".to_string(),
                    to: REVIEWED.to_string(),
                },
            ],
        )
        .unwrap();

        // The start seed loads ALL projects' feedback into the held DB.
        let session = testutil::start_session_process(&wt, &home);

        let routed = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "routed query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        // Both projects' Reviews edges are in the held DB — the scope, not the
        // seed, is what must exclude `other` from the warning.
        let both = routed(BOTH_REVIEWS);
        assert!(
            both.contains("foo/feedback-1") && both.contains("other/feedback-1"),
            "the start seed must load both projects' feedback into the held DB: {both}"
        );

        // The direct-path builder over the INTACT `.trans` yields exactly one
        // warning, scoped to `foo` by `project_transient_files(apg_root, "foo")`.
        let args = av(&["rm", "requirements", "requirement", "reviewed"]);
        let direct = build_change(&wt_apg, "node", &args).unwrap();
        assert_eq!(
            direct.warnings.len(),
            1,
            "the direct builder must yield exactly one warning: {:?}",
            direct.warnings
        );
        let direct_warning = direct.warnings[0].as_str();
        assert!(
            direct_warning.contains("foo/feedback-1 (open)"),
            "the direct builder's warning must name the session project's feedback: {direct_warning}"
        );
        assert!(
            !direct_warning.contains("other"),
            "the direct builder must not name another project's feedback: {direct_warning}"
        );

        // The routed rm admits with the DB-sourced warning; it must MATCH the
        // direct builder's warning byte-for-byte and exclude `other`.
        let out =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            out.status.success(),
            "the routed rm must admit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Removed node requirements.requirement.reviewed"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("other/feedback-1"),
            "the routed warning must not name another project's feedback: {stderr}"
        );
        let routed_warning = stderr
            .lines()
            .find(|l| l.starts_with("apg: warning:"))
            .expect("the routed rm must print its outstanding-feedback warning");
        assert_eq!(
            routed_warning, direct_warning,
            "the routed warning must match the direct builder's byte-for-byte"
        );

        // Clean release: save (the single durability point) then end.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-24 (d), updated for the phase-04 project-context guard and
    /// trimmed by project-context-guard phase-07.task-8: a DETACHED HEAD is not
    /// a project context, so the CLI refuses the rm, and the direct-path builder
    /// (`node_cmd::build_change`) over the intact `.trans` warns on NOTHING —
    /// exactly the `git::repo_identity(...).branch` `None` case.
    ///
    /// A committed-and-scanned durable requirement (`reviewed`) plus a
    /// `Feedback -[:Reviews]-> reviewed` pair is sealed into `.trans` before the
    /// direct-path builder runs, so the builder sees the outstanding feedback.
    /// HEAD is then detached at the scanned commit.
    ///
    /// COVERAGE NOTE: the routed half is unreachable by construction once a live
    /// session requires a project context — `apg session start` on a detached
    /// checkout now refuses before binding, so no detached-HEAD session exists
    /// to forward a mutation to. The `Some(project)` warning path (the shape that
    /// warns about a same-project outstanding feedback) stays covered by
    /// `routed_rm_feedback_warning_is_scoped_to_the_session_project`.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn detached_head_rm_refuses_context_and_the_direct_path_warns_on_nothing() {
        let (repo, wt, wt_apg) = testutil::project_with_db("routed-rm-detached-head");
        let home = repo.root.join("home");

        // A committed durable requirement, scanned into the branch DB.
        let mut reviewed = node("requirements", "requirement", "reviewed");
        reviewed.body = "detached head".to_string();
        layers::write_node(&wt_apg, &reviewed).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed node");
        testutil::scan_checkout(&wt).unwrap();

        // Seed the Feedback + Reviews pair into `.trans` so the direct-path
        // builder sees the outstanding feedback.
        let mirror = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        specs::write_jsonl(
            &mirror,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "review of reviewed".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: "requirements.requirement.reviewed".to_string(),
                },
            ],
        )
        .unwrap();

        // Detach HEAD at the scanned commit, so `repo_identity` yields no branch.
        {
            let git_repo = git2::Repository::open(&wt).unwrap();
            let head = git_repo.head().unwrap().peel_to_commit().unwrap().id();
            git_repo.set_head_detached(head).unwrap();
            assert!(
                git_repo.head_detached().unwrap(),
                "HEAD must be detached for this scenario"
            );
            assert!(
                apg::git::repo_identity(&wt_apg).unwrap().branch.is_none(),
                "a detached HEAD must resolve to no project"
            );
        }

        // The direct-path builder over the intact `.trans` warns on nothing when
        // HEAD is detached (branch None).
        let args = av(&["rm", "requirements", "requirement", "reviewed"]);
        assert!(
            build_change(&wt_apg, "node", &args)
                .unwrap()
                .warnings
                .is_empty(),
            "a detached-HEAD direct-path builder must warn on nothing"
        );

        // The project-context guard: a detached checkout holds no branch, so
        // the CLI client refuses the rm — never the session requirement or a
        // missing node.
        let out =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            !out.status.success(),
            "a detached-HEAD checkout must refuse the mutation"
        );
        let msg = String::from_utf8_lossy(&out.stderr);
        let refusal = msg.strip_prefix("apg: ").unwrap_or(&msg);
        assert!(refusal.starts_with("mutation refused — "), "{msg}");
        assert!(msg.contains("holds no branch"), "{msg}");
        assert!(msg.contains("apg project start"), "{msg}");
        assert!(!msg.contains("apg session start"), "{msg}");

        testutil::remove(&repo);
    }

    /// Phase-05 task-17 (1): a CORRUPT `.trans` plan file refuses `apg session
    /// start` before any socket is bound, naming the offending file and the
    /// remedy, and leaves the held `db.lbug` untouched.
    ///
    /// `project_with_db` leaves `.trans` with only `db.lbug`/`graph.jsonl`, so a
    /// single malformed `.trans/plans/foo.jsonl` planted before start is the
    /// only transient file the start-time seed enumerates. The seed reads it via
    /// `specs::read_jsonl`, which fails loud, and — because the seed runs BEFORE
    /// `UnixListener::bind` and its apply transaction — the refusal:
    ///
    ///   1. exits non-zero with stderr naming the file and the remedy;
    ///   2. leaves no socket bound and no live session; and
    ///   3. leaves the held DB logically unchanged — its full durable
    ///      node-file reconstruction is identical and no partial transient row
    ///      leaked. (A byte comparison is unusable: lbug rewrites `db.lbug` on
    ///      open, so the test snapshots the DB's logical state instead.)
    ///   4. releases the extended `specs.lock` flock it acquired before the seed
    ///      — proven cross-process by removing the corrupt file and bringing up
    ///      a second `apg session start`, which would block on the flock (and
    ///      time out) if the refused start still held it.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_start_refuses_corrupt_trans_seed_and_leaves_db_unchanged() {
        const COUNT_SEEDED: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.seeded'}) \
             RETURN count(n)";

        let (repo, wt, wt_apg) = testutil::project_with_db("session-corrupt-trans-seed");
        let home = repo.root.join("home");

        // A committed durable requirement, scanned into the branch DB — the
        // pre-existing node set the refused seed must leave untouched.
        let mut seeded = node("requirements", "requirement", "seeded");
        seeded.body = "seed body".to_string();
        layers::write_node(&wt_apg, &seeded).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable node");
        testutil::scan_checkout(&wt).unwrap();

        let socket = apg::session::socket_path(&wt_apg);

        // Precondition: the durable node is in the DB, no transient Feedback
        // row exists, and no session socket exists. Capture the full durable
        // node-file set reconstructed from the DB — the strongest logical
        // snapshot the refused seed must leave unchanged.
        let durable_before;
        {
            let opened = ArtifactDb::open(&wt_apg).unwrap();
            assert_eq!(
                opened
                    .q(COUNT_SEEDED)
                    .unwrap()
                    .lines()
                    .last()
                    .map(str::trim),
                Some("1"),
                "the pre-existing durable node must be in the DB before the refusal"
            );
            assert_eq!(
                opened
                    .q("MATCH (f:Feedback) RETURN count(*)")
                    .unwrap()
                    .lines()
                    .last()
                    .map(str::trim),
                Some("0"),
                "no transient Feedback row may exist before the seed"
            );
            durable_before = opened.node_files_from_db().unwrap();
        }
        assert!(
            !socket.exists(),
            "the fixture must start with no session socket"
        );

        // A malformed plan file planted BEFORE start: the start-time seed
        // enumerates `.trans/plans/*.jsonl` and reads it, so it must fail closed.
        let plan_file = specs::plan_jsonl_path(&wt_apg, "foo");
        std::fs::create_dir_all(plan_file.parent().unwrap()).unwrap();
        std::fs::write(&plan_file, "garbage plan bytes {[(]} not jsonl\n").unwrap();

        // `apg session start` must exit non-zero rather than bind and serve.
        let refused = testutil::ApgCommand::new(&["session", "start"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !refused.status.success(),
            "a corrupt .trans plan file must refuse `apg session start`: {}",
            String::from_utf8_lossy(&refused.stdout)
        );

        // The refusal names the offending file AND the remedy.
        let err = String::from_utf8_lossy(&refused.stderr);
        assert!(
            err.contains("foo.jsonl"),
            "the refusal must name the offending file: {err}"
        );
        assert!(
            err.contains("repair or remove"),
            "the refusal must name the repair/remove remedy: {err}"
        );
        assert!(
            err.contains("apg scan"),
            "the refusal must name `apg scan` as the re-projection remedy: {err}"
        );

        // No socket bound, no live session.
        assert!(
            !socket.exists(),
            "a refused start must leave no session socket behind"
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "a refused start leaves no live session"
        );

        // The held DB is logically unchanged by the refused seed: its full
        // durable node-file reconstruction is identical, and no partial
        // transient row leaked (the seed reads every file before it applies and
        // applies through one transaction that rolls back on error).
        let opened = ArtifactDb::open(&wt_apg).unwrap();
        assert_eq!(
            opened.node_files_from_db().unwrap(),
            durable_before,
            "the refused start must leave the held DB's durable node-file set unchanged"
        );
        assert_eq!(
            opened
                .q(COUNT_SEEDED)
                .unwrap()
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the pre-existing durable node set must survive the refused seed"
        );
        assert_eq!(
            opened
                .q("MATCH (f:Feedback) RETURN count(*)")
                .unwrap()
                .lines()
                .last()
                .map(str::trim),
            Some("0"),
            "the refused seed must not leave a partial transient row"
        );
        drop(opened);

        // (4) The refused start must also have RELEASED the extended
        // `specs.lock` flock. It takes that flock before the DB open and the
        // seed; because the local `SpecLockGuard` drops on the error path, the
        // fd closes and the flock frees when the refusing process exits. The
        // strongest proof is cross-process: remove the corrupt file so a fresh
        // start's seed is a clean no-op, then bring up a second `apg session
        // start` on the SAME worktree. If the refused start still held the
        // flock, `flock(LOCK_EX)` would block and `start_session_process` would
        // time out; a live second session proves the release.
        std::fs::remove_file(&plan_file).unwrap();
        assert!(
            !plan_file.exists(),
            "the corrupt plan file must be removed before the clean restart"
        );
        assert!(
            specs::plan_files(&wt_apg).is_empty(),
            "no plan file may remain for the fresh start's seed to choke on"
        );

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "a second start must succeed once the refused start released the specs.lock flock"
        );

        // The restarted session owns the held DB and serves a routed read.
        let routed = spawn_apg(&["query", COUNT_SEEDED], &wt);
        assert!(
            routed.status.success(),
            "a routed read through the restarted session must succeed: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8(routed.stdout)
                .unwrap()
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the restarted session must serve the durable node set"
        );

        // End the restarted session cleanly: save (the single durability point,
        // a no-op on the untouched buffer) then end, and confirm no session
        // lingers.
        let save = spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the restarted session must end cleanly"
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-17 (2): ABSENT `.trans` plan files leave `apg session
    /// start` a clean no-op — the seed enumerates only EXISTING files, so with
    /// no `plans/*.jsonl` and no tier mirrors start succeeds normally and the
    /// live session serves a routed read and a routed mutation.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_start_with_absent_trans_seed_is_a_clean_noop() {
        const COUNT_CREATED: &str = "MATCH (n:Constraint {fqn: 'global.constraint.created'}) \
             RETURN count(n)";

        let (repo, wt, wt_apg) = testutil::project_with_db("session-absent-trans-seed");
        let home = repo.root.join("home");

        // The seed's enumerators see only existing files: this fixture carries
        // no plan file and no tier mirror, so the seed is an empty no-op.
        assert!(
            specs::plan_files(&wt_apg).is_empty(),
            "the fixture must start with no .trans plan files"
        );
        assert!(
            specs::trans_mirror_files(&wt_apg).is_empty(),
            "the fixture must start with no .trans tier mirrors"
        );

        // Absent `.trans` must not refuse start: the session comes up and serves.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "absent .trans plan files must leave start a clean no-op"
        );

        // A routed read observes the held DB.
        let routed = |q: &str| -> String {
            let out = spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "routed query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(
            routed(COUNT_CREATED).lines().last().map(str::trim),
            Some("0"),
            "the fixture must start with the node absent"
        );

        // A routed mutation is admitted and immediately visible to a routed read.
        let added = spawn_apg(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "created",
                "--body",
                "created through a session started without .trans",
            ],
            &wt,
        );
        assert!(
            added.status.success(),
            "a routed node add must succeed: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&added.stdout).trim(),
            "Added node global.constraint.created"
        );
        assert_eq!(
            routed(COUNT_CREATED).lines().last().map(str::trim),
            Some("1"),
            "the routed mutation must be visible through a routed read"
        );

        // Clean release: save (the single durability point) then end.
        let save = spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-3 (E2E): on the MAIN checkout with no live session, every
    /// node mutation refuses for project context — BEFORE the live-session
    /// requirement and before any existence resolution — so `node add`,
    /// `node update` and `node rm` never read as `apg session start` or an
    /// absent-node error, and no node file or commit lands.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn node_mutations_on_main_refuse_context_before_session_and_existence() {
        let repo = Repo::new("node-guard-main");
        let apg_root = repo.apg_root();
        let before_commits = testutil::commit_count(&repo.root);

        for argv in [
            vec![
                "node",
                "add",
                "requirements",
                "requirement",
                "fresh",
                "--body",
                "must not land",
            ],
            vec![
                "node",
                "update",
                "requirements",
                "requirement",
                "ghost",
                "--body",
                "absent",
            ],
            vec!["node", "rm", "requirements", "requirement", "ghost"],
        ] {
            let out = testutil::spawn_apg(&argv, &repo.root);
            assert_refused_on_main(&out, &argv, &["apg session start", "does not exist"]);
        }

        assert_no_durable_trace(&apg_root, &repo.root, before_commits);
        testutil::remove(&repo);
    }

    /// Phase-04 task-4 (E2E): on the MAIN checkout with no live session, every
    /// edge mutation refuses for project context BEFORE either endpoint is
    /// resolved — so `edge add`, `edge update` and `edge rm` over endpoints
    /// absent from that DB never report the absent node/edge, and no node file
    /// or commit lands.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn edge_mutations_on_main_refuse_context_before_endpoint_resolution() {
        let repo = Repo::new("edge-guard-main");
        let apg_root = repo.apg_root();
        let before_commits = testutil::commit_count(&repo.root);

        for argv in [
            vec![
                "edge",
                "add",
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
            ],
            vec![
                "edge",
                "update",
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
                "--property",
                "note=1",
            ],
            vec![
                "edge",
                "rm",
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
            ],
        ] {
            let out = testutil::spawn_apg(&argv, &repo.root);
            assert_refused_on_main(&out, &argv, &["apg session start", "does not exist"]);
        }

        assert_no_durable_trace(&apg_root, &repo.root, before_commits);
        testutil::remove(&repo);
    }

    /// Phase-04 task-5, repaired by project-context-guard phase-07.task-7: the
    /// live-session-in-main premise is now impossible, so the pair proven here
    /// is the START refusal plus the CLIENT refusal.
    ///
    /// `apg session start` on the scanned main checkout exits non-zero with the
    /// project-context refusal (naming branch `main` and the `apg project start`
    /// fix, never `already live`), binds no socket and leaves no live session.
    /// Then the same six absent-target node/edge mutations still refuse on the
    /// CLIENT for project context — never the session requirement or a missing
    /// node — with `apg/layers` untouched and no commit.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn main_session_start_and_mutations_refuse_project_context() {
        let repo = Repo::new("node-guard-main");
        let apg_root = repo.apg_root();
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        testutil::scan_checkout(&repo.root).unwrap();
        let home = repo.root.join("home");

        // The start refusal: no live session is reachable on main.
        let refused = testutil::ApgCommand::new(&["session", "start"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert_refused_on_main(
            &refused,
            &["session", "start"],
            &["already live", "apg scan"],
        );
        assert!(
            !apg::session::socket_path(&apg_root).exists(),
            "a refused start on main must bind no socket"
        );
        assert!(
            !apg::session::live_session(&apg_root),
            "a refused start leaves no live session"
        );

        let before_commits = testutil::commit_count(&repo.root);

        for argv in [
            vec![
                "node",
                "add",
                "requirements",
                "requirement",
                "fresh",
                "--body",
                "must not land",
            ],
            vec![
                "node",
                "update",
                "requirements",
                "requirement",
                "ghost",
                "--body",
                "absent",
            ],
            vec!["node", "rm", "requirements", "requirement", "ghost"],
            vec![
                "edge",
                "add",
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
            ],
            vec![
                "edge",
                "update",
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
                "--property",
                "note=1",
            ],
            vec![
                "edge",
                "rm",
                "contains",
                "requirements.requirement.a",
                "requirements.requirement.b",
            ],
        ] {
            let out = testutil::ApgCommand::new(&argv)
                .cwd(&repo.root)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert_refused_on_main(&out, &argv, &["apg session start", "does not exist"]);
        }

        assert_no_durable_trace(&apg_root, &repo.root, before_commits);
        testutil::remove(&repo);
    }

    /// project-context-guard phase-07.task-5 (E2E): the two checkouts of one
    /// repo have distinct session sockets, so a session started in the project
    /// worktree is the one ITS checkout's durable mutations reach.
    ///
    /// An `apg node add` spawned with cwd inside the worktree (no directory
    /// override) is admitted by the live worktree session — its row is visible
    /// to a routed `apg query` before any save — and `apg session save` lands
    /// exactly one commit. The SAME mutation from the main checkout refuses
    /// client-side for project context (never `apg session start`), and a
    /// following save over the now-clean buffer adds no commit: the main
    /// checkout can never reach the worktree session.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/db/process); run via cargo test-e2e"]
    fn worktree_session_admits_its_checkout_and_main_never_reaches_it() {
        const COUNT: &str =
            "MATCH (n:Constraint {fqn: 'global.constraint.fresh-rule'}) RETURN count(n)";

        let (wt_apg, repo, wt) = mutation_fixture("worktree-session-scope");
        let home = repo.root.join("home");

        // A whole-model-valid durable base; a lone `global.constraint` has no
        // cardinality minimums.
        layers::write_project(&wt_apg, &testutil::valid_model(), &[]).unwrap();

        assert_ne!(
            apg::session::socket_path(&wt_apg),
            apg::session::socket_path(&repo.apg_root()),
            "the two checkouts must have distinct session sockets"
        );

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the project worktree's session must be live"
        );
        let before_commits = testutil::commit_count(&wt);

        let argv = [
            "node",
            "add",
            "global",
            "constraint",
            "fresh-rule",
            "--body",
            "admitted by the worktree session",
        ];
        let out = testutil::ApgCommand::new(&argv)
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            out.status.success(),
            "the worktree mutation must be admitted: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The admission projected the row into the held DB, visible to a routed
        // read before any save; the durable store and history are untouched.
        let routed = testutil::spawn_apg(&["query", COUNT], &wt);
        assert!(
            routed.status.success(),
            "routed query: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert!(
            String::from_utf8_lossy(&routed.stdout).contains('1'),
            "the buffered row must be visible through the session's DB: {}",
            String::from_utf8_lossy(&routed.stdout)
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "fresh-rule").exists(),
            "an admitted mutation must not write a node file before save"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "an admitted mutation must not commit before save"
        );

        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the admitted mutation must land in exactly one commit"
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "fresh-rule").exists(),
            "the node file must be durable after save"
        );

        // The SAME mutation from the main checkout refuses client-side, and can
        // never reach the worktree session: a following save adds no commit.
        let main_out = testutil::spawn_apg(&argv, &repo.root);
        assert_refused_on_main(&main_out, &argv, &["apg session start"]);
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "{}",
            String::from_utf8_lossy(&save.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the main-checkout refusal must never route to the worktree session"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "the session must be ended cleanly"
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-6 (E2E): inside the PROJECT worktree on the project branch
    /// the guard admits a genuine project context, so all six mutations —
    /// `node add`/`update`/`rm` and `edge add`/`update`/`rm` — still succeed
    /// through ONE live session and land durably (one commit) on
    /// `apg session save`; the mandatory-session requirement is unchanged.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn all_six_node_and_edge_mutations_save_through_one_session_in_the_worktree() {
        let (wt_apg, repo, wt) = mutation_fixture("guard-six-mutations");
        let home = repo.root.join("home");

        // A whole-model-valid durable base plus one isolated `global.constraint`
        // for `node rm` (a lone constraint has no cardinality minimums).
        let mut base = testutil::valid_model();
        base.push(node("global", "constraint", "retire"));
        layers::write_project(&wt_apg, &base, &[]).unwrap();

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "the six mutations must run under a live session"
        );
        let before_commits = testutil::commit_count(&wt);

        let fresh_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "fresh");
        let retire_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "retire");
        assert!(!fresh_path.exists(), "the fixture must start without fresh");
        assert!(retire_path.exists(), "the fixture must carry retire");

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

        // The new requirement's two structural edges make it whole-model-valid
        // (rooted through `place-order`'s contains decomposition, driven-by the
        // Service) before the save boundary.
        run(
            &["node", "add", "requirements", "requirement", "fresh"],
            "Added node requirements.requirement.fresh",
        );
        run(
            &[
                "edge",
                "add",
                "contains",
                "requirements.requirement.place-order",
                "requirements.requirement.fresh",
            ],
            "Added edge contains requirements.requirement.place-order -> requirements.requirement.fresh",
        );
        run(
            &[
                "edge",
                "add",
                "driven-by",
                "domain.service.place-order",
                "requirements.requirement.fresh",
            ],
            "Added edge driven-by domain.service.place-order -> requirements.requirement.fresh",
        );
        run(
            &[
                "node",
                "update",
                "requirements",
                "requirement",
                "fresh",
                "--body",
                "fresh body",
            ],
            "Updated node requirements.requirement.fresh",
        );
        run(
            &[
                "edge",
                "update",
                "contains",
                "requirements.requirement.place-order",
                "requirements.requirement.fresh",
                "--property",
                "note=1",
            ],
            "Updated edge contains requirements.requirement.place-order -> requirements.requirement.fresh",
        );
        // `reads` is optional (unlike `writes`), so its removal keeps the model
        // valid.
        run(
            &[
                "edge",
                "rm",
                "reads",
                "domain.service.place-order",
                "domain.entity.order",
            ],
            "Removed edge reads domain.service.place-order -> domain.entity.order",
        );
        run(
            &["node", "rm", "global", "constraint", "retire"],
            "Removed node global.constraint.retire",
        );

        // Every mutation is BUFFERED: the durable store and the history are
        // untouched until the single durability point.
        assert!(!fresh_path.exists(), "a buffered add must not write a file");
        assert!(retire_path.exists(), "a buffered rm must not delete a file");
        let svc = layers::read_node_file(&wt_apg, Layer::Domain, "service", "place-order").unwrap();
        assert!(
            svc.out.iter().any(|e| e.kind == "reads"),
            "a buffered edge rm must not drop the out-half"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a buffered mutation must not commit"
        );

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
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the whole buffered set must land in exactly one commit"
        );

        // The durable effects of all six verbs.
        let fresh =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "fresh").unwrap();
        assert_eq!(fresh.body, "fresh body", "the node update landed");
        assert!(!retire_path.exists(), "the node rm landed durably on save");
        let place_order =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "place-order")
                .unwrap();
        let contains = place_order
            .out
            .iter()
            .find(|e| e.kind == "contains" && e.target == "requirements.requirement.fresh")
            .expect("the added contains out-half must be durable");
        assert_eq!(
            contains.properties.get("note").map(String::as_str),
            Some("1"),
            "the edge update merge landed on the out-half"
        );
        assert!(
            fresh.in_edges.iter().any(|e| {
                e.kind == "contains"
                    && e.source == "requirements.requirement.place-order"
                    && e.properties.get("note").map(String::as_str) == Some("1")
            }),
            "the added edge's in-half carries the identical merged map"
        );
        assert!(
            fresh
                .in_edges
                .iter()
                .any(|e| e.kind == "driven-by" && e.source == "domain.service.place-order"),
            "the added driven-by edge is durable"
        );
        let svc = layers::read_node_file(&wt_apg, Layer::Domain, "service", "place-order").unwrap();
        assert!(
            !svc.out.iter().any(|e| e.kind == "reads"),
            "the edge rm dropped the out-half durably"
        );
        let order = layers::read_node_file(&wt_apg, Layer::Domain, "entity", "order").unwrap();
        assert!(
            !order
                .in_edges
                .iter()
                .any(|e| { e.kind == "reads" && e.source == "domain.service.place-order" }),
            "the edge rm dropped the in-half durably"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        testutil::remove(&repo);
    }

    /// Phase-04 task-12 (session-start-recovery): with **no session socket**,
    /// `apg session start` classifies the database it is about to own and, when
    /// it carries pending state — a dirty durable row
    /// ([`apg::artifacts::db::db_has_pending_state`]) or a pending-removal
    /// ledger identity — discards it and rebuilds it from the durable
    /// `apg/layers/**` node files BEFORE it admits anything; a clean,
    /// shape-compatible database starts without a scan.
    ///
    /// Three starts over one scratch worktree, no socket before any of them:
    ///
    /// 1. an admitted-but-unsaved durable row plus a dirty mark on a committed
    ///    one — rebuilt, so the unsaved row is never served and the committed
    ///    pair is;
    /// 2. only a phantom `PendingRemoval` ledger identity — rebuilt, so the
    ///    ledger drains yet the named committed requirement survives (the
    ///    rebuild sources the node files, not the discarded index);
    /// 3. a clean, shape-compatible database — starts without a scan and never
    ///    prints the rebuild line.
    ///
    /// The rebuild touches only derived artifacts: `apg/layers/**` stays
    /// byte-identical to its pre-injection snapshot and no commit lands.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn session_start_discards_a_pending_state_database_with_no_socket() {
        // The recovery classification `reclaim_stale_socket` prints.
        const REBUILD: &str = "carries pending state — rebuilding it from apg/layers/**";
        const UNSAVED: &str = "requirements.requirement.unsaved";
        const KEEP_A: &str = "requirements.requirement.keep-a";
        const KEEP_B: &str = "requirements.requirement.keep-b";

        let (wt_apg, repo, wt) = node_store_fixture("session-start-pending");
        let home = repo.root.join("home");

        // The durable system of record: two committed node files, projected
        // into a genuinely fresh `db.lbug` — the rebuild's only source.
        layers::write_project(
            &wt_apg,
            &[
                node("requirements", "requirement", "keep-a"),
                node("requirements", "requirement", "keep-b"),
            ],
            &[],
        )
        .unwrap();
        testutil::scan_checkout(&wt).unwrap();

        let socket = apg::session::socket_path(&wt_apg);
        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();
        let before_commits = testutil::commit_count(&wt);
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the baseline DB must be fresh before anything is injected"
        );
        assert!(
            !socket.exists(),
            "the fixture must start with no session socket"
        );

        let last = |out: &str| {
            out.lines()
                .last()
                .map(str::trim)
                .unwrap_or_default()
                .to_string()
        };
        // A routed read goes through the live session's held DB.
        let routed = |q: &str| -> String {
            let out = spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "routed query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        let csv_rows = |csv: &str| -> Vec<String> {
            csv.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        };
        let committed_rows = |csv: &str| -> Vec<String> {
            assert_eq!(
                csv_rows(csv),
                vec!["n.fqn".to_string(), KEEP_A.to_string(), KEEP_B.to_string()],
                "the session must serve exactly the two committed requirements: {csv}"
            );
            assert!(
                !csv.contains("unsaved"),
                "the session must never serve the unsaved row: {csv}"
            );
            csv_rows(csv)
        };

        // ---- (1) DIRTY ROW + UNSAVED ROW, no socket ----------------------
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            db.conn()
                .unwrap()
                .query(&format!(
                    "MATCH (n:Requirement {{fqn: '{KEEP_A}'}}) SET n.updated = 1"
                ))
                .unwrap();
            db.conn()
                .unwrap()
                .query(&format!(
                    "CREATE (n:Requirement {{fqn: '{UNSAVED}', body: 'admitted-but-unsaved', updated: 1}})"
                ))
                .unwrap();
        }
        assert!(
            apg::artifacts::db_has_pending_state(&wt_apg),
            "the fixture must carry the injected dirty + unsaved rows"
        );
        assert!(!apg::git::is_fresh(&wt_apg), "pending state is never fresh");
        assert!(!socket.exists(), "the recovering start gets no socket");

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "a pending-state start must rebuild and then serve"
        );
        let rows = routed("MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn");
        committed_rows(&rows);
        assert_eq!(
            last(&routed(
                "MATCH (n:Requirement) WHERE n.updated = 1 RETURN count(*)"
            )),
            "0",
            "the rebuilt DB must leave no dirty requirement row"
        );
        assert_eq!(
            last(&routed("MATCH (n:PendingRemoval) RETURN count(*)")),
            "0",
            "the rebuilt DB must leave no pending-removal ledger identity"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "unsaved")
                .exists(),
            "the unsaved row never had a node file, and the rebuild writes none"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(REBUILD),
            "start must classify the dirty row as pending state and rebuild: {stderr}"
        );
        assert!(!socket.exists(), "end must release the session socket");
        assert!(
            !apg::session::live_session(&wt_apg),
            "the recovered session must end cleanly"
        );
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the rebuilt DB must carry no pending state"
        );

        // ---- (2) LEDGER IDENTITY ONLY, no socket -------------------------
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            db.record_pending_removal(KEEP_B).unwrap();
        }
        assert!(
            apg::artifacts::db_has_pending_state(&wt_apg),
            "a lone ledger identity is pending state"
        );
        assert!(!apg::git::is_fresh(&wt_apg), "pending state is never fresh");
        assert!(!socket.exists(), "the recovering start gets no socket");

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "a ledger-only start must rebuild and then serve"
        );
        assert_eq!(
            last(&routed("MATCH (n:PendingRemoval) RETURN count(*)")),
            "0",
            "the rebuild must drain the phantom ledger identity"
        );
        // The rebuild reads the node files, so the requirement the phantom
        // ledger named survives despite that ledger entry.
        let rows = routed("MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn");
        committed_rows(&rows);

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(REBUILD),
            "start must classify the ledger identity as pending state and rebuild: {stderr}"
        );
        assert!(!socket.exists(), "end must release the session socket");

        // ---- (3) CLEAN DB, no socket: start WITHOUT a scan ---------------
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the rebuilt DB must be clean before the last start"
        );
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the rebuild must re-anchor the scan so the DB stays fresh"
        );
        assert!(!socket.exists(), "the clean start gets no socket");

        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&wt_apg),
            "a clean, shape-compatible DB must start without a scan"
        );
        let rows = routed("MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn");
        committed_rows(&rows);
        assert_eq!(
            last(&routed(
                "MATCH (n:Requirement) WHERE n.updated = 1 RETURN count(*)"
            )),
            "0",
            "a scan-free start must leave the committed rows clean"
        );

        let out = end_session(&wt, session);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(apg::session::DB_OPEN_MARKER),
            "the clean start must open the existing DB: {stderr}"
        );
        assert!(
            !stderr.contains("rebuilding it from apg/layers/**"),
            "a clean, shape-compatible DB must start without a rebuild: {stderr}"
        );
        assert!(
            !stderr.contains("[load]"),
            "a clean, shape-compatible DB must start without a scan: {stderr}"
        );

        // ---- the rebuild never touched the durable store -----------------
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the rebuilds must leave apg/layers/** byte-for-byte at its snapshot"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "the rebuilds must create no commit"
        );
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the re-anchored scan must leave the DB fresh after all three starts"
        );

        testutil::remove(&repo);
    }
}
