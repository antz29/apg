//! Relocated e2e tests for the `spec_lint` module: the deterministic
//! `apg spec lint` report (R2/R3/R4 + the change-set delta gates) and the
//! write-surface advisory/refusal hooks (`apg node add` / `apg node update`).
//! Real I/O only; every test is `#[ignore]`d and runs through `cargo test-e2e`.

mod common;

use apg::specs;
use apg::testutil::{self, Repo, wt_commit};
use std::path::{Path, PathBuf};

/// The module/file namespace the hermetic scan payload uses.
const SCAN_MOD: &str = "fixture.mod";
const SCAN_FILE: &str = "/abs/store.go";

/// A project context for the lint/write-surface tests: a real git repo whose
/// worktree `foo` on branch `foo` hosts the durable `apg/layers` store.
/// Returns `(wt_apg_root, repo, wt_root)`.
fn wt_fixture(tag: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = Repo::new(&format!("speclint-{tag}"));
    let wt = repo.start_project("foo");
    let wt_apg = wt.join(specs::LAYOUT);
    (wt_apg, repo, wt)
}

/// Writes `content` to `<wt>/<rel>`, creating parent dirs.
fn wt_write(wt: &Path, rel: &str, content: &str) {
    let p = wt.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// e2e tier -- real I/O: every test here writes node files / `apg/layers`
/// trees under a scratch `/tmp` git repo, opens `db.lbug`, runs git, or spawns
/// the built `apg` binary. Each is `#[ignore]`d, so a plain `cargo test` never
/// runs one; the only entry point is the named guard `cargo test-e2e`.
mod e2e {
    use super::*;

    /// `apg spec lint` reports the deterministic R2/R3/R4 violations and the
    /// change-set delta gates as ERRORS against a scratch `/tmp` repo: R2 (a
    /// constraint carrying `attaches-to`), R3 (two notes detailing one node),
    /// R4 (a note whose `details` names no node), a delta-added requirement no
    /// phase `Satisfies` (named by requirement FQN), and a delta-added
    /// `implemented-by` claim no plan task touches. The read-only lint leaves
    /// the durable graph authorable.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn spec_lint_reports_r2_r3_r4_and_delta_gate_violations() {
        let (apg_root, repo, wt) = wt_fixture("report");

        // Real scanned code: the branch gets a current `db.lbug`/`graph.jsonl`
        // and the solution node's `implemented-by` target (`go.fixture.mod.Store`)
        // resolves against the scanned universe.
        wt_write(
            &wt,
            "code/seed.scan.jsonl",
            &testutil::code_payload(SCAN_MOD, SCAN_FILE, &["Store"]),
        );
        wt_commit(&wt, &["code/seed.scan.jsonl"], "seed code");

        // R2 — a constraint carrying the off-model `attaches-to` property. It is
        // written RAW (the write surface refuses a NEW attached constraint; that
        // refusal is pinned by `new_constraint_attaches_to_is_refused`).
        let mut bad_scope = testutil::node("requirements", "constraint", "bad-scope");
        bad_scope.properties.insert(
            apg::layers::PROP_ATTACHES_TO.to_string(),
            "domain.value.thing".to_string(),
        );
        apg::layers::write_node(&apg_root, &bad_scope).unwrap();

        // R3 — two notes both detailing the same non-note node (paired halves).
        let mut thing = testutil::node("domain", "value", "thing");
        for name in ["first", "second"] {
            let mut note = testutil::node("requirements", "note", name);
            note.out
                .push(testutil::out_edge("details", "domain.value.thing"));
            apg::layers::write_node(&apg_root, &note).unwrap();
            thing.in_edges.push(testutil::in_edge(
                "details",
                &format!("requirements.note.{name}"),
            ));
        }
        apg::layers::write_node(&apg_root, &thing).unwrap();

        // R4 — a note whose `details` names no node (zero out-edges).
        apg::layers::write_node(&apg_root, &testutil::node("requirements", "note", "orphan"))
            .unwrap();

        // Delta gates — with no plan store the whole branch spec is the
        // merge-base delta (an empty base degenerates to the branch spec): this
        // requirement is delta-added with no `Satisfies`'ing phase, and this
        // solution node's claim is delta-added with no touching task.
        apg::layers::write_node(
            &apg_root,
            &testutil::node("requirements", "requirement", "uncovered"),
        )
        .unwrap();
        let mut claim = testutil::node("solution", "system", "claim");
        claim
            .out
            .push(testutil::out_edge("implemented-by", "go.fixture.mod.Store"));
        apg::layers::write_node(&apg_root, &claim).unwrap();

        // Build the real branch DB/graph.jsonl over the authored tree.
        testutil::scan_checkout(&wt).unwrap();

        // The read-only evidence: capture the constraint file's bytes.
        let bad_scope_path = apg::layers::node_file_path(
            &apg_root,
            apg::layers::Layer::Requirements,
            "constraint",
            "bad-scope",
        );
        let before = std::fs::read_to_string(&bad_scope_path).unwrap();

        let out = testutil::spawn_apg(&["spec", "lint"], &wt);
        assert!(
            !out.status.success(),
            "spec lint must exit non-zero on violations"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        for needle in [
            "R2",
            "requirements.constraint.bad-scope",
            "R3",
            "domain.value.thing",
            "R4",
            "requirements.note.orphan",
            "delta gate",
            "requirements.requirement.uncovered",
            "solution.system.claim",
            "go.fixture.mod.Store",
        ] {
            assert!(
                stderr.contains(needle),
                "spec lint stderr must report `{needle}`:\n{stderr}"
            );
        }

        // Read-only: the lint never rewrites a durable node file...
        assert_eq!(
            std::fs::read_to_string(&bad_scope_path).unwrap(),
            before,
            "apg spec lint is read-only — it must not rewrite node files"
        );
        // ...and the graph stays authorable afterwards.
        let added = testutil::spawn_apg(&["node", "add", "requirements", "user", "auditor"], &wt);
        assert!(
            added.status.success(),
            "the durable graph must stay authorable after lint: {}",
            String::from_utf8_lossy(&added.stderr)
        );

        testutil::remove(&repo);
    }

    /// `apg node add` and `apg node update` on a tier node whose `--body`
    /// carries negation/future/time-relative wording emit the shared advisory
    /// (naming the re-phrase option) and still SUCCEED — the writer decides.
    /// Plain present-tense wording is silent. Covers the write-surface hooks
    /// `rust.apg.node_cmd.node_add_change` / `node_update_change`.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/git/process); run via cargo test-e2e"]
    fn node_add_and_update_emit_advisory_wording_warning() {
        let (apg_root, repo, wt) = wt_fixture("advisory");
        let path =
            apg::layers::node_file_path(&apg_root, apg::layers::Layer::Domain, "value", "demo-val");

        // `node add`: time-relative wording (`was`) emits the advisory AND
        // still succeeds — advisory, never a refusal.
        let out = testutil::spawn_apg(
            &[
                "node",
                "add",
                "domain",
                "value",
                "demo-val",
                "--body",
                "The old flow was synchronous.",
            ],
            &wt,
        );
        assert!(
            out.status.success(),
            "the advisory must not block the add: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(apg::spec_lint::WORDING_ADVISORY),
            "node add must emit the advisory:\n{stderr}"
        );
        assert!(
            path.exists(),
            "the flagged-but-advisory write must still land its node file"
        );

        // `node update`: future-tense wording (`will`) emits the advisory AND
        // still succeeds; the body is stored.
        let out = testutil::spawn_apg(
            &[
                "node",
                "update",
                "domain",
                "value",
                "demo-val",
                "--body",
                "The service will retry the request.",
            ],
            &wt,
        );
        assert!(
            out.status.success(),
            "the advisory must not block the update: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(apg::spec_lint::WORDING_ADVISORY),
            "node update must emit the advisory:\n{stderr}"
        );
        let back =
            apg::layers::read_node_file(&apg_root, apg::layers::Layer::Domain, "value", "demo-val")
                .unwrap();
        assert_eq!(back.body, "The service will retry the request.");

        // Plain present-tense wording is silent.
        let out = testutil::spawn_apg(
            &[
                "node",
                "update",
                "domain",
                "value",
                "demo-val",
                "--body",
                "The service stores the record.",
            ],
            &wt,
        );
        assert!(
            out.status.success(),
            "a plain update must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains(apg::spec_lint::WORDING_ADVISORY),
            "plain present-tense wording must be silent:\n{stderr}"
        );

        testutil::remove(&repo);
    }

    /// The write-surface `attaches-to` refusal (`rust.apg.layers.write.validate_change`),
    /// exercised under mandatory-session: a NEW constraint carrying
    /// `attaches-to` is REFUSED by the session coordinator's admission
    /// validation with no node file left behind; an EXISTING attached
    /// constraint stays authorable (`node update` of its body and `edge add
    /// details <note> <constraint>` targeting it both succeed, because its
    /// `attaches-to` value is unchanged); and a NEW constraint WITHOUT
    /// `attaches-to` (a plain tier-scoped `domain.constraint.*`) SUCCEEDS and
    /// lands its file.
    ///
    /// Every durable `node`/`edge` write is routed through one live
    /// `apg session start` and staged in its write-back buffer; `apg session
    /// save` is the single durability point the file/read-back assertions below
    /// observe.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/git/process); run via cargo test-e2e"]
    fn new_constraint_attaches_to_is_refused() {
        let (apg_root, repo, wt) = wt_fixture("attaches");
        let home = repo.root.join("home");

        // Seed an EXISTING attached constraint RAW and commit it: the write
        // surface refuses a NEW attached constraint, so the legacy one predates
        // the mutation under test.
        let mut legacy = testutil::node("requirements", "constraint", "legacy");
        legacy.properties.insert(
            apg::layers::PROP_ATTACHES_TO.to_string(),
            "domain.value.thing".to_string(),
        );
        apg::layers::write_node(&apg_root, &legacy).unwrap();
        wt_commit(
            &wt,
            &["apg/layers/requirements/constraint/legacy.json"],
            "seed the legacy attached constraint",
        );

        // Durable mutations are mandatory-session: one live `apg session start`
        // owns the DB AND the write-back buffer, so every `node`/`edge` write
        // below is forwarded to it and staged (NON-durable until `apg session
        // save`).
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&apg_root),
            "the durable mutations must run under a live session"
        );

        // A routed durable write: the client forwards to the live session
        // (buffered), with the isolated HOME the session was started under.
        let run = |args: &[&str]| -> std::process::Output {
            testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output()
        };

        // A NEW constraint carrying `attaches-to` is REFUSED at admission — no
        // buffered write, so no file can ever land.
        let fresh_path = apg::layers::node_file_path(
            &apg_root,
            apg::layers::Layer::Requirements,
            "constraint",
            "fresh",
        );
        let out = run(&[
            "node",
            "add",
            "requirements",
            "constraint",
            "fresh",
            "--property",
            "attaches-to=domain.value.thing",
        ]);
        assert!(
            !out.status.success(),
            "a NEW attached constraint must be refused"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("attaches-to"),
            "the refusal must name `attaches-to`:\n{stderr}"
        );
        assert!(
            !fresh_path.exists(),
            "a refused constraint write must leave no node file behind"
        );

        // An EXISTING attached constraint stays authorable: `node update` of its
        // body (attaches-to unchanged) is ADMITTED into the buffer.
        let out = run(&[
            "node",
            "update",
            "requirements",
            "constraint",
            "legacy",
            "--body",
            "The rule holds.",
        ]);
        assert!(
            out.status.success(),
            "an existing attached constraint must stay updatable: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // `edge add details <note> <constraint>` targeting it also SUCCEEDS: the
        // constraint's in-half write carries its unchanged `attaches-to`. The
        // edge's note endpoint is added earlier in the same unsaved run, so the
        // admission resolves it against the cumulative buffered state.
        let out = run(&["node", "add", "requirements", "note", "audit"]);
        assert!(
            out.status.success(),
            "the details source note must be authorable: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = run(&[
            "edge",
            "add",
            "details",
            "requirements.note.audit",
            "requirements.constraint.legacy",
        ]);
        assert!(
            out.status.success(),
            "edge add details to an existing attached constraint must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The positive case: a NEW constraint WITHOUT `attaches-to` is ADMITTED
        // — the refusal targets only a newly-added attached constraint, never
        // every new constraint.
        let law_path =
            apg::layers::node_file_path(&apg_root, apg::layers::Layer::Domain, "constraint", "law");
        let out = run(&["node", "add", "domain", "constraint", "law"]);
        assert!(
            out.status.success(),
            "a plain tier-scoped constraint must be authorable: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The single durability point: `apg session save` flushes the buffered
        // set (the legacy update, the note, the edge halves, the plain law)
        // into node files in one commit.
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

        // Durable result: the refused write left no file, while the legacy
        // update (its `attaches-to` preserved) and the plain constraint landed.
        assert!(
            !fresh_path.exists(),
            "a refused constraint write must leave no node file behind after save"
        );
        let back = apg::layers::read_node_file(
            &apg_root,
            apg::layers::Layer::Requirements,
            "constraint",
            "legacy",
        )
        .unwrap();
        assert_eq!(
            back.properties
                .get(apg::layers::PROP_ATTACHES_TO)
                .map(String::as_str),
            Some("domain.value.thing"),
            "the update must preserve the existing `attaches-to`"
        );
        assert_eq!(back.body, "The rule holds.");
        assert!(
            law_path.exists(),
            "the plain constraint's node file must land"
        );

        // End the session cleanly.
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let sout = session.child.wait_with_output().unwrap();
        assert!(
            sout.status.success(),
            "{}",
            String::from_utf8_lossy(&sout.stderr)
        );
        assert!(
            !apg::session::live_session(&apg_root),
            "the session must be ended cleanly"
        );

        testutil::remove(&repo);
    }
}
