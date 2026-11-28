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
/// process-wide `CWD_LOCK` hold. The per-invocation write path requires the
/// database (it refuses without one), so this fixture deliberately creates
/// none: a caller that needs the validated `write_project` writer must seed a
/// database first (e.g. [`testutil::touch_db`]). Returns
/// `(wt_apg_root, repo, wt_root)`.
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

/// Creates `hub` + `leaf-0..n` requirement nodes in a single durable
/// mutation. Setup only establishes the target nodes; the burst under test
/// is the edge/node work, so the N+1 endpoint writes happen once here while
/// the burst's N `apg edge add` invocations queue on the blocking worktree
/// lock.
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
/// waits for all, and returns the `(child-index, stderr)` of every failure.
///
/// Under Model C each child opens the worktree database read-only, upgrades
/// under the **blocking** worktree flock (waiting behind a concurrent writer,
/// never `EAGAIN`), performs its node-file write, exactly one commit and the
/// database projection in one critical section, then releases. A non-empty
/// result therefore means a writer failed to queue — the contract-violating
/// case the burst tests assert against.
fn run_edge_burst(wt: &Path, home: &Path, n: usize) -> Vec<(usize, String)> {
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
    let mut failures = Vec::new();
    for (i, child) in children {
        let out = child.wait_with_output().unwrap();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            eprintln!("burst[{i}] FAILED: {stderr}");
            failures.push((i, stderr));
        }
    }
    failures
}

/// The shared hub's out-edge count in the durable node-file store.
fn hub_out_edges(wt_apg: &Path) -> usize {
    layers::read_node_file(wt_apg, Layer::Requirements, "requirement", "hub")
        .unwrap()
        .out
        .len()
}

/// The complete `Constraint` FQN set, in a deterministic order, read through a
/// **real** `apg query` child with an isolated HOME.
///
/// Asserts the query exits 0 and printed its CSV header — the header appears
/// only after its own database open succeeds — so a caller that runs this from
/// inside a writer's blocking critical section gets the read-only/lock-free
/// proof for free: a read-write open would be refused by the held writer and a
/// `specs.lock` acquisition would wait behind it. Returns the observed
/// identities, i.e. one whole database snapshot (never a partial row set).
fn read_constraints(wt: &Path, home: &Path) -> Vec<String> {
    let out =
        testutil::ApgCommand::new(&["query", "MATCH (n:Constraint) RETURN n.fqn ORDER BY n.fqn"])
            .cwd(wt)
            .env("HOME", home.to_str().unwrap())
            .output();
    assert!(
        out.status.success(),
        "a read-only query must complete during a durable write: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("n.fqn"),
        "the read must print its CSV header only after opening the database: {text}"
    );
    lines.map(str::to_string).collect()
}

/// Asserts a mutation refusal is the unified project-context one: the
/// `mutation refused — ` prefix, naming branch `main` (the repo's default
/// branch) and the `apg project start` fix, and NEVER any of `preempted` — the
/// downstream errors the positional guard must precede (the absent-database
/// scan remedy, an absent node/edge).
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
/// guard `cargo test-e2e` (= `cargo test e2e:: -- --ignored`).
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

    /// A durable `node`/`edge` mutation needs NO started session: the
    /// invocation IS the session. Each `apg node add` / `apg edge add` opens
    /// the worktree database read-only, upgrades under the blocking worktree
    /// lock, writes its node file(s), makes exactly one git commit, projects
    /// the change into the database and releases. There is no
    /// `apg session start`/`save`/`end` to run, and no unsaved buffer a later
    /// process could be asked to flush.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn durable_mutations_need_no_started_session_and_land_atomically() {
        let (wt_apg, repo, wt) = mutation_fixture("no-started-session");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

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

        // Each invocation landed its own durable write: the node files are on
        // disk immediately and every invocation made exactly one commit.
        let owner_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "stakeholder", "owner");
        let r1_path = layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "r1");
        assert!(owner_path.exists(), "{} must exist", owner_path.display());
        assert!(r1_path.exists(), "{} must exist", r1_path.display());
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 3,
            "each of the three invocations must land exactly one commit"
        );

        // Both halves of the edge are durable and the store pairs cleanly.
        let src =
            layers::read_node_file(&wt_apg, Layer::Requirements, "stakeholder", "owner").unwrap();
        let dst =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(src.out.len(), 1);
        assert_eq!(src.out[0].kind, "requires");
        assert_eq!(src.out[0].target, "requirements.requirement.r1");
        assert!(
            dst.in_edges
                .iter()
                .any(|e| e.kind == "requires" && e.source == "requirements.stakeholder.owner"),
            "r1's requires in-half must be durable"
        );
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();

        // The branch DB reflects the writes: the invocation that wrote them
        // projected them into the held database inside that same critical
        // section.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.stakeholder.owner"));
        assert!(db.has_node("requirements.requirement.r1"));
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

    /// `apg node add` lands ONE file per node at
    /// `<layer>/<type>/<name>.json` immediately — the file name IS the
    /// identity, the FQN is derived `<layer>.<type>.<name>`, visible in the
    /// branch DB, committed on the project branch in exactly one commit, and
    /// never creating the legacy `apg/specs/`/`apg/notes/` paths. The write is
    /// the whole durability point: there is no save step to flush a buffer.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_add_lands_one_file_per_node_with_identity_and_branch_db_visibility() {
        let (wt_apg, repo, wt) = mutation_fixture("node-add");
        let home = repo.root.join("home");
        let before_commits = testutil::commit_count(&wt);

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

        // The file name is the identity, and it is durable at once.
        let path = wt_apg
            .join(layers::LAYERS_DIR)
            .join("global")
            .join("constraint")
            .join("timer.json");
        assert!(path.exists(), "{} must exist", path.display());
        let back = layers::read_node_file(&wt_apg, Layer::Global, "constraint", "timer").unwrap();
        assert_eq!(back.name, "timer");
        assert_eq!(
            fqn(Layer::Global, "constraint", "timer"),
            "global.constraint.timer"
        );

        // Visible in the branch DB: the write projected its own delta.
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.timer'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q.status.success(),
            "query: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the written node must be visible in the branch DB"
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

        // The node file was committed on the project branch in exactly one
        // commit (R8).
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "one node add must land exactly one commit"
        );
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            head.tree()
                .unwrap()
                .get_path(Path::new("apg/layers/global/constraint/timer.json"))
                .is_ok(),
            "the node file must be committed on the project branch"
        );

        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("global.constraint.timer"));
        drop(db);

        testutil::remove(&repo);
    }

    /// A client that disconnects before reading its reply cannot corrupt the
    /// store or orphan a lock: the per-invocation write runs to completion
    /// before the failed reply write, so every dropped client leaves a
    /// complete, committed, projected mutation behind and the next invocation
    /// still proceeds. (Model C has no long-lived daemon a disconnecting
    /// client could kill, and no socket a stale process could leave behind.)
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_client_that_disconnects_before_reading_its_reply_leaves_the_write_whole() {
        let (wt_apg, repo, wt) = mutation_fixture("client-disconnect");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

        for i in 0..3 {
            let name = format!("disconnected-{i}");
            let mut child =
                testutil::ApgCommand::new(&["node", "add", "global", "constraint", name.as_str()])
                    .cwd(&wt)
                    .env("HOME", home.to_str().unwrap())
                    .spawn();
            // The client drops both reply streams without reading them: only
            // the reply write can fail — the durable write already happened.
            drop(child.stdout.take());
            drop(child.stderr.take());
            let _ = child.wait();

            let path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", &name);
            assert!(
                path.exists(),
                "a disconnected client must still leave the `{name}` node file"
            );
        }

        // Every invocation made exactly one commit, so all three writes are
        // durable and no lock was lost.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 3,
            "every disconnected write must land exactly once"
        );

        // A fresh invocation opens the committed state: no orphaned lock, no
        // half-applied store.
        let q = testutil::spawn_apg(&["query", "MATCH (n:Constraint) RETURN count(n)"], &wt);
        assert!(
            q.status.success(),
            "a fresh read after the disconnects: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("3"),
            "all three disconnected writes must be visible"
        );
        // Model C has no daemon, so no live socket can be left behind.
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "a per-invocation write must leave no session socket"
        );

        testutil::remove(&repo);
    }

    /// `apg edge add` writes BOTH endpoint files immediately — the out half in
    /// the source's file, the matching in half in the target's, with identical
    /// properties — keeping the store pairing-consistent, landing the edge in
    /// the branch DB, and making exactly one commit for the one logical
    /// mutation.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn edge_add_writes_both_endpoint_files() {
        let (wt_apg, repo, wt) = mutation_fixture("edge-add");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

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
        // A `requires` edge from a Stakeholder to a Requirement is the minimal
        // whole-model-valid authored edge (SPEC §8.1): it roots the Requirement
        // and satisfies the Stakeholder's `requires` minimum.
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

        // Both halves landed on disk: out in the source's file, in in the
        // target's, with matching kind/endpoints and identical properties.
        let src_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "stakeholder", "owner").unwrap();
        let dst_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(src_back.out.len(), 1);
        assert_eq!(src_back.out[0].kind, "requires");
        assert_eq!(src_back.out[0].target, "requirements.requirement.r1");
        assert_eq!(dst_back.in_edges.len(), 1);
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

        // Exactly ONE commit for the edge's one logical mutation (the two node
        // adds above each made their own).
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 3,
            "each invocation must land exactly one commit"
        );

        // The edge is in the branch DB.
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
    /// DB shows them exactly like a full scan would. Neither record kind is
    /// dropped.
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
        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
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
        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
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
        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
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

    /// The per-invocation write is a **blocking-lock critical section**: one
    /// `apg node add` opens the worktree database read-only, upgrades under the
    /// extended `apg/.trans/specs.lock` flock, performs its node-file write,
    /// exactly one commit and the database projection, then releases. The lock
    /// file the write took exists afterwards; the read-write class is free and
    /// a read-only query coexists; and no daemon/socket survives the
    /// invocation.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn per_invocation_write_takes_the_blocking_lock_and_commits_once() {
        let (wt_apg, repo, wt) = mutation_fixture("write-lock");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let spec_lock = wt_apg.join(specs::TRANS).join("specs.lock");
        let before_commits = testutil::commit_count(&wt);

        let out = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "locked"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Added node global.constraint.locked"
        );

        // The write took the extended whole-durable-sequence flock (the lock
        // file is its observable trace).
        assert!(
            spec_lock.exists(),
            "the durable write must take the extended specs.lock flock: {} missing",
            spec_lock.display()
        );
        // One node file, one commit, and the row projected into the DB.
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "locked").exists(),
            "the write must land its node file in its own invocation"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the write must land exactly one commit"
        );

        // The critical section released: a read-write opener succeeds and a
        // read-only query coexists.
        let reopened = ArtifactDb::open(&wt_apg);
        assert!(
            reopened.is_ok(),
            "the blocking lock must be released after the write: {:?}",
            reopened.err()
        );
        drop(reopened);
        let ro = spawn_apg(&["query", "MATCH (n:Constraint) RETURN count(n)"], &wt);
        assert!(
            ro.status.success(),
            "a read-only query must coexist with the released write: {}",
            String::from_utf8_lossy(&ro.stderr)
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "a per-invocation write must leave no session socket"
        );

        testutil::remove(&repo);
    }

    /// The strict node/edge UPDATE sweep through the top-level
    /// `cmd_node`/`cmd_edge` DISPATCH, each call its own per-invocation write.
    ///
    /// The setup writes the r0 -> r1 -> r2 fixture once; every dispatch call
    /// below is a separate invocation that reads the committed model, writes
    /// its change, commits once and projects. Every update is edge-preserving:
    /// the node update MERGEs body/properties without dropping a key (only an
    /// explicit `--unset-property` drops one) and keeps r1's out- and in-halves;
    /// the edge update MERGEs/un-sets the same map onto BOTH halves.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn strict_node_and_edge_update_preserve_edges_through_dispatch() {
        let (wt_apg, repo, wt) = node_store_fixture("dispatch-strict");

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
        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
        layers::write_project(&wt_apg, &[owner, r0, r1, r2, svc, grp, cmp, rl], &[]).unwrap();
        testutil::scan_checkout(&wt).unwrap();

        let before =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(before.out.len(), 1, "one out-edge before the update");
        assert_eq!(before.in_edges.len(), 2, "two in-edges before the update");
        let commits_before = testutil::commit_count(&wt);

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
        // is duplicated. The setup edge is on disk, so the next invocation's
        // committed read refuses.
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

        // The four successful dispatch calls each landed their own commit (the
        // refused duplicate landed none).
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 4,
            "each successful invocation must land exactly one commit"
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

        testutil::remove(&repo);
    }

    /// A parallel burst of N separate `apg edge add` invocations all queue on
    /// the **blocking** worktree flock: the lock waits (never `EAGAIN`), so
    /// every writer takes its turn, none fails, and no update is lost. After
    /// the burst the shared hub carries exactly N distinct out-edges — the
    /// serial application — landed one commit per invocation, and the branch
    /// DB equals that serial store.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn concurrent_writers_queue_and_lose_no_update() {
        const N: usize = 8;
        let (wt_apg, repo, wt) = mutation_fixture("concurrent-writers");
        setup_hub_and_leaves(&wt, N);
        let home = repo.root.join("home");
        let before_commits = testutil::commit_count(&wt);

        // A parallel burst of N separate `apg edge add` processes: the blocking
        // lock serialises their critical sections, so zero failures is the
        // no-lock-error / no-lost-update evidence.
        let failures = run_edge_burst(&wt, &home, N);
        assert!(
            failures.is_empty(),
            "the burst lost {} of {N} mutations: {failures:?}",
            failures.len()
        );

        // Durable now: the hub carries exactly N distinct out-edges — one per
        // leaf, no duplicate and no lost update — landed one commit per
        // invocation.
        assert_eq!(hub_out_edges(&wt_apg), N, "the burst lost an edge");
        let hub =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "hub").unwrap();
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
            "the burst must carry exactly one edge to each leaf"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + N,
            "each queued invocation must land exactly one commit"
        );

        // Store == serial application, visible in the DB.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let q = db
        .q("MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:Contains]->(b) RETURN count(*)")
        .unwrap();
        assert_eq!(q.lines().last().map(str::trim), Some("8"), "{q}");
        drop(db);
        testutil::remove(&repo);
    }

    /// Every mutation is visible to a SEPARATE reader as soon as its invocation
    /// returns: the write projects into the worktree database inside its own
    /// critical section (there is no end-of-run flush). The DB open is **not**
    /// amortized across mutations — each invocation opens its own read-only
    /// handle and upgrades it under the lock — and each mutation lands its node
    /// file plus exactly one commit. No daemon is left behind.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn per_invocation_writes_are_visible_immediately_and_never_amortized() {
        const N: usize = 4;
        let (wt_apg, repo, wt) = mutation_fixture("per-invocation-visible");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

        for i in 0..N {
            let name = format!("visible-{i}");
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

            // Durable immediately: the node file exists and exactly one commit
            // landed for this invocation.
            assert!(
                layers::node_file_path(&wt_apg, Layer::Global, "constraint", &name).exists(),
                "mutation {i} must write its node file in its own invocation"
            );
            assert_eq!(
                testutil::commit_count(&wt),
                before_commits + i + 1,
                "mutation {i} must land exactly one commit"
            );

            // A SEPARATE reader sees the mutation as soon as it returns.
            let query =
                format!("MATCH (n:Constraint {{fqn: 'global.constraint.{name}'}}) RETURN count(n)");
            let q = testutil::spawn_apg(&["query", query.as_str()], &wt);
            assert!(
                q.status.success(),
                "read {i}: {}",
                String::from_utf8_lossy(&q.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&q.stdout)
                    .lines()
                    .last()
                    .map(str::trim),
                Some("1"),
                "mutation {i} must be visible immediately"
            );
        }

        // N invocations, N commits, N durable node files — no amortized run.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + N,
            "each invocation must commit its own mutation"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "no daemon may survive the invocations"
        );
        testutil::remove(&repo);
    }

    /// A repeated `node add` is still NOT an implicit upsert: there is no
    /// at-most-once client id and no cached reply to hide the second call, so
    /// the second invocation re-runs the strict existence check, refuses, and
    /// writes nothing. The first invocation's write stays exactly once.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_repeated_node_add_is_refused_never_an_implicit_upsert() {
        let (wt_apg, repo, wt) = mutation_fixture("repeated-add");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

        let args = ["node", "add", "global", "constraint", "once"];
        let first = testutil::ApgCommand::new(&args)
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&first.stdout).trim(),
            "Added node global.constraint.once"
        );
        let path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "once");
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(testutil::commit_count(&wt), before_commits + 1);

        // The repeated invocation refuses: the add is never an implicit upsert.
        let replay = testutil::ApgCommand::new(&args)
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !replay.status.success(),
            "a repeated node add must be refused"
        );
        let msg = String::from_utf8_lossy(&replay.stderr);
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg node update"), "{msg}");
        assert!(msg.contains("apg node rm"), "{msg}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "a refused re-add must write nothing"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "a refused re-add must not create a commit"
        );

        // The node was present exactly once throughout.
        let count = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.once'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            count.status.success(),
            "query: {}",
            String::from_utf8_lossy(&count.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&count.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the refused re-add must not duplicate the node"
        );

        testutil::remove(&repo);
    }

    /// `apg scan` and `apg project merge` run between writes with no session to
    /// save or end. A completed write re-anchors the scan row, so `apg scan`
    /// takes its content-identity freshness fast path and succeeds; the merge
    /// path is never blocked by a session gate (it proceeds to the fixture's
    /// unrelated missing-plan refusal). Running the scan creates no commit.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn scan_and_merge_run_between_writes() {
        let (_wt_apg, repo, wt) = mutation_fixture("scan-merge-between");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

        let add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "global",
            "constraint",
            "between",
            "--body",
            "written between scans",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the write must land exactly one commit"
        );

        // `apg scan` runs between writes: no save/end is needed.
        let scan = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            scan.status.success(),
            "scan must run between writes: {}",
            String::from_utf8_lossy(&scan.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "a scan between writes must not create a commit"
        );

        // `apg project merge` is never blocked by a session gate; it proceeds
        // to the fixture's unrelated missing-plan refusal.
        let merge = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        let merge_err = String::from_utf8_lossy(&merge.stderr);
        assert!(
            !merge_err.to_lowercase().contains("session"),
            "merge must not be blocked by a session gate: {merge_err}"
        );

        // And a second scan still runs after the merge attempt.
        let scan_after = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            scan_after.status.success(),
            "scan must run after the merge attempt: {}",
            String::from_utf8_lossy(&scan_after.stderr)
        );

        testutil::remove(&repo);
    }

    /// The Model C acceptance, end to end on a scratch `/tmp` git repo: a
    /// per-invocation session makes a durable write impossible to run without
    /// one, so no invocation can be told to `save` or `end` — and under it
    ///
    /// 1. concurrent durable writes from separate invocations **queue** on the
    ///    blocking worktree lock (they wait, never `EAGAIN`), so none reports a
    ///    lock error;
    /// 2. every write is **durable** — no lost update: the store carries all
    ///    N edges, each invocation landed exactly one commit, and the branch
    ///    database equals the serial store;
    /// 3. a durable write with **no started session** succeeds (there is no
    ///    session to start: the invocation is the session);
    /// 4. `apg scan` and `apg project merge` run **between** writes, after
    ///    which a further durable write still lands.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn acceptance_model_c_concurrent_writers_queue_without_lock_error_or_lost_update() {
        const N: usize = 8;
        let (wt_apg, repo, wt) = mutation_fixture("model-c-acceptance");
        setup_hub_and_leaves(&wt, N);
        let home = repo.root.join("home");
        let spec_lock = wt_apg.join(specs::TRANS).join("specs.lock");
        let before_commits = testutil::commit_count(&wt);

        // (1) A parallel burst of N separate `apg edge add` processes: each
        // upgrades under the blocking worktree flock and waits its turn, so a
        // lock failure would surface as a failed child. Zero failures is the
        // no-lock-error evidence.
        let failures = run_edge_burst(&wt, &home, N);
        assert!(
            failures.is_empty(),
            "the burst lost {} of {N} mutations on the blocking lock: {failures:?}",
            failures.len()
        );
        // The burst actually took the blocking whole-durable-sequence flock.
        assert!(
            spec_lock.exists(),
            "the writers must take the blocking specs.lock flock: {} missing",
            spec_lock.display()
        );

        // (2) Durable, no lost update: exactly N distinct hub -> leaf-i edges,
        // one commit per invocation, and the branch DB equals the serial store.
        assert_eq!(hub_out_edges(&wt_apg), N, "the burst lost an edge");
        let hub =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "hub").unwrap();
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
            "the burst must carry exactly one durable edge to each leaf"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + N,
            "each queued invocation must land exactly one commit"
        );
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            let q = db
                .q("MATCH (:Requirement {fqn: 'requirements.requirement.hub'})-[:Contains]->(b) RETURN count(*)")
                .unwrap();
            assert_eq!(q.lines().last().map(str::trim), Some("8"), "{q}");
        }

        // (3) A durable write with NO started session succeeds: nothing is
        // started, saved or ended — the invocation is the session. It takes the
        // blocking lock, lands its node file, exactly one commit and projection,
        // and leaves no daemon/socket behind.
        let add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "global",
            "constraint",
            "model-c-no-session",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "a write with no started session must succeed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&add.stdout).trim(),
            "Added node global.constraint.model-c-no-session"
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "model-c-no-session")
                .exists(),
            "the session-less write must land its node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + N + 1,
            "the session-less write must land exactly one commit"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "no started session/daemon may exist"
        );
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Constraint {fqn: 'global.constraint.model-c-no-session'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q.status.success(),
            "read after the session-less write: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the session-less write must be projected into the branch DB"
        );

        // (4) `apg scan` and `apg project merge` run between writes — no save or
        // end is needed. The scan takes its freshness fast path and commits
        // nothing; the merge is never blocked by a session gate (it proceeds to
        // the fixture's unrelated missing-plan refusal).
        let scan = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            scan.status.success(),
            "scan must run between writes: {}",
            String::from_utf8_lossy(&scan.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + N + 1,
            "a scan between writes must not create a commit"
        );
        let merge = testutil::ApgCommand::new(&["project", "merge", "foo"])
            .cwd(&repo.root)
            .env("HOME", home.to_str().unwrap())
            .output();
        let merge_err = String::from_utf8_lossy(&merge.stderr);
        assert!(
            !merge_err.to_lowercase().contains("session"),
            "merge must not be blocked by a session gate: {merge_err}"
        );

        // A further durable write after scan/merge still lands exactly one
        // commit, proving the between-write runs leave the write path usable.
        let after =
            testutil::ApgCommand::new(&["node", "add", "global", "constraint", "model-c-after"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            after.status.success(),
            "a write after scan/merge must succeed: {}",
            String::from_utf8_lossy(&after.stderr)
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + N + 2,
            "the write after scan/merge must land exactly one commit"
        );
        let scan_after = testutil::ApgCommand::new(&["scan", wt.to_str().unwrap()])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            scan_after.status.success(),
            "scan must run after the between-write burst: {}",
            String::from_utf8_lossy(&scan_after.stderr)
        );

        testutil::remove(&repo);
    }

    /// The Model C acceptance for a READ overlapping a durable WRITE, made
    /// deterministic rather than a lucky race: the real durable-write sequence
    /// ([`layers::write_project`]) is paused at its one-shot
    /// `MutationBoundary::BeforeCommit` / `BeforeProject` seams while its
    /// **blocking** `specs.lock` critical section and write-capable database
    /// handle are held, and a real `apg query` child runs inside each window.
    ///
    /// The read must, at every observed checkpoint:
    /// 1. succeed — it opens the worktree database **read-only** and takes no
    ///    `specs.lock` (a read-write open would be refused by the held writer, a
    ///    flock acquisition would wait behind it, so exit 0 is the read-only,
    ///    lock-free proof);
    /// 2. observe a **complete committed state** — either the prior projection or
    ///    the write's complete committed projection, never a partial, mixed or
    ///    empty result and never an error;
    /// 3. agree with the durable `apg/layers` store and the git HEAD: before the
    ///    commit's node file, commit and projection are all absent; after the
    ///    write's invocation returns all three are present.
    ///
    /// The commit→project window is observed too: the durable node file and its
    /// one commit have landed while the projection stays prior, and the read
    /// still returns the whole prior projection — commit-then-project never
    /// exposes a half-applied one.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn acceptance_model_c_read_during_write_observes_a_complete_committed_state() {
        let (wt_apg, repo, wt) = mutation_fixture("model-c-read-during-write");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let spec_lock = wt_apg.join(specs::TRANS).join("specs.lock");
        let constraint = |name: &str| format!("global.constraint.{name}");

        // A committed prior state: one node landed by its own invocation, so
        // there is a complete state for a concurrent read to observe while the
        // write under test runs.
        let prior = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "prior"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            prior.status.success(),
            "the prior state must land: {}",
            String::from_utf8_lossy(&prior.stderr)
        );
        let prior_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "prior");
        let during_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "during");
        let after_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "after");
        assert!(prior_path.exists(), "the prior node file must exist");
        assert_eq!(
            read_constraints(&wt, &home),
            vec![constraint("prior")],
            "the prior state must be the whole committed projection"
        );
        let commits_before_write = testutil::commit_count(&wt);

        // --- Checkpoint 1: the write paused BEFORE its commit ----------------
        // The hook runs inside `write_project`'s blocking flock section, with
        // its write-capable handle held; the child read runs in that window.
        layers::install_mutation_hook(layers::MutationBoundary::BeforeCommit, {
            let (wt, home) = (wt.clone(), home.clone());
            let (prior_path, during_path) = (prior_path.clone(), during_path.clone());
            let spec_lock = spec_lock.clone();
            move || {
                assert!(
                    spec_lock.exists(),
                    "the hook must run inside the writer's blocking specs.lock critical section"
                );
                assert!(
                    prior_path.exists(),
                    "the prior committed node file must be intact"
                );
                assert!(
                    !during_path.exists(),
                    "before the commit this write's node file must not exist"
                );
                assert_eq!(
                    testutil::commit_count(&wt),
                    commits_before_write,
                    "before the commit this write must not have committed"
                );
                // The read-only query completes beside the held writer and sees
                // the prior complete state (no partial/mixed/empty result).
                assert_eq!(
                    read_constraints(&wt, &home),
                    vec![constraint("prior")],
                    "a read in the pre-commit window must observe the prior complete state"
                );
                Ok(())
            }
        });
        layers::write_project(&wt_apg, &[node("global", "constraint", "during")], &[]).unwrap();

        // --- Checkpoint 2: the write's invocation returned -------------------
        // Its node file, its one commit and its projection now all agree.
        assert!(during_path.exists(), "the write must land its node file");
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before_write + 1,
            "the write must land exactly one commit"
        );
        assert_eq!(
            read_constraints(&wt, &home),
            vec![constraint("during"), constraint("prior")],
            "the read must observe the write's complete committed state"
        );

        // --- Checkpoint 3: the next write paused between commit and project --
        // The durable node file and its one commit HAVE landed; the projection
        // has not. The read still observes the whole prior projection, never a
        // half-applied one.
        let commits_before_second = commits_before_write + 1;
        layers::install_mutation_hook(layers::MutationBoundary::BeforeProject, {
            let (wt, home) = (wt.clone(), home.clone());
            let (after_path, spec_lock) = (after_path.clone(), spec_lock.clone());
            move || {
                assert!(
                    spec_lock.exists(),
                    "the hook must run inside the writer's blocking specs.lock critical section"
                );
                assert!(
                    after_path.exists(),
                    "the committed node file must exist before the projection apply"
                );
                assert_eq!(
                    testutil::commit_count(&wt),
                    commits_before_second + 1,
                    "the durable write must have committed before the projection apply"
                );
                // Commit-then-project: a complete committed projection, not a
                // partial node-file/commit/projection mix served to the reader.
                assert_eq!(
                    read_constraints(&wt, &home),
                    vec![constraint("during"), constraint("prior")],
                    "a read in the commit→project window must observe a complete committed projection"
                );
                Ok(())
            }
        });
        layers::write_project(&wt_apg, &[node("global", "constraint", "after")], &[]).unwrap();

        // --- Checkpoint 4: the second write's invocation returned ------------
        // Durable store, git HEAD and the projection agree on the whole new
        // state.
        assert!(
            after_path.exists(),
            "the second write must land its node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before_second + 1,
            "the second write must land exactly one commit"
        );
        assert_eq!(
            read_constraints(&wt, &home),
            vec![
                constraint("after"),
                constraint("during"),
                constraint("prior")
            ],
            "the read must observe the second write's complete committed state"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "no session daemon/socket may exist under per-invocation sessions"
        );

        testutil::remove(&repo);
    }

    /// A durable `apg node add` prints its write-time wording advisory on the
    /// caller's OWN stderr at the write (`apg: warning: …`) while the write's
    /// result is left intact — the add succeeds and its node file is durable.
    /// An update whose `--body` carries no likely-flagged wording prints NO
    /// warning, so the advisory is body-driven rather than unconditional.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn the_wording_advisory_reaches_the_client_at_the_write() {
        let (wt_apg, repo, wt) = mutation_fixture("routed-warning");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let path = layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "demo-val");

        let run = |args: &[&str]| -> std::process::Output {
            testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output()
        };

        // A `node add` whose body carries time-relative wording (`was`)
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
            "the advisory must not block the add: {}",
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

        // A `node update` whose supplied `--body` carries no likely-flagged
        // wording emits NO warning — the advisory is body-driven, not
        // unconditional — and still succeeds.
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
            "a plain update must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("apg: warning:"),
            "plain wording must not print a warning: {stderr}"
        );

        // The write's result is intact: the node file carries the plain body.
        assert!(
            path.exists(),
            "the flagged-but-advisory add must still land its node file"
        );
        let back = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "demo-val")
            .unwrap();
        assert_eq!(back.body, "The service stores the record.");

        testutil::remove(&repo);
    }

    /// A worktree with no `apg/.trans/db.lbug` cannot host a durable write —
    /// the per-invocation session's read-only open refuses, naming `apg scan`
    /// as the remedy, and the refusal leaves nothing behind: no node file and
    /// no commit. The project-context guard ADMITS this project worktree, so
    /// the refusal is pinned to the DB cause ALONE — its stderr carries
    /// NEITHER the guard's `mutation refused` prefix NOR the `project context`
    /// wording — and a subsequent scan then makes the same write succeed
    /// (proving the refused invocation released the flock).
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_durable_write_without_a_db_refuses_naming_scan() {
        let (wt_apg, repo, wt) = node_store_fixture("write-no-db");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A durable node gives the later scan authored content to ingest. The
        // fixture must stay DB-less (the exact state under test), so write and
        // commit the node file directly: the validated `write_project` path
        // resolves its code universes from `db.lbug` and refuses an absent one.
        layers::write_node(&wt_apg, &node("requirements", "requirement", "seeded")).unwrap();
        wt_commit(
            &wt,
            &["apg/layers/requirements/requirement/seeded.json"],
            "seed node",
        );
        let before_commits = testutil::commit_count(&wt);

        let db = wt_apg.join(specs::TRANS).join("db.lbug");
        assert!(!db.exists(), "fixture must start with no db.lbug");

        // (a) The refusal: the candidate binary exits non-zero and its stderr
        // names `apg scan`.
        let refused =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "fresh"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            !refused.status.success(),
            "a durable write must refuse an absent db.lbug"
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

        // (b) Nothing left behind: no DB conjured, no node file, no commit.
        assert!(!db.exists(), "a refused write must not create db.lbug");
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "fresh").exists(),
            "a refused write must not write a node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a refused write must not create a commit"
        );

        // (c) A scan creates the DB the write needs.
        testutil::scan_checkout(&wt).unwrap();
        assert!(db.exists(), "the scan must create db.lbug");

        // (d) The same write then succeeds — re-acquiring the flock proves the
        // refused invocation released it — and lands its one commit.
        let added =
            testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "fresh"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            added.status.success(),
            "the post-refusal write must succeed: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "fresh").exists()
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the successful write must land exactly one commit"
        );

        testutil::remove(&repo);
    }

    /// On the MAIN checkout every node mutation refuses for project context —
    /// BEFORE the per-invocation session opens the database and before any
    /// existence resolution — so `node add`, `node update` and `node rm` never
    /// read as `apg scan` or an absent-node error, and no node file or commit
    /// lands. The guard behaves identically whether or not a `db.lbug` exists.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn node_mutations_on_main_refuse_context_before_db_and_existence() {
        let repo = Repo::new("node-guard-main");
        let apg_root = repo.apg_root();

        // No DB yet: the refusal is still the context one, never `apg scan`.
        let argv = vec![
            "node",
            "add",
            "requirements",
            "requirement",
            "fresh",
            "--body",
            "must not land",
        ];
        let before_no_db = testutil::commit_count(&repo.root);
        let out = testutil::spawn_apg(&argv, &repo.root);
        assert_refused_on_main(&out, &argv, &["apg scan", "does not exist"]);
        assert_no_durable_trace(&apg_root, &repo.root, before_no_db);

        // A scanned DB exists: the same refusal (the guard precedes the DB and
        // any existence lookup).
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        testutil::scan_checkout(&repo.root).unwrap();
        let before_commits = testutil::commit_count(&repo.root);
        for argv in [
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
            assert_refused_on_main(&out, &argv, &["apg scan", "does not exist"]);
        }

        assert_no_durable_trace(&apg_root, &repo.root, before_commits);
        testutil::remove(&repo);
    }

    /// On the MAIN checkout every edge mutation refuses for project context
    /// BEFORE either endpoint is resolved — so `edge add`, `edge update` and
    /// `edge rm` over endpoints absent from that DB never report the absent
    /// node/edge, and no node file or commit lands.
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
            assert_refused_on_main(&out, &argv, &["apg scan", "does not exist"]);
        }

        assert_no_durable_trace(&apg_root, &repo.root, before_commits);
        testutil::remove(&repo);
    }

    /// On the scanned main checkout, all six absent-target node/edge mutations
    /// refuse on the CLIENT for project context — never the absent-database
    /// scan remedy or a missing node — with `apg/layers` untouched and no
    /// commit.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
    fn main_mutations_refuse_project_context() {
        let repo = Repo::new("node-guard-main-scanned");
        let apg_root = repo.apg_root();
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        testutil::scan_checkout(&repo.root).unwrap();
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
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
            assert_refused_on_main(&out, &argv, &["apg scan", "does not exist"]);
        }

        assert_no_durable_trace(&apg_root, &repo.root, before_commits);
        testutil::remove(&repo);
    }

    /// A detached HEAD is not a project context, so the CLI refuses the rm,
    /// and the direct-path builder (`node_cmd::build_change`) over the intact
    /// `.trans` warns on NOTHING — exactly the `git::repo_identity(...).branch`
    /// `None` case.
    ///
    /// A committed-and-scanned durable requirement (`reviewed`) plus a
    /// `Feedback -[:Reviews]-> reviewed` pair is sealed into `.trans` before the
    /// direct-path builder runs, so the builder sees the outstanding feedback.
    /// HEAD is then detached at the scanned commit.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn detached_head_rm_refuses_context_and_the_direct_path_warns_on_nothing() {
        let (repo, wt, wt_apg) = testutil::project_with_db("detached-head-rm");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

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
        // the CLI refuses the rm — never the absent-database scan remedy or a
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
        assert!(!msg.contains("apg scan"), "{msg}");
        assert!(!msg.contains("does not exist"), "{msg}");

        testutil::remove(&repo);
    }

    /// A plain directory with no git repository is not a project context, so
    /// every mutation refuses naming `not inside a git repository`, `holds no
    /// branch`, `git init` and `apg project start` — and writes nothing under
    /// `apg/layers`.
    #[test]
    #[ignore = "e2e tier: real I/O (fs/git/db/process); run via cargo test-e2e"]
    fn non_git_mutations_refuse_project_context() {
        let dir = std::env::temp_dir().join(format!("apg-project-{}-non-git", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let apg_root = dir.join(specs::LAYOUT);
        std::fs::create_dir_all(apg_root.join(specs::TRANS)).unwrap();
        testutil::touch_db(&apg_root);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let out = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "fresh"])
            .cwd(&dir)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            !out.status.success(),
            "a non-git directory must refuse a durable mutation"
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
        let layers_dir = apg_root.join(layers::LAYERS_DIR);
        assert!(
            !layers_dir.exists() || std::fs::read_dir(&layers_dir).unwrap().next().is_none(),
            "a refused mutation must write nothing under apg/layers"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A DURABLE node that is first updated and then removed: each invocation
    /// is its own critical section, so the update lands (one commit) before the
    /// remove reads the committed model, and the remove then drops exactly that
    /// node file in its own commit while leaving the untouched neighbour
    /// byte-identical.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn rm_of_a_saved_node_that_was_updated_leaves_no_file_and_one_commit() {
        // The untouched neighbour is a durable node file written BEFORE the
        // scan, so its bytes witness that the remove rewrites only its own
        // identity.
        let (wt_apg, repo, wt) = node_store_fixture("rm-marker");
        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
        layers::write_project(&wt_apg, &[node("global", "constraint", "keep")], &[]).unwrap();
        testutil::scan_checkout(&wt).unwrap();
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

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
        // Reads the branch DB through a fresh read-only invocation and returns
        // the single scalar the query projects.
        let routed_count = |query: &str| -> String {
            let out = testutil::spawn_apg(&["query", query], &wt);
            assert!(
                out.status.success(),
                "query `{query}`: {}",
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
        let doomed_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "doomed");
        let keep_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "keep");
        assert!(
            doomed_path.exists(),
            "the add must have written the `doomed` node file"
        );
        let keep_bytes = std::fs::read(&keep_path).unwrap();
        let commits_before = testutil::commit_count(&wt);

        // The update lands in its own invocation (one commit); the remove reads
        // that committed state in its own invocation and lands one more.
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
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN n.body"),
            "second",
            "the update must be projected into the branch DB"
        );
        run(
            &["node", "rm", "global", "constraint", "doomed"],
            "Removed node global.constraint.doomed",
        );

        assert!(
            !doomed_path.exists(),
            "the remove must delete the durable `doomed` node file"
        );
        assert!(
            keep_path.exists(),
            "the remove must leave the untouched `keep` node file in place"
        );
        assert_eq!(
            std::fs::read(&keep_path).unwrap(),
            keep_bytes,
            "the remove must leave the untouched node file byte-identical"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 2,
            "the update and the remove must each land exactly one commit"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.doomed'}) RETURN count(*)"),
            "0",
            "the removed node must be gone from the branch DB"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.keep'}) RETURN count(*)"),
            "1",
            "the untouched neighbour must survive"
        );

        testutil::remove(&repo);
    }

    /// A node CREATED and then REMOVED: both are separate per-invocation
    /// writes, so the add lands its file and commit and the remove then deletes
    /// that file in its own commit — the net store is empty of the ephemeral
    /// identity, and no pending-removal ledger row exists (there is no
    /// buffer to hold one).
    ///
    /// The mirror arm — a node that was saved, then removed and re-added. The
    /// remove commits the deletion; the re-add then writes the last admitted
    /// content in a fresh commit, so the file ends with the re-added body.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn rm_of_a_node_created_in_the_same_run_leaves_no_file() {
        let (wt_apg, repo, wt) = mutation_fixture("rm-created");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
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
        let routed_count = |query: &str| -> String {
            let out = testutil::spawn_apg(&["query", query], &wt);
            assert!(
                out.status.success(),
                "query `{query}`: {}",
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
        let node_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "ephemeral");
        assert!(
            node_path.exists(),
            "the add must write the ephemeral node file"
        );
        run(
            &["node", "rm", "requirements", "requirement", "ephemeral"],
            "Removed node requirements.requirement.ephemeral",
        );

        // Two invocations, two commits, and the net store has no file.
        assert!(
            !node_path.exists(),
            "the remove must delete the ephemeral node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            commits_before + 2,
            "the add and the remove must each land exactly one commit"
        );
        assert_eq!(
            routed_count(
                "MATCH (n:PendingRemoval {fqn: 'requirements.requirement.ephemeral'}) RETURN count(*)"
            ),
            "0",
            "a created-then-removed identity must leave no ledger row"
        );
        assert_eq!(
            routed_count(
                "MATCH (n:Requirement {fqn: 'requirements.requirement.ephemeral'}) RETURN count(*)"
            ),
            "0",
            "the branch DB must not carry the removed node"
        );

        // The mirror arm: add `reborn`, remove it, then re-add it with a new
        // body. Each step is its own commit; the final file is the re-added
        // content, never deleted.
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
        assert!(reborn_path.exists());
        run(
            &["node", "rm", "global", "constraint", "reborn"],
            "Removed node global.constraint.reborn",
        );
        assert!(!reborn_path.exists(), "the remove must delete the file");
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
        assert!(
            reborn_path.exists(),
            "the re-add must write the node file, never delete it"
        );
        assert_eq!(
            layers::read_node_file(&wt_apg, Layer::Global, "constraint", "reborn")
                .unwrap()
                .body,
            "second",
            "the re-add must write its last admitted content"
        );
        assert_eq!(
            routed_count("MATCH (n:Constraint {fqn: 'global.constraint.reborn'}) RETURN count(*)"),
            "1",
            "the branch DB must carry the re-added node"
        );

        testutil::remove(&repo);
    }

    /// A durable mutation touches ONLY its own rows.
    ///
    /// With two committed-and-scanned durable requirements (`keep` and `move`)
    /// in the branch DB, plus a pre-existing transient
    /// `Feedback -[:Reviews]-> requirements.requirement.keep` pairing seeded
    /// into `.trans` before the write, one update of `move` must:
    ///
    ///   1. leave the untouched `keep` row — fqn/id/title/body/feature and the
    ///      full serialized `properties` — **byte-for-byte unchanged**; and
    ///   2. move only `move`'s row: its body reflects the mutation (with its
    ///      preserved properties), and
    ///   3. project the `.trans` Feedback/Reviews pairing the write re-merges.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_mutation_touches_only_its_own_rows() {
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
        std::fs::create_dir_all(&home).unwrap();

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

        // Seed a pre-existing `Feedback -[:Reviews]-> keep` pairing into the
        // worktree's `.trans` requirements mirror BEFORE the scan, so the scan
        // records the same authored digest the write's staleness gate compares
        // against; the write's projection then re-merges the transient records.
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
        testutil::scan_checkout(&wt).unwrap();

        // A fresh read-only invocation renders a query's rows.
        let read = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        // Capture the "before" state: the scan projected the durable nodes and
        // the transient pairing.
        let keep_before = read(KEEP_ROW);
        let move_before = read(MOVE_ROW);
        let reviews_before = read(REVIEWS_EDGE);
        assert!(
            keep_before.contains("isolation") && keep_before.contains("empty"),
            "the untouched node's full properties must be present in its row: {keep_before}"
        );
        assert!(
            reviews_before.contains("foo/feedback-1")
                && reviews_before.contains("requirements.requirement.keep"),
            "the scan must project Feedback -[:Reviews]-> keep: {reviews_before}"
        );

        // One mutation that changes ONLY `move`.
        let update = testutil::ApgCommand::new(&[
            "node",
            "update",
            "requirements",
            "requirement",
            "move",
            "--body",
            "moved body",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            update.status.success(),
            "the update must land: {}",
            String::from_utf8_lossy(&update.stderr)
        );

        // (1) The untouched identity is byte-for-byte unchanged.
        let keep_after = read(KEEP_ROW);
        assert_eq!(
            keep_after, keep_before,
            "the unaffected node's row must be byte-for-byte unchanged by the mutation"
        );

        // (2) Only `move` moved: its body reflects the mutation and its
        // preserved properties survive.
        let move_after = read(MOVE_ROW);
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

        // (3) The write re-merged the `.trans` records, so the pre-existing
        // Feedback/Reviews pairing is present and unchanged.
        let reviews_after = read(REVIEWS_EDGE);
        assert_eq!(
            reviews_after, reviews_before,
            "the write's projection must re-merge the unchanged Feedback -[:Reviews]-> keep pairing"
        );
        let feedback_after = read(FEEDBACK_ROW);
        assert!(
            feedback_after.contains("foo/feedback-1")
                && feedback_after.contains("review of keep")
                && feedback_after.contains("open"),
            "the Feedback record must be projected intact: {feedback_after}"
        );

        testutil::remove(&repo);
    }

    /// A write's projection re-merges the worktree's `.trans` records, so an
    /// update preserves a transient `Feedback -[:Reviews]-> durable-node`
    /// pairing sourced from `.trans`; a subsequent remove of that node drops
    /// the now-dangling Reviews edge while the Feedback record itself survives.
    ///
    /// Both are separate per-invocation writes: the update commits its body
    /// change, and the remove commits the node file's deletion. The Feedback
    /// records are authoritative `.trans` state, so removing their target does
    /// not delete them.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_write_projection_preserves_transient_reviews_and_a_remove_drops_the_dangling_edge() {
        const REVIEWS_EDGE: &str = "MATCH (f:Feedback)-[:Reviews]->\
             (n:Requirement {fqn: 'requirements.requirement.reviewed'}) RETURN f.fqn, n.fqn";
        const REVIEWS_COUNT: &str = "MATCH (:Feedback)-[:Reviews]->\
             (:Requirement {fqn: 'requirements.requirement.reviewed'}) RETURN count(*)";
        const FEEDBACK_ROW: &str = "MATCH (f:Feedback {fqn: 'foo/feedback-1'}) \
             RETURN f.fqn, f.body, f.status, f.disposition";
        const REVIEWED_BODY: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.reviewed'}) \
             RETURN n.body";

        let (repo, wt, wt_apg) = testutil::project_with_db("write-projection-reviews");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A committed durable requirement.
        let mut reviewed = node("requirements", "requirement", "reviewed");
        reviewed.body = "before the update".to_string();
        layers::write_node(&wt_apg, &reviewed).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed node");

        // Seed the Feedback + Reviews pair into the requirements tier mirror
        // BEFORE the scan, so the scan records the same authored digest the
        // write's staleness gate compares against; the write's projection then
        // re-merges it.
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
        testutil::scan_checkout(&wt).unwrap();

        let read = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        // (1) An update of the reviewed node succeeds and its projection
        // re-merges the `.trans` pairing.
        let update = testutil::ApgCommand::new(&[
            "node",
            "update",
            "requirements",
            "requirement",
            "reviewed",
            "--body",
            "updated body",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            update.status.success(),
            "the update must land: {}",
            String::from_utf8_lossy(&update.stderr)
        );
        let reviews_after = read(REVIEWS_EDGE);
        assert!(
            reviews_after.contains("foo/feedback-1")
                && reviews_after.contains("requirements.requirement.reviewed"),
            "the Feedback -[:Reviews]-> reviewed pairing must be projected: {reviews_after}"
        );
        let feedback_after = read(FEEDBACK_ROW);
        assert!(
            feedback_after.contains("foo/feedback-1")
                && feedback_after.contains("review of reviewed"),
            "the Feedback record must be projected: {feedback_after}"
        );
        let body_after = read(REVIEWED_BODY);
        assert!(
            body_after.contains("updated body"),
            "the update must be projected into the branch DB: {body_after}"
        );

        // (2) Destroy `.trans`, re-anchor the scan so the tree's authored
        // digest matches, then remove the reviewed node: the dangling Reviews
        // edge is gone and only the removed node's FQN is dropped.
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
        testutil::scan_checkout(&wt).unwrap();

        let rm =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            rm.status.success(),
            "the remove must land: {}",
            String::from_utf8_lossy(&rm.stderr)
        );
        assert_eq!(
            read(REVIEWS_COUNT).lines().last().map(str::trim),
            Some("0"),
            "the dangling Reviews edge must be gone after the rm"
        );

        testutil::remove(&repo);
    }

    /// A `node rm`'s outstanding-feedback warning is sourced from the
    /// worktree's `.trans/<project>` mirrors — the same source the direct-path
    /// builder reads — so it names each open/actioned item reviewing the node
    /// while the removal completes.
    ///
    /// A CORRUPT project mirror fails that read closed: the rm refuses BEFORE
    /// any durable change (node file, commit and DB untouched, the corrupt file
    /// byte-identical). With the mirror absent the rm proceeds and warns on
    /// nothing. The Feedback records themselves are authoritative and survive
    /// the target's removal in the branch DB.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn rm_warning_is_sourced_from_trans_and_a_corrupt_mirror_fails_closed() {
        const COUNT_A: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.reviewed-a'}) \
             RETURN count(n)";
        const COUNT_B: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.reviewed-b'}) \
             RETURN count(n)";
        const FEEDBACK_A: &str = "MATCH (f:Feedback {fqn: 'foo/feedback-1'}) \
             RETURN f.fqn, f.body, f.status";
        const FEEDBACK_COUNT: &str = "MATCH (f:Feedback) RETURN count(*)";

        let (repo, wt, wt_apg) = testutil::project_with_db("rm-warning-trans");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // Two committed durable requirements, scanned into the branch DB.
        for name in ["reviewed-a", "reviewed-b"] {
            let mut reviewed = node("requirements", "requirement", name);
            reviewed.body = format!("body of {name}");
            layers::write_node(&wt_apg, &reviewed).unwrap();
        }
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed nodes");

        // Both Feedback + Reviews pairs in the requirements tier mirror,
        // written BEFORE the scan so the recorded authored digest matches.
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
        testutil::scan_checkout(&wt).unwrap();

        let read = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        let a_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "reviewed-a");
        let before_commits = testutil::commit_count(&wt);

        // (a) A CORRUPT project mirror fails the rm closed, before any durable
        // change.
        let corrupt_mirror: Vec<u8> = b"\x00not a jsonl record\x01\nfor sure not jsonl\n".to_vec();
        std::fs::write(&mirror, &corrupt_mirror).unwrap();
        let refused =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed-a"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            !refused.status.success(),
            "a corrupt project mirror must refuse the rm before any write"
        );
        let err = String::from_utf8_lossy(&refused.stderr);
        assert!(
            err.contains("foo.jsonl"),
            "the refusal must name the offending file: {err}"
        );
        assert!(
            a_path.exists(),
            "a refused rm must leave the reviewed-a node file in place"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a refused rm must not create a commit"
        );
        assert_eq!(
            std::fs::read(&mirror).unwrap(),
            corrupt_mirror,
            "the refused rm must leave the corrupt mirror byte-identical"
        );
        assert_eq!(
            read(COUNT_A).lines().last().map(str::trim),
            Some("1"),
            "a refused rm must leave the branch DB row present"
        );

        // (b) A valid mirror makes the rm proceed, warning on the open item.
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
        let rm_a =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed-a"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            rm_a.status.success(),
            "the rm with an intact mirror must admit: {}",
            String::from_utf8_lossy(&rm_a.stderr)
        );
        let stderr_a = String::from_utf8_lossy(&rm_a.stderr);
        assert!(
            stderr_a.contains("apg: warning: removing `requirements.requirement.reviewed-a`"),
            "the warning must reach the client's stderr: {stderr_a}"
        );
        assert!(
            stderr_a.contains("foo/feedback-1 (open)"),
            "the warning must name reviewed-a's outstanding feedback: {stderr_a}"
        );
        assert_eq!(
            read(COUNT_A).lines().last().map(str::trim),
            Some("0"),
            "the rm must be projected into the branch DB"
        );
        // The Feedback records are authoritative `.trans` state, so they
        // survive the target's removal; the write's projection re-merged them.
        assert!(
            read(FEEDBACK_A).contains("foo/feedback-1"),
            "the Feedback record must survive the target's removal"
        );
        assert_eq!(
            read(FEEDBACK_COUNT).lines().last().map(str::trim),
            Some("2"),
            "both authoritative Feedback records must survive the removal"
        );

        // (c) With the mirror ABSENT the rm of the fresh reviewed-b proceeds
        // and warns on nothing. Removing the mirror changes the tree's authored
        // digest, so the scan re-anchors the database to the mirror-less tree.
        std::fs::remove_file(&mirror).unwrap();
        assert!(!mirror.exists());
        testutil::scan_checkout(&wt).unwrap();
        let rm_b =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed-b"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            rm_b.status.success(),
            "the rm with an absent mirror must admit: {}",
            String::from_utf8_lossy(&rm_b.stderr)
        );
        let stderr_b = String::from_utf8_lossy(&rm_b.stderr);
        assert!(
            !stderr_b.contains("apg: warning:"),
            "an absent mirror must warn on nothing: {stderr_b}"
        );
        assert_eq!(
            read(COUNT_B).lines().last().map(str::trim),
            Some("0"),
            "the rm must be projected into the branch DB"
        );
        assert!(
            !mirror.exists(),
            "the rm must not recreate the absent mirror"
        );

        testutil::remove(&repo);
    }

    /// A `node rm`'s warning is SCOPED to the current project's `.trans`
    /// mirror, matching the direct builder's
    /// `specs::project_transient_files(project)` scope: a second project's
    /// Feedback reviewing the same node is never named, and the CLI warning
    /// matches the builder's byte-for-byte.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn rm_feedback_warning_is_scoped_to_the_project() {
        const REVIEWED: &str = "requirements.requirement.reviewed";

        let (repo, wt, wt_apg) = testutil::project_with_db("rm-feedback-scope");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A committed durable requirement, scanned into the branch DB.
        let mut reviewed = node("requirements", "requirement", "reviewed");
        reviewed.body = "scoped review".to_string();
        layers::write_node(&wt_apg, &reviewed).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable reviewed node");

        // Project `foo`'s Feedback + Reviews pair (the current project).
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

        // Scan AFTER seeding both mirrors so the recorded authored digest
        // matches the tree the write's staleness gate compares against.
        testutil::scan_checkout(&wt).unwrap();

        // The direct-path builder over the intact `.trans` yields exactly one
        // warning, scoped to `foo` by `project_transient_files(apg_root, "foo")`.
        let args = av(&["rm", "requirements", "requirement", "reviewed"]);
        let direct = build_change(&wt_apg, "node", &args).unwrap();
        assert_eq!(
            direct.warnings.len(),
            1,
            "the builder must yield exactly one warning: {:?}",
            direct.warnings
        );
        let direct_warning = direct.warnings[0].as_str();
        assert!(
            direct_warning.contains("foo/feedback-1 (open)"),
            "the builder's warning must name the project's feedback: {direct_warning}"
        );
        assert!(
            !direct_warning.contains("other"),
            "the builder must not name another project's feedback: {direct_warning}"
        );

        // The CLI rm admits with the same warning, and the other project's
        // feedback is never named.
        let out =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "reviewed"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            out.status.success(),
            "the rm must admit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Removed node requirements.requirement.reviewed"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("other/feedback-1"),
            "the warning must not name another project's feedback: {stderr}"
        );
        let printable = stderr
            .lines()
            .find(|l| l.starts_with("apg: warning:"))
            .expect("the rm must print its outstanding-feedback warning");
        assert_eq!(
            printable, direct_warning,
            "the CLI warning must match the builder's byte-for-byte"
        );

        testutil::remove(&repo);
    }

    /// A corrupt project `.trans` record refuses a durable `node rm` BEFORE any
    /// durable change, naming the offending file, and leaves the held
    /// `db.lbug`, `apg/layers/**` and the git history untouched. Removing the
    /// corrupt file then lets the same rm succeed — proving the refused
    /// invocation released the flock and is side-effect free.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_corrupt_trans_record_refuses_a_rm_before_any_durable_change() {
        const COUNT_SEEDED: &str = "MATCH (n:Requirement {fqn: 'requirements.requirement.seeded'}) \
             RETURN count(n)";

        let (repo, wt, wt_apg) = testutil::project_with_db("corrupt-trans-rm");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A committed durable requirement, scanned into the branch DB — the
        // pre-existing node set the refused write must leave untouched.
        let mut seeded = node("requirements", "requirement", "seeded");
        seeded.body = "seed body".to_string();
        layers::write_node(&wt_apg, &seeded).unwrap();
        testutil::wt_commit_paths(&wt, &["apg/layers"], "seed the durable node");
        testutil::scan_checkout(&wt).unwrap();

        let seeded_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "seeded");
        let durable_before = layers::read_existing_nodes(&wt_apg).unwrap();
        let before_commits = testutil::commit_count(&wt);
        let count_before = spawn_apg(&["query", COUNT_SEEDED], &wt);
        assert_eq!(
            String::from_utf8_lossy(&count_before.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the pre-existing durable node must be in the DB before the refusal"
        );

        // A malformed project mirror planted before the rm: the warning read
        // enumerates the project's transient files and must fail closed.
        let mirror = specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        std::fs::write(&mirror, "garbage plan bytes {[(]} not jsonl\n").unwrap();

        let refused =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "seeded"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            !refused.status.success(),
            "a corrupt .trans record must refuse the rm: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
        let err = String::from_utf8_lossy(&refused.stderr);
        assert!(
            err.contains("foo.jsonl"),
            "the refusal must name the offending file: {err}"
        );

        // Nothing durable moved: the node file, the store, the commits and the
        // branch DB rows are all unchanged.
        assert!(
            seeded_path.exists(),
            "a refused rm must leave the seeded node file in place"
        );
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            durable_before,
            "a refused rm must leave apg/layers/** unchanged"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a refused rm must not create a commit"
        );
        let count_after = spawn_apg(&["query", COUNT_SEEDED], &wt);
        assert_eq!(
            String::from_utf8_lossy(&count_after.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a refused rm must leave the branch DB row present"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "a refused write must leave no session socket"
        );

        // Removing the corrupt file lets the same rm succeed: the refusal
        // released the flock and wrote nothing.
        std::fs::remove_file(&mirror).unwrap();
        let removed =
            testutil::ApgCommand::new(&["node", "rm", "requirements", "requirement", "seeded"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
        assert!(
            removed.status.success(),
            "the rm must succeed once the corrupt file is gone: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
        assert!(
            !seeded_path.exists(),
            "the successful rm must remove the seeded node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the successful rm must land exactly one commit"
        );
        let count_final = spawn_apg(&["query", COUNT_SEEDED], &wt);
        assert_eq!(
            String::from_utf8_lossy(&count_final.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("0"),
            "the successful rm must drop the branch DB row"
        );

        testutil::remove(&repo);
    }

    /// ABSENT `.trans` plan files and tier mirrors are a clean no-op for a
    /// durable write: the invocation still opens the database, writes its node
    /// file, makes exactly one commit, projects the row and succeeds. There is
    /// nothing to seed and nothing to refuse.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn absent_trans_records_are_a_clean_noop_for_a_write() {
        const COUNT_CREATED: &str = "MATCH (n:Constraint {fqn: 'global.constraint.created'}) \
             RETURN count(n)";

        let (wt_apg, repo, wt) = mutation_fixture("absent-trans-write");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // The transient enumerators see only existing files: this fixture
        // carries no plan file and no tier mirror.
        assert!(
            specs::plan_files(&wt_apg).is_empty(),
            "the fixture must start with no .trans plan files"
        );
        assert!(
            specs::trans_mirror_files(&wt_apg).is_empty(),
            "the fixture must start with no .trans tier mirrors"
        );

        let before_commits = testutil::commit_count(&wt);
        let added = spawn_apg(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "created",
                "--body",
                "created without .trans",
            ],
            &wt,
        );
        assert!(
            added.status.success(),
            "the write must succeed with absent .trans: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&added.stdout).trim(),
            "Added node global.constraint.created"
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "created").exists(),
            "the write must land its node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the write must land exactly one commit"
        );
        let count = spawn_apg(&["query", COUNT_CREATED], &wt);
        assert_eq!(
            String::from_utf8_lossy(&count.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the write must be projected into the branch DB"
        );
        assert!(
            specs::plan_files(&wt_apg).is_empty(),
            "the write must not create a plan file"
        );

        testutil::remove(&repo);
    }

    /// A write inside the PROJECT worktree lands durably in that worktree's
    /// own store (node file + one commit + projection). The SAME mutation from
    /// the main checkout refuses client-side for project context and can never
    /// reach the worktree store.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/fs/git/db/process); run via cargo test-e2e"]
    fn worktree_write_lands_in_its_checkout_and_main_never_reaches_it() {
        const COUNT: &str =
            "MATCH (n:Constraint {fqn: 'global.constraint.fresh-rule'}) RETURN count(n)";

        let (wt_apg, repo, wt) = mutation_fixture("worktree-scope");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A whole-model-valid durable base; a lone `global.constraint` has no
        // cardinality minimums.
        layers::write_project(&wt_apg, &testutil::valid_model(), &[]).unwrap();
        let before_commits = testutil::commit_count(&wt);

        let argv = [
            "node",
            "add",
            "global",
            "constraint",
            "fresh-rule",
            "--body",
            "written by the worktree's own invocation",
        ];
        let out = testutil::ApgCommand::new(&argv)
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            out.status.success(),
            "the worktree write must land: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The write is durable at once and visible to a fresh read.
        let rule_path = layers::node_file_path(&wt_apg, Layer::Global, "constraint", "fresh-rule");
        assert!(rule_path.exists(), "the node file must be durable");
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the write must land exactly one commit"
        );
        let read = spawn_apg(&["query", COUNT], &wt);
        assert!(
            read.status.success(),
            "query: {}",
            String::from_utf8_lossy(&read.stderr)
        );
        assert!(
            String::from_utf8_lossy(&read.stdout).contains('1'),
            "the row must be visible in the worktree's DB: {}",
            String::from_utf8_lossy(&read.stdout)
        );

        // The SAME mutation from the main checkout refuses client-side, and can
        // never reach the worktree store.
        let main_out = testutil::spawn_apg(&argv, &repo.root);
        assert_refused_on_main(&main_out, &argv, &["apg scan", "does not exist"]);
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the main-checkout refusal must never reach the worktree store"
        );
        assert!(
            !repo
                .apg_root()
                .join(layers::LAYERS_DIR)
                .join("global")
                .join("constraint")
                .join("fresh-rule.json")
                .exists(),
            "the main checkout must never carry the worktree's node file"
        );

        testutil::remove(&repo);
    }

    /// Inside the PROJECT worktree on the project branch the guard admits a
    /// genuine project context, so all six mutation verbs —
    /// `node add`/`update`/`rm` and `edge add`/`update`/`rm` — each succeed in
    /// their own invocation and land durably (one commit each, projected into
    /// the branch DB). There is no session to start, save or end.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn all_six_mutation_verbs_land_in_their_own_invocations() {
        let (wt_apg, repo, wt) = mutation_fixture("guard-six-mutations");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A whole-model-valid durable base plus one isolated `global.constraint`
        // for `node rm` (a lone constraint has no cardinality minimums).
        let mut base = testutil::valid_model();
        base.push(node("global", "constraint", "retire"));
        layers::write_project(&wt_apg, &base, &[]).unwrap();
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
        // Service).
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

        // Seven invocations, seven commits.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 7,
            "each mutation must land exactly one commit"
        );

        // The durable effects of all six verbs.
        let fresh =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "fresh").unwrap();
        assert_eq!(fresh.body, "fresh body", "the node update landed");
        assert!(!retire_path.exists(), "the node rm landed durably");
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

        testutil::remove(&repo);
    }

    /// A database carrying pending state from an unclean exit is never served
    /// to a durable write: the write's staleness gate refuses it, naming the
    /// pending state and the `apg scan` remedy, before any node file or commit.
    /// A scan discards the unclean database and rebuilds it from the durable
    /// `apg/layers/**` node files, so the phantom projection never becomes
    /// durable. Once the database is clean a write runs without a scan.
    ///
    /// Two injected unclean shapes are covered — a dirty durable row plus an
    /// unsaved row, and a lone pending-removal ledger identity — and the
    /// recovery touches only derived artifacts: `apg/layers/**` stays
    /// byte-identical and no commit lands.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn an_unclean_database_refuses_the_next_write_and_a_scan_repairs_it() {
        const UNSAVED: &str = "requirements.requirement.unsaved";
        const KEEP_A: &str = "requirements.requirement.keep-a";
        const KEEP_B: &str = "requirements.requirement.keep-b";

        let (wt_apg, repo, wt) = node_store_fixture("unclean-db-write");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );

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

        let before_store = layers::read_existing_nodes(&wt_apg).unwrap();
        let before_commits = testutil::commit_count(&wt);
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the baseline DB must be fresh before anything is injected"
        );
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the baseline DB must carry no pending state"
        );

        let last = |out: &str| {
            out.lines()
                .last()
                .map(str::trim)
                .unwrap_or_default()
                .to_string()
        };
        let read = |q: &str| -> String {
            let out = spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "query `{q}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        let committed_rows = |csv: &str| {
            let rows: Vec<String> = csv
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            assert_eq!(
                rows,
                vec!["n.fqn".to_string(), KEEP_A.to_string(), KEEP_B.to_string()],
                "the DB must serve exactly the two committed requirements: {csv}"
            );
            assert!(
                !csv.contains("unsaved"),
                "the DB must never serve the unsaved row: {csv}"
            );
        };
        let refused_write = || {
            let out =
                testutil::ApgCommand::new(&["node", "add", "global", "constraint", "attempt"])
                    .cwd(&wt)
                    .env("HOME", home.to_str().unwrap())
                    .output();
            assert!(
                !out.status.success(),
                "an unclean DB must refuse the durable write"
            );
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("graph is stale"), "{err}");
            assert!(err.contains("pending state"), "{err}");
            assert!(err.contains("apg scan"), "{err}");
            assert!(
                !layers::node_file_path(&wt_apg, Layer::Global, "constraint", "attempt").exists(),
                "a refused write must not write a node file"
            );
            assert_eq!(
                testutil::commit_count(&wt),
                before_commits,
                "a refused write must not create a commit"
            );
        };

        // ---- (1) DIRTY ROW + UNSAVED ROW ---------------------------------
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
        refused_write();

        // A scan repairs it: the pending state is discarded and rebuilt from
        // the durable node files.
        testutil::scan_checkout(&wt).unwrap();
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the rebuilt DB must carry no pending state"
        );
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the rebuild must re-anchor the scan so the DB stays fresh"
        );
        committed_rows(&read("MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn"));
        assert_eq!(
            last(&read(
                "MATCH (n:Requirement) WHERE n.updated = 1 RETURN count(*)"
            )),
            "0",
            "the rebuilt DB must leave no dirty requirement row"
        );
        assert!(
            !layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "unsaved")
                .exists(),
            "the unsaved row never had a node file, and the rebuild writes none"
        );
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the repair must leave apg/layers/** byte-identical"
        );

        // ---- (2) LEDGER IDENTITY ONLY ------------------------------------
        {
            let db = ArtifactDb::open(&wt_apg).unwrap();
            db.record_pending_removal(KEEP_B).unwrap();
        }
        assert!(
            apg::artifacts::db_has_pending_state(&wt_apg),
            "a lone ledger identity is pending state"
        );
        assert!(!apg::git::is_fresh(&wt_apg), "pending state is never fresh");
        refused_write();

        testutil::scan_checkout(&wt).unwrap();
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the rebuilt DB must carry no pending state"
        );
        committed_rows(&read("MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn"));
        assert_eq!(
            layers::read_existing_nodes(&wt_apg).unwrap(),
            before_store,
            "the repair must leave apg/layers/** byte-identical"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "the repairs must create no commit"
        );

        // ---- (3) CLEAN DB: the write runs without a scan -----------------
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the rebuilt DB must be clean before the last write"
        );
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the rebuild must leave the DB fresh"
        );
        let created = spawn_apg(
            &[
                "node",
                "add",
                "global",
                "constraint",
                "created",
                "--body",
                "written without a scan",
            ],
            &wt,
        );
        assert!(
            created.status.success(),
            "a clean DB must accept the write: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Global, "constraint", "created").exists(),
            "the write must land its node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the write must land exactly one commit"
        );
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "a completed write must leave no pending state"
        );
        assert!(
            apg::git::is_fresh(&wt_apg),
            "the write must re-anchor the scan so the DB stays fresh"
        );

        testutil::remove(&repo);
    }

    /// `apg node rm` on a node reviewed by outstanding (open/actioned)
    /// Feedback PROCEEDS — the writer decides — and the write-time warning
    /// naming each unresolved item (`apg: warning: removing `<fqn>` — it is
    /// reviewed by unresolved feedback: …`) reaches the CALLER's stderr while
    /// the removal completes. The Feedback records are authoritative and
    /// survive the target's removal; `apg review list` still reports both items
    /// marked `(removed target)`. A node whose items are all `resolved`, and a
    /// node with no items, remove cleanly with NO warning. Every add/rm is its
    /// own durable invocation.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_rm_warns_on_outstanding_feedback_and_keeps_the_record() {
        let (wt_apg, repo, wt) = mutation_fixture("node-rm-warn");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A durable `args` run in its own invocation, asserting success.
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

        // The warning names each unresolved item on the CLIENT's stderr, while
        // the removal completes.
        let rm = run(&["node", "rm", "requirements", "requirement", "gone"]);
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
            "the removed node must not survive as a node file"
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
        // An all-resolved node removes without the outstanding-feedback warning.
        let rm = run(&["node", "rm", "requirements", "requirement", "calm"]);
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
        run(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "bare",
            "--body",
            "x",
        ]);
        // An unreviewed node removes without a warning too.
        let rm = run(&["node", "rm", "requirements", "requirement", "bare"]);
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

    /// Sequential invocations COMPOSE: each mutation reads the state the
    /// previous invocation committed, so an add followed by an update of the
    /// just-added node and then an edge whose target was added earlier all
    /// succeed. Every step lands its file + one commit, and a fresh read
    /// observes the cumulative result.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn sequential_invocations_compose_over_committed_state() {
        let (wt_apg, repo, wt) = mutation_fixture("compose");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let before_commits = testutil::commit_count(&wt);

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
                "global",
                "constraint",
                "saved",
                "--body",
                "baseline",
            ],
            "Added node global.constraint.saved",
        );
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
        // An update that did NOT compose over the committed add would refuse
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
        // The target `b` was committed by the previous invocation — the edge
        // composes.
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

        // Five invocations, five commits.
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 5,
            "each invocation must land exactly one commit"
        );

        // The cumulative committed result: A carries the updated body and the
        // A→B edge exists.
        let a = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "a").unwrap();
        assert_eq!(a.body, "second");
        assert!(
            a.out
                .iter()
                .any(|e| e.kind == "contains" && e.target == "requirements.requirement.b")
        );
        let b = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "b").unwrap();
        assert!(
            b.in_edges
                .iter()
                .any(|e| e.kind == "contains" && e.source == "requirements.requirement.a")
        );

        let body = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.a'}) RETURN n.body",
            ],
            &wt,
        );
        assert!(
            body.status.success(),
            "query: {}",
            String::from_utf8_lossy(&body.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&body.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("second"),
            "a fresh read must see the composed update"
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
            "query: {}",
            String::from_utf8_lossy(&edge.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&edge.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a fresh read must see the composed edge"
        );

        testutil::remove(&repo);
    }

    /// Each invocation writes EXACTLY its own rows in exactly one commit: one
    /// `apg edge add` rewrites the source out-half's file and the target
    /// in-half's file — and no other node file — as one commit whose parent is
    /// the previous HEAD, so an unchanged node file stays byte-identical and no
    /// file appears for an untouched identity.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn each_invocation_writes_its_own_rows_in_exactly_one_commit() {
        let (wt_apg, repo, wt) = mutation_fixture("write-delta");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

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

        // Snapshot the whole `apg/layers` tree (repo-relative keys), the commit
        // count and HEAD before the edge invocation.
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
        let saved_layers = snapshot(&wt);
        let saved_commits = testutil::commit_count(&wt);
        let saved_head = head();

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

        // EXACTLY the two endpoint rows changed.
        let after_layers = snapshot(&wt);
        let changed: BTreeSet<&str> = after_layers
            .iter()
            .filter(|(path, bytes)| saved_layers.get(*path) != Some(*bytes))
            .map(|(path, _)| path.as_str())
            .collect();
        let expected: BTreeSet<String> = [
            layer_path(Layer::Requirements, "requirement", "alpha"),
            layer_path(Layer::Requirements, "requirement", "beta"),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            changed,
            expected.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            "one edge add must rewrite exactly its two endpoint files"
        );

        // One commit, whose parent is the pre-edge HEAD.
        assert_eq!(
            testutil::commit_count(&wt),
            saved_commits + 1,
            "the edge add must land exactly one commit"
        );
        let git = git2::Repository::open(&wt).unwrap();
        let after_commit = git.head().unwrap().peel_to_commit().unwrap();
        assert_ne!(after_commit.id().to_string(), saved_head, "HEAD must move");
        assert_eq!(
            after_commit.parent(0).unwrap().id().to_string(),
            saved_head,
            "the edge commit's parent must be the previous HEAD"
        );

        // Both halves are durable.
        let alpha =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "alpha").unwrap();
        let beta =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "beta").unwrap();
        assert!(
            alpha
                .out
                .iter()
                .any(|e| e.kind == "contains" && e.target == "requirements.requirement.beta")
        );
        assert!(
            beta.in_edges
                .iter()
                .any(|e| e.kind == "contains" && e.source == "requirements.requirement.alpha")
        );

        testutil::remove(&repo);
    }

    /// The per-invocation write boundary applies the per-node structure rules
    /// and the out-edge matrix, but DEFERS the whole-model minimums to `apg spec
    /// lint` and the merge gate: a Component that is owned and realises a
    /// Service but carries NO Role and no `implemented-by` claim is written by
    /// its own invocation and committed. The deferred claim stays deferred.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn the_write_path_defers_whole_model_minimums_to_lint_and_merge() {
        let (wt_apg, repo, wt) = mutation_fixture("write-whole-model");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A complete, cardinality-consistent durable base (SPEC §4).
        testutil::seed_valid_model(&wt_apg);
        let before_commits = testutil::commit_count(&wt);

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
        // (SPEC §4.4) the per-write boundary does not judge: each mutation is
        // accepted and committed.
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
        assert!(
            component_path.exists(),
            "the write must land the component row's node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 3,
            "each mutation must land exactly one commit"
        );

        // The deferred claim stays deferred: the written Component carries no
        // `implemented-by` out-edge, and the write did not require one.
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

        testutil::remove(&repo);
    }

    /// A per-invocation write has NO unsaved buffer to lose: the node file, the
    /// one commit and the database projection all land in the invocation, so a
    /// fresh read after the process exits opens the committed state and the
    /// database carries no pending state (no dirty row, no pending-removal
    /// ledger identity) and no session socket.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_per_invocation_write_leaves_no_unsaved_buffer_to_lose() {
        let (wt_apg, repo, wt) = node_store_fixture("no-unsaved-buffer");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // The write path validates its code refs against the worktree
        // database and refuses an absent `db.lbug`, so seed a minimal current DB
        // (schema + a `Scan` row at the worktree HEAD) before the write.
        testutil::touch_db(&wt_apg);
        testutil::write_scan_meta(
            &wt_apg,
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
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
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "the baseline DB must carry no pending state"
        );
        let before_commits = testutil::commit_count(&wt);

        let added = spawn_apg(
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "added",
                "--body",
                "durable at once",
            ],
            &wt,
        );
        assert!(
            added.status.success(),
            "the write must succeed: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        assert!(
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "added").exists(),
            "the write must land its node file"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits + 1,
            "the write must land exactly one commit"
        );
        assert!(
            !apg::artifacts::db_has_pending_state(&wt_apg),
            "a completed write must leave no pending state to lose"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("session.sock").exists(),
            "a per-invocation write must leave no session socket"
        );

        // A fresh read after the process exits opens the committed state.
        let q = spawn_apg(
            &["query", "MATCH (n:Requirement) RETURN n.fqn ORDER BY n.fqn"],
            &wt,
        );
        assert!(
            q.status.success(),
            "the fresh read failed: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        let rows: Vec<String> = String::from_utf8(q.stdout)
            .unwrap()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        assert_eq!(
            rows,
            vec![
                "n.fqn".to_string(),
                "requirements.requirement.added".to_string(),
                "requirements.requirement.keep-a".to_string(),
                "requirements.requirement.keep-b".to_string(),
            ],
            "the committed state must be the durable node set"
        );
        let removal = spawn_apg(&["query", "MATCH (n:PendingRemoval) RETURN count(*)"], &wt);
        assert_eq!(
            String::from_utf8_lossy(&removal.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("0"),
            "the DB must carry no pending-removal ledger identity"
        );

        testutil::remove(&repo);
    }

    /// A refused invocation is a byte-identical no-op: a duplicate `node add`
    /// and an update of an absent node both refuse before any write, leaving
    /// the whole `apg/layers/**` tree, HEAD and the commit count unchanged.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn a_refused_invocation_leaves_the_store_byte_identical() {
        let (wt_apg, repo, wt) = mutation_fixture("refused-noop");
        let home = repo.root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A pre-existing durable node file, committed through the validated
        // write path, so "nothing written or changed" is a real claim rather
        // than a vacuous one.
        layers::write_project(&wt_apg, &[node("global", "constraint", "existing")], &[]).unwrap();

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
        let before_layers = snapshot(&wt_apg);
        assert!(
            before_layers.keys().any(|k| k.ends_with("existing.json")),
            "the seeded node file must be on disk: {before_layers:?}"
        );

        // A duplicate add refuses.
        let dup = testutil::ApgCommand::new(&["node", "add", "global", "constraint", "existing"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(!dup.status.success(), "a duplicate add must refuse");
        assert!(
            String::from_utf8_lossy(&dup.stderr).contains("already exists"),
            "{}",
            String::from_utf8_lossy(&dup.stderr)
        );
        // An update of an absent node refuses.
        let ghost = testutil::ApgCommand::new(&[
            "node",
            "update",
            "global",
            "constraint",
            "ghost",
            "--body",
            "absent",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(!ghost.status.success(), "an absent update must refuse");
        assert!(
            String::from_utf8_lossy(&ghost.stderr).contains("does not exist"),
            "{}",
            String::from_utf8_lossy(&ghost.stderr)
        );

        // Nothing durable moved.
        assert_eq!(
            snapshot(&wt_apg),
            before_layers,
            "a refused invocation must leave apg/layers/** byte-identical"
        );
        assert_eq!(
            testutil::commit_count(&wt),
            before_commits,
            "a refused invocation must not create a commit"
        );
        assert_eq!(
            head(),
            before_head,
            "a refused invocation must not move HEAD"
        );

        testutil::remove(&repo);
    }
}
