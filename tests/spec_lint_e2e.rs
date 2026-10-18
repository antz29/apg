//! Relocated e2e tests for the `spec_lint` module: the deterministic
//! `apg spec lint` report (R2/R3/R4 and the shared validator set; the
//! change-set delta gates are plan-verification concerns and are excluded,
//! SPEC §9.3) and the write-surface advisory/refusal hooks (`apg node add` /
//! `apg node update`). Real I/O only; every test is `#[ignore]`d and runs
//! through `cargo test-e2e`.

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

    /// `apg spec lint` reports the deterministic R3/R4 violations and the
    /// shared whole-model cardinalities as ERRORS against a scratch `/tmp`
    /// repo: R3 (two notes detailing one node) and R4 (a note whose `details`
    /// names no node). Delta coverage is NOT lint's concern (SPEC §9.3): the
    /// delta-added requirement and the delta-added `implemented-by` claim below
    /// are touched by no plan phase/task, and lint stays silent about them —
    /// that gate belongs to `apg plan verify`.
    ///
    /// Lint inspects a deliberately-INVALID durable model, so the fixture writes
    /// that model through the direct `write_project` primitive rather than a
    /// live session: the SPEC §8.1 save boundary correctly refuses to persist an
    /// inconsistent whole model (that refusal is pinned in `node_cmd_e2e`), while
    /// lint's job is to READ and report such a model. The read-only lint leaves
    /// the durable store authorable.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn spec_lint_reports_r3_r4_and_skips_delta_gates() {
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

        // Build the real branch DB/graph.jsonl over the seeded code, so the
        // fixture's `implemented-by` code target resolves against the scanned
        // universe.
        testutil::scan_checkout(&wt).unwrap();

        // The intentionally-invalid durable model, written through the direct
        // primitive (a whole-model-invalid store cannot be persisted through a
        // session save — the save boundary refuses it). Both are the write-time
        // legitimate halves the lint then reports:
        //
        // R3 — two implementation-layer notes both detailing the same code node
        // (`global.constraint.note-details-shape`), so the code node holds two
        // notes; R4 — an implementation-layer note whose `details` names no node.
        let mut nodes = Vec::new();
        for name in ["first", "second"] {
            let mut note = testutil::node("implementation", "note", name);
            note.out = vec![testutil::out_edge("details", "go.fixture.mod.Store")];
            nodes.push(note);
        }
        nodes.push(testutil::node("implementation", "note", "orphan"));

        // Delta coverage fixtures — with no plan store the whole branch spec is
        // the merge-base delta (an empty base degenerates to the branch spec):
        // this requirement is delta-added with no `Satisfies`'ing phase, and
        // this solution node's claim is delta-added with no touching task.
        // Ordinary lint excludes those gates (SPEC §9.3); the assertions below
        // pin their absence — `apg plan verify` owns them.
        nodes.push(testutil::node("requirements", "requirement", "uncovered"));
        let mut claim = testutil::node("solution", "component", "claim");
        claim.out = vec![testutil::out_edge("implemented-by", "go.fixture.mod.Store")];
        nodes.push(claim);
        apg::layers::write_project(&apg_root, &nodes, &[]).unwrap();

        // The read-only evidence: capture the delta-added requirement's bytes.
        let uncovered_path = apg::layers::node_file_path(
            &apg_root,
            apg::layers::Layer::Requirements,
            "requirement",
            "uncovered",
        );
        let before = std::fs::read_to_string(&uncovered_path).unwrap();

        let out = testutil::spawn_apg(&["spec", "lint"], &wt);
        assert!(
            !out.status.success(),
            "spec lint must exit non-zero on violations"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        for needle in [
            "R3",
            "go.fixture.mod.Store",
            "implementation.note.first",
            "R4",
            "implementation.note.orphan",
            "requirements.requirement.uncovered",
            "solution.component.claim",
            "go.fixture.mod.Store",
            // The shared whole-model cardinalities surface through lint too.
            "realises no Service",
            "not rooted in a Stakeholder or User",
        ] {
            assert!(
                stderr.contains(needle),
                "spec lint stderr must report `{needle}`:\n{stderr}"
            );
        }
        // Delta coverage is a plan-verification concern (SPEC §9.3): ordinary
        // lint reports neither the claim-coverage gap nor the requirement-
        // Satisfies gap, though both are present in this fixture.
        for absent in [
            "delta gate",
            "is touched by no plan task",
            "Satisfies`'d by no plan phase",
        ] {
            assert!(
                !stderr.contains(absent),
                "spec lint must exclude the delta coverage gate `{absent}`:\n{stderr}"
            );
        }

        // Read-only: the lint never rewrites a durable node file...
        assert_eq!(
            std::fs::read_to_string(&uncovered_path).unwrap(),
            before,
            "apg spec lint is read-only — it must not rewrite node files"
        );

        // ...and the store stays authorable afterwards: a further direct write
        // (a lone, whole-model-valid `solution.role`) succeeds and leaves the
        // model as it was otherwise.
        apg::layers::write_project(
            &apg_root,
            &[testutil::node("solution", "role", "auditor")],
            &[],
        )
        .unwrap();
        assert!(
            apg::layers::node_file_path(&apg_root, apg::layers::Layer::Solution, "role", "auditor")
                .exists(),
            "the durable store must stay authorable after a read-only lint"
        );

        testutil::remove(&repo);
    }

    /// Phase-03 task-12: `apg node add` and `apg node update` on a tier node
    /// whose `--body` carries negation/future/time-relative wording still
    /// SUCCEED — the writer decides, the advisory never blocks a write — and
    /// land their node files; AND the routed write-time wording advisory
    /// (`crate::spec_lint::WORDING_ADVISORY`, R1/R5) reaches the CLIENT's stderr
    /// (`apg: warning: <fqn>: …`) while the write completes. Plain present-tense
    /// wording prints no warning and is accepted too. Covers the write-surface
    /// hooks `rust.apg.node_cmd.node_add_change` / `node_update_change`, the
    /// session's warning carrier, and `cmd_node`'s caller-side print.
    ///
    /// Durable mutations are mandatory-session: the add/updates run under one
    /// live `apg session start` and `apg session save` is the single durability
    /// point the node-file/read-back assertions below observe.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/db.lbug/git/process); run via cargo test-e2e"]
    fn node_add_and_update_emit_advisory_wording_warning() {
        let (apg_root, repo, wt) = wt_fixture("advisory");
        let home = repo.root.join("home");
        let path = apg::layers::node_file_path(
            &apg_root,
            apg::layers::Layer::Solution,
            "role",
            "demo-val",
        );

        // A session always holds a database, so seed a committed baseline and
        // scan it before opening the session — the same write-then-scan pattern
        // the crash/reclaim test uses. The base node is a lone `solution.role`
        // (unconstrained), so the seeded store is whole-model-valid (SPEC §8.1).
        apg::layers::write_project(
            &apg_root,
            &[testutil::node("solution", "role", "seed")],
            &[],
        )
        .unwrap();
        testutil::scan_checkout(&wt).unwrap();

        // Durable mutations are mandatory-session: one live `apg session start`
        // owns the DB AND the write-back buffer, so every `node` write below is
        // forwarded to it and staged (NON-durable until `apg session save`).
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
        // The single durability point: `apg session save` flushes the buffered
        // set into node files so the file/read-back assertions observe it.
        let save = |wt: &Path| {
            let out = testutil::spawn_apg(&["session", "save"], wt);
            assert!(
                out.status.success(),
                "session save failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Session saved");
        };

        // `node add`: time-relative wording (`was`) still succeeds — an
        // advisory, never a refusal — and the node file lands on save.
        let out = run(&[
            "node",
            "add",
            "solution",
            "role",
            "demo-val",
            "--body",
            "The old flow was synchronous.",
        ]);
        assert!(
            out.status.success(),
            "the advisory must not block the add: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The advisory is routed back through the session reply and printed on
        // the CLIENT's stderr, naming the mutated node — while the write
        // completes (the success assert above).
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("apg: warning: solution.role.demo-val:"),
            "the warning must name the mutated node on the client's stderr: {stderr}"
        );
        assert!(
            stderr.contains(apg::spec_lint::WORDING_ADVISORY),
            "the wording advisory must reach the client's stderr: {stderr}"
        );
        save(&wt);
        assert!(
            path.exists(),
            "the flagged-but-advisory write must still land its node file"
        );

        // `node update`: future-tense wording (`will`) still succeeds; the body
        // is stored.
        let out = run(&[
            "node",
            "update",
            "solution",
            "role",
            "demo-val",
            "--body",
            "The service will retry the request.",
        ]);
        assert!(
            out.status.success(),
            "the advisory must not block the update: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The future-tense (`will`) advisory also reaches the client's stderr
        // while the update completes.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("apg: warning: solution.role.demo-val:"),
            "the warning must name the mutated node on the client's stderr: {stderr}"
        );
        assert!(
            stderr.contains(apg::spec_lint::WORDING_ADVISORY),
            "the wording advisory must reach the client's stderr: {stderr}"
        );
        save(&wt);
        let back = apg::layers::read_node_file(
            &apg_root,
            apg::layers::Layer::Solution,
            "role",
            "demo-val",
        )
        .unwrap();
        assert_eq!(back.body, "The service will retry the request.");

        // Plain present-tense wording is accepted too; the body is stored. Its
        // accepted wording is NOT flagged, so the client's stderr carries no
        // warning — the advisory is body-driven, not unconditional.
        let out = run(&[
            "node",
            "update",
            "solution",
            "role",
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
        save(&wt);
        let back = apg::layers::read_node_file(
            &apg_root,
            apg::layers::Layer::Solution,
            "role",
            "demo-val",
        )
        .unwrap();
        assert_eq!(back.body, "The service stores the record.");

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

    /// Drop one out-edge (and its matching in half) from an in-memory model,
    /// keeping the fixture pairing-consistent for `write_project`.
    fn drop_edge(
        nodes: &mut [apg::layers::NodeFile],
        layer: &str,
        node_type: &str,
        name: &str,
        kind: &str,
        target: &str,
    ) {
        let source = format!("{layer}.{node_type}.{name}");
        nodes
            .iter_mut()
            .find(|n| n.layer == layer && n.node_type == node_type && n.name == name)
            .unwrap_or_else(|| panic!("fixture node {source} is missing"))
            .out
            .retain(|e| !(e.kind == kind && e.target == target));
        nodes
            .iter_mut()
            .find(|n| format!("{}.{}.{}", n.layer, n.node_type, n.name) == target)
            .unwrap_or_else(|| panic!("fixture node {target} is missing"))
            .in_edges
            .retain(|e| !(e.kind == kind && e.source == source));
    }

    /// The `--tier` scope is separate from context (SPEC §9.2/§9.4): a Domain
    /// cardinality violation is reported by domain and all-tier lint and is
    /// invisible to requirements lint; every tier scope passes the complete
    /// new-model fixture. The shared validator's cardinality errors surface
    /// through the binary, and the usage errors are refused.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/git/process); run via cargo test-e2e"]
    fn spec_lint_tier_scopes_domain_violations() {
        let repo = Repo::new("speclint-tier-domain");
        let wt = repo.start_project("foo");
        let apg_root = wt.join(specs::LAYOUT);
        testutil::seed_valid_model(&apg_root);

        // The complete model passes every single-tier scope (a missing
        // Component implemented-by is the sole deferred obligation, §8.2).
        for tier in ["requirements", "domain", "solution"] {
            let out = testutil::spawn_apg(&["spec", "lint", "--tier", tier], &wt);
            assert!(
                out.status.success(),
                "the complete model must pass `--tier {tier}`: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // Break the domain: the driving Requirement no longer points at its
        // Service. Both endpoint files are rewritten so pairing stays intact.
        let mut nodes = testutil::valid_model();
        drop_edge(
            &mut nodes,
            "requirements",
            "requirement",
            "place-order",
            "drives",
            "domain.service.place-order",
        );
        let changed: Vec<apg::layers::NodeFile> = nodes
            .into_iter()
            .filter(|n| {
                (n.layer == "requirements"
                    && n.node_type == "requirement"
                    && n.name == "place-order")
                    || (n.layer == "domain" && n.node_type == "service" && n.name == "place-order")
            })
            .collect();
        apg::layers::write_project(&apg_root, &changed, &[]).unwrap();

        let rule = "Service `domain.service.place-order` is driven by no Requirement";
        let domain = testutil::spawn_apg(&["spec", "lint", "--tier", "domain"], &wt);
        assert!(!domain.status.success());
        assert!(
            String::from_utf8_lossy(&domain.stderr).contains(rule),
            "domain lint must report the domain cardinality violation: {}",
            String::from_utf8_lossy(&domain.stderr)
        );

        let requirements = testutil::spawn_apg(&["spec", "lint", "--tier", "requirements"], &wt);
        assert!(
            requirements.status.success(),
            "requirements lint must not require domain completeness: {}",
            String::from_utf8_lossy(&requirements.stderr)
        );

        // All-tier lint reports the same shared-validator error.
        let all = testutil::spawn_apg(&["spec", "lint"], &wt);
        assert!(!all.status.success());
        assert!(
            String::from_utf8_lossy(&all.stderr).contains(rule),
            "all-tier lint must report the same error: {}",
            String::from_utf8_lossy(&all.stderr)
        );

        testutil::remove(&repo);
    }

    /// Scope, mirror image (SPEC §9.2): a Requirements cardinality violation is
    /// reported by requirements and all-tier lint and is invisible to domain
    /// lint.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/git/process); run via cargo test-e2e"]
    fn spec_lint_tier_scopes_requirements_violations() {
        let repo = Repo::new("speclint-tier-req");
        let wt = repo.start_project("foo");
        let apg_root = wt.join(specs::LAYOUT);
        testutil::seed_valid_model(&apg_root);

        // The User no longer requires a Requirement (its only `requires` edge).
        let mut nodes = testutil::valid_model();
        drop_edge(
            &mut nodes,
            "requirements",
            "user",
            "customer",
            "requires",
            "requirements.requirement.place-order",
        );
        let changed: Vec<apg::layers::NodeFile> = nodes
            .into_iter()
            .filter(|n| {
                (n.layer == "requirements" && n.node_type == "user" && n.name == "customer")
                    || (n.layer == "requirements"
                        && n.node_type == "requirement"
                        && n.name == "place-order")
            })
            .collect();
        apg::layers::write_project(&apg_root, &changed, &[]).unwrap();

        let rule = "`requirements.user.customer` requires no Requirement";
        let requirements = testutil::spawn_apg(&["spec", "lint", "--tier", "requirements"], &wt);
        assert!(!requirements.status.success());
        assert!(
            String::from_utf8_lossy(&requirements.stderr).contains(rule),
            "requirements lint must report the requirements violation: {}",
            String::from_utf8_lossy(&requirements.stderr)
        );

        let domain = testutil::spawn_apg(&["spec", "lint", "--tier", "domain"], &wt);
        assert!(
            domain.status.success(),
            "domain lint must not require requirements completeness: {}",
            String::from_utf8_lossy(&domain.stderr)
        );

        testutil::remove(&repo);
    }

    /// The `apg spec lint` usage surface (SPEC §9.4): unknown flags, a repeated
    /// `--tier`, a missing/invalid value, and unexpected positionals are usage
    /// errors; `--tier implementation` is outside the tier-1-3 document.
    #[test]
    #[ignore = "e2e tier: real I/O (process/git); run via cargo test-e2e"]
    fn spec_lint_refuses_bad_tier_usage() {
        let repo = Repo::new("speclint-tier-usage");
        let wt = repo.start_project("foo");
        for args in [
            vec!["spec", "lint", "--tier"],
            vec!["spec", "lint", "--tier", "implementation"],
            vec!["spec", "lint", "--tier", "bogus"],
            vec!["spec", "lint", "--tier", "domain", "--tier", "solution"],
            vec!["spec", "lint", "--bogus"],
            vec!["spec", "lint", "extra"],
        ] {
            let out = testutil::spawn_apg(&args, &wt);
            assert!(!out.status.success(), "`{args:?}` must be a usage error");
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("usage:"),
                "`{args:?}` must report a usage error: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        testutil::remove(&repo);
    }

    /// A tier-scoped constraint is prose-only and needs no attachment: a NEW
    /// `domain.constraint.*` with an arbitrary metadata property is ADMITTED
    /// (the model has no constraint-attachment edge or property), and its node
    /// file lands on save.
    ///
    /// Every durable `node` write is routed through one live `apg session
    /// start` and staged in its write-back buffer; `apg session save` is the
    /// single durability point the file/read-back assertions below observe.
    #[test]
    #[ignore = "e2e tier: real I/O (node files/git/process); run via cargo test-e2e"]
    fn tier_scoped_constraint_with_metadata_is_authorable() {
        let (apg_root, repo, wt) = wt_fixture("attaches");
        let home = repo.root.join("home");

        // A session always holds a database, so seed a committed baseline and
        // scan it before opening the session. The base node is a lone
        // `solution.role` — an unconstrained type, so the seeded store is
        // whole-model-valid (SPEC §8.1) on its own.
        apg::layers::write_project(
            &apg_root,
            &[testutil::node("solution", "role", "seed")],
            &[],
        )
        .unwrap();
        testutil::scan_checkout(&wt).unwrap();

        // Durable mutations are mandatory-session.
        let session = testutil::start_session_process(&wt, &home);
        assert!(
            apg::session::live_session(&apg_root),
            "the durable mutations must run under a live session"
        );

        let run = |args: &[&str]| -> std::process::Output {
            testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output()
        };

        // A NEW domain-tier constraint, with arbitrary metadata, is accepted.
        let law_path =
            apg::layers::node_file_path(&apg_root, apg::layers::Layer::Domain, "constraint", "law");
        let out = run(&[
            "node",
            "add",
            "domain",
            "constraint",
            "law",
            "--property",
            "scope=global",
        ]);
        assert!(
            out.status.success(),
            "a tier-scoped constraint must be authorable: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The single durability point: `apg session save` flushes the buffered
        // set (the plain law) into node files in one commit.
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

        // Durable result: the constraint's node file landed with its metadata.
        assert!(
            law_path.exists(),
            "the plain constraint's node file must land"
        );
        let back =
            apg::layers::read_node_file(&apg_root, apg::layers::Layer::Domain, "constraint", "law")
                .unwrap();
        assert_eq!(
            back.properties.get("scope").map(String::as_str),
            Some("global")
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
