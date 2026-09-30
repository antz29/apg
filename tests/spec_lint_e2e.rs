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
}
