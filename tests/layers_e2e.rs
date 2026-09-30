//! Relocated e2e tests for the `layers` module (from `src/layers.rs`'s inline
//! `#[cfg(test)] mod tests`). Real I/O only; every test is `#[ignore]`d and
//! runs through `cargo test-e2e`.

mod common;

use apg::artifacts;
use apg::layers::*;
use apg::schema::Record;
use apg::testutil::{self, Repo, code_universe, in_edge, node, out_edge};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The module/fqn namespace the scan payloads use.
const SCAN_MOD: &str = "fixture.mod";
const SCAN_FILE: &str = "/abs/store.go";

/// A unique temp dir for one test (removed on cleanup) — the node-file
/// writer is the first I/O in this module, so tests stage under
/// `std::env::temp_dir()` like specs.rs/git.rs do.
fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("apg-layers-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A sample requirement node file mirroring the §4.1 example.
fn sample_node() -> NodeFile {
    NodeFile {
        layer: "requirements".to_string(),
        node_type: "requirement".to_string(),
        name: "place-order".to_string(),
        body: "A customer can place an order.".to_string(),
        properties: BTreeMap::new(),
        out: vec![OutEdge {
            kind: "drives".to_string(),
            target: "domain.service.checkout".to_string(),
            properties: BTreeMap::new(),
        }],
        in_edges: vec![InEdge {
            kind: "contains".to_string(),
            source: "requirements.user.customer".to_string(),
            properties: BTreeMap::new(),
        }],
    }
}

/// The small paired node set the rewrite regression authors: A --contains-->
/// B, with A's out and B's in matching (the SPEC §4.1 pairwise invariant).
fn authored_pair(a_name: &str, b_name: &str) -> (NodeFile, NodeFile) {
    let a_fqn = fqn(Layer::Requirements, "requirement", a_name);
    let b_fqn = fqn(Layer::Requirements, "requirement", b_name);
    let mut a = node("requirements", "requirement", a_name);
    a.out.push(out_edge("contains", &b_fqn));
    let mut b = node("requirements", "requirement", b_name);
    b.in_edges.push(in_edge("contains", &a_fqn));
    (a, b)
}

/// Read one node file back from its derived path (the file name IS the
/// identity) — the post-rewrite state the pairing check runs on.
fn read_node_file(root: &Path, layer: &str, node_type: &str, name: &str) -> NodeFile {
    let path = root
        .join("layers")
        .join(layer)
        .join(node_type)
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Write a set of node files under `<root>/layers/…` at their derived
/// paths — the durable tree `ingest_tree` walks.
fn write_tree(root: &Path, nodes: &[NodeFile]) {
    for n in nodes {
        write_node(root, n).unwrap();
    }
}

/// A sample spine tree: a requirement `drives` a group, the group is
/// `realised-by` a system, the system is `implemented-by` a code FQN.
/// Fully paired in/out halves.
fn sample_tree() -> Vec<NodeFile> {
    let mut req = node("requirements", "requirement", "place-order");
    req.out.push(out_edge("drives", "domain.group.sales"));
    let mut grp = node("domain", "group", "sales");
    grp.in_edges
        .push(in_edge("drives", "requirements.requirement.place-order"));
    grp.out
        .push(out_edge("realised-by", "solution.system.payments"));
    let mut sys = node("solution", "system", "payments");
    sys.in_edges
        .push(in_edge("realised-by", "domain.group.sales"));
    sys.out.push(out_edge("implemented-by", "apg.main"));
    vec![req, grp, sys]
}

/// A real project context for `write_project` tests: the scan fixture's
/// repo plus a worktree `foo` branched off it and a branch DB built by the
/// hermetic scan (mirrors node_cmd's mutation fixture). Returns
/// `(wt_apg_root, repo, wt_root)`.
fn mutation_fixture(tag: &str) -> (PathBuf, Repo, PathBuf) {
    let repo = scan_repo(tag);
    let wt = repo.start_project("foo");
    testutil::scan_checkout(&wt).unwrap();
    (wt.join(apg::specs::LAYOUT), repo, wt)
}

/// A fixture repo carrying a scanned-code payload, committed.
fn scan_repo(tag: &str) -> Repo {
    let repo = Repo::new(&format!("layers-scan-{tag}"));
    repo.write(
        "code/seed.scan.jsonl",
        &apg::testutil::code_payload(SCAN_MOD, SCAN_FILE, &["Store"]),
    );
    repo.commit_all("seed code");
    repo
}

/// e2e tier -- real I/O: every test here writes node files / `apg/layers`
/// trees under the temp dir, reads them back, or opens `db.lbug` / runs
/// git. Each is `#[ignore]`d, so a plain `cargo test` never runs one; the
/// only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    // --- Node-file schema + single-node writer (phase-3 task-8) ---

    /// write_node writes one file at
    /// `<root>/layers/<layer>/<type>/<name>.json`; the layer/type/name/body/
    /// properties/out/in round-trip (write → read → deserialize == original),
    /// and the FQN derived from the path's segments equals layer.type.name.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_node_writes_file_with_round_tripping_identity() {
        let root = temp_root("roundtrip");
        let node = sample_node();
        let path = write_node(&root, &node).unwrap();
        assert_eq!(
            path,
            root.join("layers")
                .join("requirements")
                .join("requirement")
                .join("place-order.json")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let back: NodeFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, node);
        // The file name IS the identity: the FQN is the path's segments.
        assert_eq!(
            fqn(Layer::Requirements, &back.node_type, &back.name),
            "requirements.requirement.place-order"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A plans-layer node is refused — plans is transient (apg/.trans/plans/),
    /// never a durable node-file layer — and nothing is written.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_node_refuses_plans_layer() {
        let root = temp_root("plans");
        let mut node = sample_node();
        node.layer = "plans".to_string();
        node.node_type = "task".to_string();
        let err = write_node(&root, &node).unwrap_err().to_string();
        assert!(err.contains("plans"), "{err}");
        assert!(!root.join("layers").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An allowlist-violating name is refused (never sanitized) — the file name
    /// must stay safe. Also covers an unknown layer.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_node_refuses_allowlist_violating_name() {
        let root = temp_root("badname");
        for bad in ["CamelCase", "with.dot", "with space", "-lead", ""] {
            let mut node = sample_node();
            node.name = bad.to_string();
            let err = write_node(&root, &node).unwrap_err().to_string();
            assert!(err.contains("allowlist"), "{bad}: {err}");
        }
        // An unknown layer is refused too.
        let mut node = sample_node();
        node.layer = "banana".to_string();
        let err = write_node(&root, &node).unwrap_err().to_string();
        assert!(err.contains("banana"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Metadata properties (short ids) are stored verbatim, never treated as
    /// identity — the file name stays layer.type.name even when an `id` is
    /// present.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_node_preserves_metadata_properties_verbatim() {
        let root = temp_root("metadata");
        let mut node = sample_node();
        node.properties = BTreeMap::from([("id".to_string(), "R1".to_string())]);
        let path = write_node(&root, &node).unwrap();
        // The identity is the path, never the short id.
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("place-order.json")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let back: NodeFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.properties.get("id").map(String::as_str), Some("R1"));
        assert_eq!(back, node);
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- In/out edge pairing (phase-3 task-9) ---

    // --- Code-endpoint validation (phase-3 task-10) ---

    // --- Atomic multi-file write-through (phase-3 task-11) ---

    /// A multi-file mutation writes every file at its derived path; all are
    /// present and deserialize back to their original [`NodeFile`] afterward
    /// (a non-git temp dir — the commit is skipped, the files still land).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_through_writes_all_files() {
        let root = temp_root("multiwrite");
        let mut a = node("requirements", "requirement", "place-order");
        a.body = "A customer can place an order.".to_string();
        a.out.push(out_edge("drives", "domain.service.checkout"));
        let mut b = node("domain", "service", "checkout");
        b.in_edges
            .push(in_edge("drives", "requirements.requirement.place-order"));

        write_through(&root, &[a.clone(), b.clone()]).unwrap();

        let a_path = root
            .join("layers")
            .join("requirements")
            .join("requirement")
            .join("place-order.json");
        let b_path = root
            .join("layers")
            .join("domain")
            .join("service")
            .join("checkout.json");
        assert!(a_path.exists(), "{} must exist", a_path.display());
        assert!(b_path.exists(), "{} must exist", b_path.display());
        let back_a: NodeFile =
            serde_json::from_str(&std::fs::read_to_string(&a_path).unwrap()).unwrap();
        let back_b: NodeFile =
            serde_json::from_str(&std::fs::read_to_string(&b_path).unwrap()).unwrap();
        assert_eq!(back_a, a);
        assert_eq!(back_b, b);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A pre-check failure — a plans-layer node, an allowlist-violating name,
    /// or a duplicate path — writes NOTHING (no `layers/` tree is created).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_through_precheck_failure_writes_nothing() {
        // Plans layer is transient — refused before anything is written.
        let root = temp_root("precheck-plans");
        let err = write_through(&root, &[node("plans", "task", "t1")]).unwrap_err();
        assert!(err.to_string().contains("plans"), "{err}");
        assert!(!root.join("layers").exists());

        // Allowlist-violating name — refused (never sanitized).
        let root = temp_root("precheck-name");
        let err =
            write_through(&root, &[node("requirements", "requirement", "Bad Name")]).unwrap_err();
        assert!(err.to_string().contains("allowlist"), "{err}");
        assert!(!root.join("layers").exists());

        // Two entries colliding on the same path — refused.
        let root = temp_root("precheck-dup");
        let err = write_through(
            &root,
            &[
                node("requirements", "requirement", "dup"),
                node("requirements", "requirement", "dup"),
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
        assert!(!root.join("layers").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A mid-write failure restores the previous state all-or-nothing: an
    /// existing file is restored byte-for-byte, a newly-created file is
    /// removed, and the failure is surfaced.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_through_restores_prior_state_on_failure() {
        let root = temp_root("rollback");
        let mut existing = node("requirements", "requirement", "existing");
        existing.body = "new".to_string();
        let mut fresh = node("requirements", "requirement", "fresh");
        fresh.body = "new".to_string();
        // The third write's path is pre-created as a DIRECTORY, so writing it
        // fails mid-set (after `existing` and `fresh` are already written).
        let blocked = node("requirements", "requirement", "blocked");

        let existing_path = root
            .join("layers")
            .join("requirements")
            .join("requirement")
            .join("existing.json");
        std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
        std::fs::write(&existing_path, "OLD CONTENT").unwrap();
        let fresh_path = root
            .join("layers")
            .join("requirements")
            .join("requirement")
            .join("fresh.json");
        assert!(!fresh_path.exists());
        let blocked_path = root
            .join("layers")
            .join("requirements")
            .join("requirement")
            .join("blocked.json");
        std::fs::create_dir_all(&blocked_path).unwrap();

        let err = write_through(&root, &[existing, fresh, blocked]).unwrap_err();
        assert!(err.to_string().contains("blocked.json"), "{err}");
        // The pre-existing file is restored byte-for-byte.
        assert_eq!(
            std::fs::read_to_string(&existing_path).unwrap(),
            "OLD CONTENT"
        );
        // The newly-created file is removed.
        assert!(
            !fresh_path.exists(),
            "a fresh file must be removed on rollback"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- Pairing/atomicity regression (phase-3 task-12) ---

    /// note-18 regression: a node rewrite (rename or delete) must leave every
    /// incident edge intact — the source's out-half AND the target's in-half
    /// are rewritten or removed together, never dropped on one side.
    /// [`write_through`] is the atomic rewrite (SPEC §4.1 "renames / deletions
    /// are atomic write-throughs"); [`check_edge_pairing`] on the resulting
    /// files then proves the rewrite left no dangling pairing and no
    /// silently-dropped edge.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn incident_edges_survive_node_rewrites_rename_and_delete() {
        // --- RENAME: author A --contains--> B, then rename B -> C. ---
        let root = temp_root("rename");
        let (a, b) = authored_pair("a", "b");
        write_through(&root, &[a.clone(), b.clone()]).unwrap();
        assert!(check_edge_pairing(&[a.clone(), b.clone()]).is_ok());

        // The rewrite: B's file is gone, C carries the FQN plus the incoming
        // edge, and A's out-edge target is re-pointed to C.
        let mut a_renamed = node("requirements", "requirement", "a");
        a_renamed
            .out
            .push(out_edge("contains", "requirements.requirement.c"));
        let mut c = node("requirements", "requirement", "c");
        c.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        write_through(&root, &[a_renamed.clone(), c.clone()]).unwrap();

        // The incident edge survived the rename intact: A's out -> C and C's
        // in <- A still pair — no dangling reference to the gone B.
        let a_read = read_node_file(&root, "requirements", "requirement", "a");
        let c_read = read_node_file(&root, "requirements", "requirement", "c");
        assert_eq!(a_read, a_renamed);
        assert_eq!(c_read, c);
        assert!(
            check_edge_pairing(&[a_read, c_read]).is_ok(),
            "a rename must leave no dangling pairing"
        );
        let _ = std::fs::remove_dir_all(&root);

        // --- DELETE: author A --contains--> B, then delete B. ---
        let root = temp_root("delete");
        let (a, b) = authored_pair("a", "b");
        write_through(&root, &[a.clone(), b.clone()]).unwrap();
        assert!(check_edge_pairing(&[a.clone(), b.clone()]).is_ok());

        // The rewrite: A's out-edge to B is removed and B's file is gone.
        let a_deleted = node("requirements", "requirement", "a");
        write_through(&root, std::slice::from_ref(&a_deleted)).unwrap();

        let a_read = read_node_file(&root, "requirements", "requirement", "a");
        assert_eq!(a_read, a_deleted);
        assert!(
            check_edge_pairing(&[a_read]).is_ok(),
            "a delete must leave no dangling pairing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The strict add/update surface through the PRODUCTION path: the
    /// `refuse_if_present` gate refuses a present FQN and allows an absent one,
    /// and `apg node update` (`node_cmd::build_change` + `layers::write_project`,
    /// the exact sequence production runs) is edge-preserving — it merges
    /// body/properties (set + explicit unset) while keeping the immutable
    /// identity and the exact count/content of the node's out/in edges — and
    /// refuses an absent node.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn update_node_file_preserves_edges_and_merges_body_and_properties() {
        let (wt_apg, repo, _wt) = mutation_fixture("node-update");
        let (mut a, b) = authored_pair("a", "b");
        a.properties.insert("a".to_string(), "0".to_string());
        a.properties.insert("b".to_string(), "2".to_string());
        write_project(&wt_apg, &[a.clone(), b.clone()], &[]).unwrap();

        // refuse_if_present: a present FQN is refused, naming update/rm.
        let a_path = node_file_path(&wt_apg, Layer::Requirements, "requirement", "a");
        let err = refuse_if_present(
            a_path.exists(),
            &fqn(Layer::Requirements, "requirement", "a"),
            "apg node update",
            "apg node rm",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert!(err.contains("apg node update"), "{err}");
        assert!(err.contains("apg node rm"), "{err}");
        // An absent FQN passes the same gate.
        assert!(
            refuse_if_present(
                node_file_path(&wt_apg, Layer::Requirements, "requirement", "ghost").exists(),
                "requirements.requirement.ghost",
                "apg node update",
                "apg node rm",
            )
            .is_ok()
        );

        // `apg node update` (the real builder + write): body set, properties
        // MERGE ({a:0,b:2} -> --property a=1 -> {a:1,b:2}), the immutable
        // identity kept, and the exact out/in edge content untouched.
        let change = apg::node_cmd::build_change(
            &wt_apg,
            "node",
            &[
                "update".to_string(),
                "requirements".to_string(),
                "requirement".to_string(),
                "a".to_string(),
                "--body".to_string(),
                "new body".to_string(),
                "--property".to_string(),
                "a=1".to_string(),
            ],
        )
        .unwrap();
        let updated = change.writes[0].clone();
        write_project(&wt_apg, &change.writes, &change.deletes).unwrap();
        assert_eq!(updated.body, "new body");
        assert_eq!(
            updated.properties,
            BTreeMap::from([
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
            ]),
            "omitting --unset-property must never drop a key"
        );
        assert_eq!(updated.layer, "requirements");
        assert_eq!(updated.node_type, "requirement");
        assert_eq!(updated.name, "a");
        assert_eq!(updated.out, a.out, "out-edges must be preserved");
        assert_eq!(updated.in_edges, a.in_edges, "in-edges must be preserved");

        // A second `apg node update` reads the just-written state back through
        // the production path: `--unset-property b` removes exactly the named
        // key and an omitted `--body` is preserved.
        let change = apg::node_cmd::build_change(
            &wt_apg,
            "node",
            &[
                "update".to_string(),
                "requirements".to_string(),
                "requirement".to_string(),
                "a".to_string(),
                "--unset-property".to_string(),
                "b".to_string(),
            ],
        )
        .unwrap();
        let updated = change.writes[0].clone();
        write_project(&wt_apg, &change.writes, &change.deletes).unwrap();
        assert_eq!(
            updated.properties,
            BTreeMap::from([("a".to_string(), "1".to_string())]),
            "an explicit unset removes exactly the named key"
        );
        assert_eq!(updated.body, "new body", "omitted --body is preserved");

        // An absent node is refused before any update is computed.
        let err = apg::node_cmd::build_change(
            &wt_apg,
            "node",
            &[
                "update".to_string(),
                "requirements".to_string(),
                "requirement".to_string(),
                "ghost".to_string(),
            ],
        )
        .err()
        .expect("an update of an absent node must be refused")
        .to_string();
        assert!(err.contains("does not exist"), "{err}");
        testutil::remove(&repo);
    }

    // --- Tree ingestion (phase-3 task-15) ---

    /// `ingest_tree` on a small tree produces the right records: the three
    /// node records at their derived `<layer>.<type>.<name>` FQNs, the three
    /// edge records (drives / realised-by / implemented-by), and nothing from
    /// the in-edge halves (out is canonical).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn ingest_tree_produces_node_and_edge_records() {
        let root = temp_root("ingest-tree");
        write_tree(&root, &sample_tree());
        let scanned = code_universe(&["apg.main"]);
        let planned: BTreeSet<String> = BTreeSet::new();

        let records = ingest_tree(&root, &scanned, &planned).unwrap();

        let has_node = |fqn: &str| {
            records.iter().any(|r| match r {
                Record::Requirement { fqn: f, .. }
                | Record::Group { fqn: f, .. }
                | Record::System { fqn: f, .. } => f == fqn,
                _ => false,
            })
        };
        assert!(has_node("requirements.requirement.place-order"));
        assert!(has_node("domain.group.sales"));
        assert!(has_node("solution.system.payments"));
        // Exactly three node records (the group carries no attribute/root).
        assert_eq!(
            records
                .iter()
                .filter(|r| matches!(
                    r,
                    Record::Requirement { .. } | Record::Group { .. } | Record::System { .. }
                ))
                .count(),
            3
        );
        // The spine edges, out-side only.
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Drives { from, to }
                if from == "requirements.requirement.place-order" && to == "domain.group.sales"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::RealisedBy { from, to }
                if from == "domain.group.sales" && to == "solution.system.payments"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::SpecImplementedBy { from, to }
                if from == "solution.system.payments" && to == "apg.main"
        )));
        // The in-edge halves are never emitted (out is canonical).
        assert!(!records.iter().any(|r| matches!(
            r,
            Record::RealisedBy { from, .. } if from == "solution.system.payments"
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A pairing mismatch — an out-edge in one file without the matching
    /// in-edge in the target's file — is an ERROR at ingestion.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn ingest_tree_pairing_mismatch_errors() {
        let root = temp_root("ingest-mismatch");
        let mut nodes = sample_tree();
        // Drop the group's in-edge: the requirement's out-edge now dangles.
        nodes[1].in_edges.clear();
        write_tree(&root, &nodes);
        let scanned = code_universe(&["apg.main"]);
        let planned: BTreeSet<String> = BTreeSet::new();
        let err = ingest_tree(&root, &scanned, &planned).unwrap_err();
        assert!(err.to_string().contains("BOTH endpoint files"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `implemented-by` target gone from the scanned graph (and not a
    /// planned node) is a spec-drift ERROR; a planned-only FQN ingests fine
    /// (pending, not an error).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn ingest_tree_code_ref_gone_errors_planned_ingests() {
        let root = temp_root("ingest-drift");
        let nodes = sample_tree();
        write_tree(&root, &nodes);
        // `apg.main` is neither scanned nor planned → drift.
        let scanned = code_universe(&["apg.other"]);
        let planned: BTreeSet<String> = BTreeSet::new();
        let err = ingest_tree(&root, &scanned, &planned).unwrap_err();
        assert!(err.to_string().contains("spec drift"), "{err}");
        assert!(err.to_string().contains("apg.main"), "{err}");
        // `apg.main` planned (not scanned) → pending, ingests Ok.
        let planned = code_universe(&["apg.main"]);
        let records = ingest_tree(&root, &scanned, &planned).unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::SpecImplementedBy { from, to }
                if from == "solution.system.payments" && to == "apg.main"
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SPEC §3.3 "contains/depends-on trees acyclic": the scan leg refuses a
    /// cycle in either edge kind at ingestion. Pairing alone would pass (both
    /// halves of every edge are present) — the acyclicity rule is what stops
    /// it.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn ingest_tree_refuses_contains_and_depends_on_cycles() {
        for kind in ["contains", "depends-on"] {
            let root = temp_root(&format!("ingest-cycle-{kind}"));
            let a_fqn = "requirements.requirement.a";
            let b_fqn = "requirements.requirement.b";
            let mut a = node("requirements", "requirement", "a");
            a.out.push(out_edge(kind, b_fqn));
            a.in_edges.push(in_edge(kind, b_fqn));
            let mut b = node("requirements", "requirement", "b");
            b.out.push(out_edge(kind, a_fqn));
            b.in_edges.push(in_edge(kind, a_fqn));
            write_tree(&root, &[a, b]);

            let empty: BTreeSet<String> = BTreeSet::new();
            let err = ingest_tree(&root, &empty, &empty).unwrap_err().to_string();
            assert!(err.contains("cycle"), "{kind}: {err}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// SPEC §3.3: a `publishes`/`subscribes` target must be an `Entity` with
    /// `kind: event` — the scan leg refuses a plain entity (or a target whose
    /// file lacks the property) and ingests the event-targeted edge.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn ingest_tree_requires_publishes_subscribes_targets_to_be_events() {
        let empty: BTreeSet<String> = BTreeSet::new();
        for kind in ["publishes", "subscribes"] {
            // Entity (kind: event) → ingests.
            let root = temp_root(&format!("ingest-event-ok-{kind}"));
            let mut svc = node("domain", "service", "checkout");
            svc.out.push(out_edge(kind, "domain.entity.order-placed"));
            let mut event = node("domain", "entity", "order-placed");
            event
                .properties
                .insert(PROP_KIND.to_string(), "event".to_string());
            event
                .in_edges
                .push(in_edge(kind, "domain.service.checkout"));
            write_tree(&root, &[svc, event]);
            assert!(
                ingest_tree(&root, &empty, &empty).is_ok(),
                "{kind} -> Entity(kind:event) must ingest"
            );
            let _ = std::fs::remove_dir_all(&root);

            // A plain Entity (kind: entity) → refused.
            let root = temp_root(&format!("ingest-event-plain-{kind}"));
            let mut svc = node("domain", "service", "checkout");
            svc.out.push(out_edge(kind, "domain.entity.orders"));
            let mut plain = node("domain", "entity", "orders");
            plain
                .properties
                .insert(PROP_KIND.to_string(), "entity".to_string());
            plain
                .in_edges
                .push(in_edge(kind, "domain.service.checkout"));
            write_tree(&root, &[svc, plain]);
            let err = ingest_tree(&root, &empty, &empty).unwrap_err().to_string();
            assert!(err.contains(kind), "{kind}: {err}");
            assert!(err.contains("event"), "{kind}: {err}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    // --- write_project orchestration (phase-3 task-16) ---

    /// `validate_change` accepts a valid add and refuses an invalid one (bad
    /// name) without writing anything — the complete change is validated first.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn validate_change_rejects_invalid_node_without_writing() {
        let root = temp_root("validate-change");
        let a = node("requirements", "requirement", "a");
        write_node(&root, &a).unwrap();

        let b = node("requirements", "requirement", "b");
        assert!(validate_change(&root, std::slice::from_ref(&b), &[]).is_ok());
        let bad = node("requirements", "requirement", "Bad Name");
        assert!(validate_change(&root, &[bad], &[]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SPEC §3.3 "contains/depends-on trees acyclic" on the write path: a
    /// cycle closed by the proposed change is refused by `validate_change`
    /// over the assembled post-mutation edge set (pairing alone passes — both
    /// halves of every edge are present), and nothing is written.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn validate_change_refuses_contains_and_depends_on_cycles() {
        for kind in ["contains", "depends-on"] {
            let root = temp_root(&format!("write-cycle-{kind}"));
            // A --kind--> B already on disk, both halves.
            let mut a = node("requirements", "requirement", "a");
            a.out.push(out_edge(kind, "requirements.requirement.b"));
            let mut b = node("requirements", "requirement", "b");
            b.in_edges.push(in_edge(kind, "requirements.requirement.a"));
            write_node(&root, &a).unwrap();
            write_node(&root, &b).unwrap();

            // The proposed change adds B --kind--> A (both halves): the
            // assembled set now cycles A → B → A.
            let mut a2 = a.clone();
            a2.in_edges
                .push(in_edge(kind, "requirements.requirement.b"));
            let mut b2 = b.clone();
            b2.out.push(out_edge(kind, "requirements.requirement.a"));
            let err = validate_change(&root, &[a2, b2], &[])
                .unwrap_err()
                .to_string();
            assert!(err.contains("cycle"), "{kind}: {err}");
            // Nothing was written: the files on disk still carry only the
            // one-directional edge.
            let a_read = read_node_file(&root, "requirements", "requirement", "a");
            assert!(a_read.in_edges.is_empty(), "{kind}: file must be unchanged");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// SPEC §3.3: `publishes`/`subscribes` targets must be `Entity` with
    /// `kind: event` — `validate_change` refuses a plain entity (the edge
    /// matrix is type-only) and accepts the event-targeted edge.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn validate_change_requires_publishes_subscribes_targets_to_be_events() {
        let root = temp_root("write-event-targets");
        for kind in ["publishes", "subscribes"] {
            // Entity (kind: event) → accepted.
            let mut svc = node("domain", "service", "checkout");
            svc.out.push(out_edge(kind, "domain.entity.order-placed"));
            let mut event = node("domain", "entity", "order-placed");
            event
                .properties
                .insert(PROP_KIND.to_string(), "event".to_string());
            event
                .in_edges
                .push(in_edge(kind, "domain.service.checkout"));
            assert!(
                validate_change(&root, &[svc, event], &[]).is_ok(),
                "{kind} -> Entity(kind:event) must be accepted"
            );

            // A plain Entity (kind: entity) → refused.
            let mut svc = node("domain", "service", "checkout");
            svc.out.push(out_edge(kind, "domain.entity.orders"));
            let mut plain = node("domain", "entity", "orders");
            plain
                .properties
                .insert(PROP_KIND.to_string(), "entity".to_string());
            plain
                .in_edges
                .push(in_edge(kind, "domain.service.checkout"));
            let err = validate_change(&root, &[svc, plain], &[])
                .unwrap_err()
                .to_string();
            assert!(err.contains(kind), "{kind}: {err}");
            assert!(err.contains("event"), "{kind}: {err}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SPEC §4.1: the code-ref drift check runs BEFORE anything is written —
    /// `write_project` with `implemented-by` targeting a FQN gone from the
    /// scanned graph is refused with no file and no commit (the class note-24
    /// fixed for constraints); the same edge to a `.trans` planned FQN is
    /// pending, not an error, and is accepted at write time.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_project_checks_code_ref_drift_before_writing_or_committing() {
        let (wt_apg, repo, wt) = mutation_fixture("write-drift");

        // (a) implemented-by -> a FQN neither scanned nor planned: drift.
        let mut ghost = node("solution", "system", "ghost-sys");
        ghost
            .out
            .push(out_edge("implemented-by", "fixture.mod.Gone"));
        let head_before = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();
        let err = write_project(&wt_apg, &[ghost], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("spec drift"), "{msg}");
        assert!(msg.contains("fixture.mod.Gone"), "{msg}");
        let ghost_path = node_file_path(&wt_apg, Layer::Solution, "system", "ghost-sys");
        assert!(
            !ghost_path.exists(),
            "a refused drift write must not land a file"
        );
        let head_after = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();
        assert_eq!(
            head_after, head_before,
            "a refused drift write must not commit"
        );

        // (b) The same edge to a `.trans` planned FQN is pending, not an
        // error: declare the planned node, re-scan (the DB carries it), and
        // the write is accepted and re-merged.
        let plan_path = wt_apg
            .join(apg::specs::TRANS)
            .join("plans")
            .join("foo.jsonl");
        apg::specs::write_jsonl(
            &plan_path,
            &[Record::PlannedNode {
                fqn: "fixture.mod.Widget".to_string(),
                kind: "struct".to_string(),
                name: "Widget".to_string(),
                parent: SCAN_MOD.to_string(),
            }],
        )
        .unwrap();
        testutil::scan_checkout(&wt).unwrap();

        let mut pending = node("solution", "system", "pending-sys");
        pending
            .out
            .push(out_edge("implemented-by", "fixture.mod.Widget"));
        write_project(&wt_apg, &[pending], &[]).unwrap();
        let back = read_node_file(&wt_apg, "solution", "system", "pending-sys");
        assert!(
            back.out
                .iter()
                .any(|oe| oe.kind == "implemented-by" && oe.target == "fixture.mod.Widget")
        );
        testutil::remove(&repo);
    }

    /// The delete write-through removes the node file AND rewrites the
    /// referencing file (incident edge dropped), leaving a pairing-consistent
    /// set — the §4.1 atomic delete. Runs in a REAL git repo, because the
    /// commit path is the whole point: a removed path must be staged as a
    /// deletion (`index.remove_path`), not `add_path`-ed, which stats the gone
    /// file and fails with a libgit2 NotFound (the latent bug the repo-less
    /// test could never see).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_through_with_deletes_removes_file_and_rewrites_referencing() {
        // A real project worktree: `write_through` commits on the branch.
        let repo = Repo::new("layers-delete-commit");
        let wt = repo.start_project("foo");
        let apg_root = wt.join(apg::specs::LAYOUT);
        let wt_sha = |wt: &Path| {
            git2::Repository::open(wt)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        };

        // Author A --contains--> B (both halves) and commit both files once.
        let (a, b) = authored_pair("a", "b");
        write_through(&apg_root, &[a.clone(), b.clone()]).unwrap();
        let first = wt_sha(&wt);

        // The delete: A's out-edge is stripped and B's file is removed — one
        // mutation, committed once.
        let mut a2 = a.clone();
        a2.out.clear();
        let b_path = node_file_path(&apg_root, Layer::Requirements, "requirement", "b");
        write_through_with_deletes(&apg_root, &[a2], std::slice::from_ref(&b_path)).unwrap();

        assert!(!b_path.exists(), "the deleted node file must be gone");
        let a_read = read_node_file(&apg_root, "requirements", "requirement", "a");
        assert!(
            a_read.out.is_empty(),
            "the referencing file must drop the incident edge"
        );
        assert!(check_edge_pairing(&[a_read]).is_ok());

        // Exactly one commit landed, and its tree delta stages the deletion
        // (Deleted) plus the rewrite (Modified) — no NotFound rollback.
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        let second = head.id().to_string();
        assert_ne!(second, first, "the delete must commit");
        assert_eq!(
            head.parent(0).unwrap().id().to_string(),
            first,
            "the delete must be exactly one commit ahead"
        );
        let parent_tree = head.parent(0).unwrap().tree().unwrap();
        let diff = wt_repo
            .diff_tree_to_tree(Some(&parent_tree), Some(&head.tree().unwrap()), None)
            .unwrap();
        let deleted: Vec<&str> = diff
            .deltas()
            .filter(|d| d.status() == git2::Delta::Deleted)
            .map(|d| d.old_file().path().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            deleted,
            vec!["apg/layers/requirements/requirement/b.json"],
            "the removed node file must be staged as a deletion"
        );
        testutil::remove(&repo);
    }

    // --- Scan wiring through the hermetic fixture (phase-3 task-14/17) ---
    // The full post-code scan leg — durable `apg/layers` tree + transient
    // `.trans/plans` mirror — exercised through `testutil::scan_checkout`
    // (a real git repo, a real payload scan, a real db.lbug), mirroring
    // `cmd_scan`'s assembly.

    /// R17 VI: the scan never opens `apg/specs/*.jsonl` or `apg/notes/` — a
    /// fixture whose committed legacy durable files are poisoned (malformed
    /// JSONL AND old-model sentinel records whose types the post-removal
    /// `Record` enum no longer knows) still scans green, ingests nothing from
    /// them, and leaves them byte-identical. Spec data comes from the
    /// `apg/layers` tree + the `.trans/plans` mirror only.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_ignores_poisoned_legacy_spec_and_note_files() {
        let repo = scan_repo("vi-unread");
        repo.write("apg/specs/foo.jsonl", "this is not json\n");
        repo.write(
            "apg/specs/_invariants.jsonl",
            "{\"type\":\"invariant\",\"fqn\":\"ghost/invariant\",\"title\":\"x\",\"body\":\"x\",\"category\":\"product\",\"scope\":\"global\",\"status\":\"active\"}\n",
        );
        repo.write(
            "apg/notes/fixture.mod.jsonl",
            "{\"type\":\"note\",\"fqn\":\"ghost/note-1\",\"body\":\"legacy\",\"kind\":\"background\"}\n",
        );
        repo.write("apg/notes/_root.jsonl", "broken {\n");
        repo.commit_all("poison the legacy durable files");
        // The positive control: a durable layers node the scan DOES ingest.
        write_tree(
            &repo.apg_root(),
            &[node("requirements", "requirement", "timer")],
        );

        testutil::scan_checkout(&repo.root).unwrap();

        // Scanned code + the layers tree landed...
        let db = artifacts::ArtifactDb::open(&repo.apg_root()).unwrap();
        assert!(
            // PHASE_09: the scan roots the module identity under its
            // lang_switch id (`go` — testutil::scan_checkout).
            db.has_node("go.fixture.mod.Store"),
            "scanned code must be in the DB"
        );
        assert!(
            db.has_node("requirements.requirement.timer"),
            "spec data must come from the apg/layers tree"
        );
        // ...and nothing from the poisoned legacy files (never read, never
        // ingested — any read would have failed on the malformed lines or the
        // unknown old-model types).
        assert!(!db.has_node("ghost/invariant"));
        assert!(!db.has_node("ghost/note-1"));
        drop(db);
        // Unwritten too: the poisoned files are byte-identical after the scan.
        assert_eq!(
            std::fs::read_to_string(repo.root.join("apg/specs/foo.jsonl")).unwrap(),
            "this is not json\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.root.join("apg/notes/_root.jsonl")).unwrap(),
            "broken {\n"
        );
        testutil::remove(&repo);
    }

    /// R16 AC: an in/out edge in one node file without the matching out/in
    /// edge in the other endpoint's file FAILS the scan — the pairing
    /// mismatch is caught at ingestion, not silently tolerated.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_fails_on_pairing_mismatch_between_node_files() {
        let repo = scan_repo("scan-mismatch");
        // A's out `contains -> B` with no matching in-edge on B's file.
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.b"));
        let b = node("requirements", "requirement", "b");
        write_tree(&repo.apg_root(), &[a, b]);

        let err = testutil::scan_checkout(&repo.root).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("BOTH endpoint files"), "{msg}");
        assert!(msg.contains("requirements.requirement.a"), "{msg}");
        testutil::remove(&repo);
    }

    /// SPEC §3.3 "contains/depends-on trees acyclic": a cycle between node
    /// files FAILS the scan at ingestion. Pairing alone would pass (all four
    /// halves are present) — the acyclicity rule is what stops the scan,
    /// proving it is wired into the scan leg.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_fails_on_contains_cycle_between_node_files() {
        let repo = scan_repo("scan-cycle");
        let a_fqn = "requirements.requirement.a";
        let b_fqn = "requirements.requirement.b";
        let mut a = node("requirements", "requirement", "a");
        a.out.push(out_edge("contains", b_fqn));
        a.in_edges.push(in_edge("contains", b_fqn));
        let mut b = node("requirements", "requirement", "b");
        b.out.push(out_edge("contains", a_fqn));
        b.in_edges.push(in_edge("contains", a_fqn));
        write_tree(&repo.apg_root(), &[a, b]);

        let err = testutil::scan_checkout(&repo.root).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        testutil::remove(&repo);
    }

    /// R16 AC (edge properties): the SAME endpoints with DIFFERENT edge
    /// properties is a mismatch — a match requires identical properties —
    /// and fails the scan.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_fails_on_same_endpoints_with_different_edge_properties() {
        let repo = scan_repo("scan-prop-mismatch");
        // The out half carries a flavor the in half does not.
        let mut a = node("requirements", "requirement", "a");
        let mut oe = out_edge("contains", "requirements.requirement.b");
        oe.properties
            .insert("flavor".to_string(), "direct".to_string());
        a.out.push(oe);
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        write_tree(&repo.apg_root(), &[a, b]);

        let err = testutil::scan_checkout(&repo.root).unwrap_err();
        assert!(err.to_string().contains("properties"), "{err}");
        testutil::remove(&repo);
    }

    /// `implemented-by` code refs vs the scanned graph (R16 code exemption):
    /// a FQN gone from the scanned graph (and not a `.trans` planned node) is
    /// spec drift and FAILS the scan; a `.trans` planned FQN ingests as
    /// pending, not an error.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_fails_on_implemented_by_drift_and_ingests_transient_planned_pending() {
        let repo = scan_repo("scan-drift");
        let mut sys = node("solution", "system", "payments");
        sys.out.push(out_edge("implemented-by", "fixture.mod.Gone"));
        write_tree(&repo.apg_root(), &[sys]);

        // Gone from both the scanned graph and .trans -> spec drift; the scan
        // fails naming the FQN.
        let err = testutil::scan_checkout(&repo.root).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("spec drift"), "{msg}");
        assert!(msg.contains("fixture.mod.Gone"), "{msg}");

        // Declared as a planned node in .trans -> pending, not an error: the
        // scan succeeds and the planned node ingests with status planned.
        let plan_path = repo
            .apg_root()
            .join(apg::specs::TRANS)
            .join("plans")
            .join("foo.jsonl");
        apg::specs::write_jsonl(
            &plan_path,
            &[Record::PlannedNode {
                fqn: "fixture.mod.Gone".to_string(),
                kind: "struct".to_string(),
                name: "Gone".to_string(),
                parent: SCAN_MOD.to_string(),
            }],
        )
        .unwrap();
        testutil::scan_checkout(&repo.root).unwrap();
        let db = artifacts::ArtifactDb::open(&repo.apg_root()).unwrap();
        assert!(
            db.is_planned("fixture.mod.Gone"),
            "a .trans planned FQN must ingest as pending"
        );
        assert!(db.has_node("solution.system.payments"));
        testutil::remove(&repo);
    }

    /// Prose satisfaction is never executed, and the scan no longer validates a
    /// constraint's `attaches-to` reference: `eval_constraint` is structure-only
    /// (name allowlist, type-in-layer, uniqueness), so a constraint whose body
    /// LOOKS like an expression (and is contradictory), and whose `attaches-to`
    /// is even UNRESOLVABLE, still ingests. Satisfaction is assessed by review
    /// only (R14).
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_never_evaluates_constraint_prose() {
        let repo = scan_repo("scan-constraint-prose");
        let mut ent = node("domain", "entity", "customer");
        ent.properties
            .insert("kind".to_string(), "entity".to_string());
        let mut c = node("requirements", "constraint", "law");
        c.properties.insert(
            PROP_ATTACHES_TO.to_string(),
            "domain.entity.ghost".to_string(),
        );
        c.body = "count(entities) == 0 AND count(entities) > 0".to_string();
        write_tree(&repo.apg_root(), &[ent, c]);

        testutil::scan_checkout(&repo.root).unwrap();
        let db = artifacts::ArtifactDb::open(&repo.apg_root()).unwrap();
        assert!(db.has_node("requirements.constraint.law"));
        assert!(db.has_node("domain.entity.customer"));
        testutil::remove(&repo);
    }

    /// R18/§5: `.trans` plans (Plan/PlanPhase/Task/PlannedNode) and feedback
    /// still ingest through the scan and pair against durable node-file nodes.
    /// The feedback sits in the tier dir of its attached node (SPEC §5) — the
    /// requirements tier mirror, `.trans/requirements/foo.jsonl` — and its
    /// Reviews edge points at the durable requirement and lands in the DB.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn scan_ingests_transient_plans_and_feedback_paired_to_durable_nodes() {
        let repo = scan_repo("scan-plans-feedback");
        // The durable side: one requirement node file.
        write_tree(
            &repo.apg_root(),
            &[node("requirements", "requirement", "timer")],
        );
        // The transient side: the plan store (plan/phase/task/planned-node)
        // plus the feedback mirror — a review of the durable requirement
        // lives in `.trans/requirements/foo.jsonl` with both halves (the
        // Feedback record AND its Reviews edge) in `.trans`.
        let plan_path = repo
            .apg_root()
            .join(apg::specs::TRANS)
            .join("plans")
            .join("foo.jsonl");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
                strategy: "G".to_string(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "fixture.mod.Widget".to_string(),
                kind: "struct".to_string(),
                name: "Widget".to_string(),
                parent: SCAN_MOD.to_string(),
            },
        ];
        apg::specs::write_jsonl(&plan_path, &records).unwrap();
        let req_mirror = repo
            .apg_root()
            .join(apg::specs::TRANS)
            .join("requirements")
            .join("foo.jsonl");
        apg::specs::write_jsonl(
            &req_mirror,
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "review".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: "requirements.requirement.timer".to_string(),
                },
            ],
        )
        .unwrap();

        testutil::scan_checkout(&repo.root).unwrap();

        let db = artifacts::ArtifactDb::open(&repo.apg_root()).unwrap();
        for f in [
            "foo/plan",
            "foo/plan.phase-01",
            "foo/plan.phase-01.task-1",
            "foo/feedback-1",
            "requirements.requirement.timer",
        ] {
            assert!(db.has_node(f), "{f} must be in the DB");
        }
        assert!(db.is_planned("fixture.mod.Widget"));
        let out = db
            .q("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(r:Requirement {fqn: 'requirements.requirement.timer'}) RETURN count(*)")
            .unwrap();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the Reviews edge must pair feedback to the durable node: {out}"
        );
        testutil::remove(&repo);
    }

    /// Phase-02 task-7: decoupled validation at the MUTATION level.
    ///
    /// - With BOTH `db.lbug` and `graph.jsonl` absent, a mutation succeeds:
    ///   structural references are still validated (a dangling authored edge is
    ///   refused), while an `implemented-by` code FQN is recorded UNVALIDATED.
    /// - With `graph.jsonl` present, a drift/invalid code FQN is rejected
    ///   WITHOUT opening `db.lbug` — proven by holding a read-write
    ///   `ArtifactDb` and asserting the reported error is the drift, not the
    ///   lbug write-lock collision.
    /// - The projection delta is applied write-through when a DB exists.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn write_project_decoupled_from_db_validates_export_and_records_unvalidated() {
        // (a) Both artifacts gone: structural refs still validated, code refs
        // recorded unvalidated. A fresh fixture's db.lbug + graph.jsonl are
        // removed before the mutations.
        let (wt_apg, repo, _wt) = mutation_fixture("decoupled-gone");
        let db_path = wt_apg.join(apg::specs::TRANS).join("db.lbug");
        let export_path = wt_apg.join(apg::specs::TRANS).join("graph.jsonl");
        std::fs::remove_file(&db_path).unwrap();
        std::fs::remove_file(&export_path).unwrap();

        // Structural reference validation still runs (no artifacts needed): a
        // dangling authored `depends-on` target is refused (no file, no commit).
        let mut dangling = node("requirements", "requirement", "dangling");
        dangling
            .out
            .push(out_edge("depends-on", "requirements.requirement.ghost"));
        let err = write_project(&wt_apg, &[dangling], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ghost"),
            "structural refs must validate from the node files alone: {msg}"
        );
        let dangling_path = node_file_path(&wt_apg, Layer::Requirements, "requirement", "dangling");
        assert!(
            !dangling_path.exists(),
            "a refused structural mutation must not land a file"
        );

        // A code-FQN implemented-by to an unscanned FQN succeeds with both
        // artifacts gone: recorded unvalidated (no export ⇒ no code identity).
        let mut unvalidated = node("solution", "system", "unknown-sys");
        unvalidated
            .out
            .push(out_edge("implemented-by", "fixture.mod.NotScanned"));
        write_project(&wt_apg, &[unvalidated], &[]).unwrap();
        let back = read_node_file(&wt_apg, "solution", "system", "unknown-sys");
        assert!(
            back.out
                .iter()
                .any(|oe| oe.kind == "implemented-by" && oe.target == "fixture.mod.NotScanned"),
            "the unvalidated code ref must still be recorded durably"
        );
        testutil::remove(&repo);

        // (b)/(c) graph.jsonl present: the drift check runs until it rejects,
        // BEFORE any db.lbug open; a valid code ref is accepted and projected.
        let (wt_apg, repo, _wt) = mutation_fixture("decoupled-export");

        // Hold the DB read-write, so any DB open on the validation path would
        // fail with an lbug lock error instead of the drift refusal.
        let held = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        let mut drift = node("solution", "system", "drift-sys");
        drift
            .out
            .push(out_edge("implemented-by", "fixture.mod.Gone"));
        let err = write_project(&wt_apg, &[drift], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("spec drift"), "expected drift refusal: {msg}");
        assert!(msg.contains("fixture.mod.Gone"), "{msg}");
        assert!(
            !msg.contains("Could not set lock"),
            "validation must never open db.lbug: {msg}"
        );
        let drift_path = node_file_path(&wt_apg, Layer::Solution, "system", "drift-sys");
        assert!(!drift_path.exists(), "a refused drift write lands nothing");
        drop(held);

        // A valid implemented-by to scanned code is accepted, and the
        // projection delta is applied write-through to the live DB.
        let mut known = node("solution", "system", "known-sys");
        known
            .out
            .push(out_edge("implemented-by", "go.fixture.mod.Store"));
        write_project(&wt_apg, &[known], &[]).unwrap();
        let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        assert!(
            db.has_node("solution.system.known-sys"),
            "the projection delta must be applied write-through"
        );
        drop(db);

        testutil::remove(&repo);
    }

    /// Phase-05 task-14 (int): commit-then-project ordering — the SINGLE
    /// ordering-test home (phase-02 task-5 points here). An injectable one-shot
    /// hook fires at the commit→project boundary; a returned `Err` stands in
    /// for a crash at that boundary.
    ///
    /// **Window 2 — die BEFORE the commit**: the mutation appears in NEITHER
    /// the durable store NOR the projection, and no commit lands.
    ///
    /// **Window 1 — die AFTER the durable commit but BEFORE the projection
    /// apply**: the durable store holds the committed mutation while the
    /// projection stays prior; the next rebuild reproduces the committed state.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn commit_then_project_orders_durable_write_before_projection() {
        let (wt_apg, repo, wt) = mutation_fixture("commit-project");
        let r2_path = node_file_path(&wt_apg, Layer::Requirements, "requirement", "r2");
        let r3_path = node_file_path(&wt_apg, Layer::Requirements, "requirement", "r3");

        // Window 2: BEFORE the durable commit.
        install_mutation_hook(MutationBoundary::BeforeCommit, || {
            anyhow::bail!("forced failure before commit")
        });
        let head_before = testutil::commit_count(&wt);
        let err =
            write_project(&wt_apg, &[node("requirements", "requirement", "r2")], &[]).unwrap_err();
        assert!(format!("{err:#}").contains("before commit"), "{err:#}");
        assert!(
            !r2_path.exists(),
            "window 2: the mutation must not be durable"
        );
        {
            let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(
                !db.has_node("requirements.requirement.r2"),
                "window 2: the mutation must not be projected"
            );
        }
        assert_eq!(
            testutil::commit_count(&wt),
            head_before,
            "window 2: a failure before the commit must not commit"
        );

        // Window 1: AFTER the durable commit, BEFORE the projection apply.
        install_mutation_hook(MutationBoundary::BeforeProject, || {
            anyhow::bail!("forced failure before projection")
        });
        let err =
            write_project(&wt_apg, &[node("requirements", "requirement", "r3")], &[]).unwrap_err();
        assert!(format!("{err:#}").contains("before projection"), "{err:#}");
        // The durable write is committed...
        assert!(
            r3_path.exists(),
            "window 1: the durable write must be committed before the projection"
        );
        // ...while the projection stays prior.
        {
            let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(
                !db.has_node("requirements.requirement.r3"),
                "window 1: the projection must NOT be applied before the hook"
            );
        }

        // The next rebuild reproduces the committed state.
        testutil::scan_checkout(&wt).unwrap();
        {
            let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(
                db.has_node("requirements.requirement.r3"),
                "the next rebuild must reproduce the committed state"
            );
            assert!(
                !db.has_node("requirements.requirement.r2"),
                "window 2 left nothing for a rebuild to reproduce"
            );
        }

        testutil::remove(&repo);
    }

    /// Phase-05 task-9: projection-equals-sources as a TWO-WAY equality over
    /// NODES AND EDGES — `db − sources = ∅` AND `sources − db = ∅` — covering
    /// every authored edge kind (Contains/Drives/RealisedBy/SpecImplementedBy/
    /// Uses/Represents/Details/DependsOn) plus the transient Satisfies/Gates/
    /// Reviews, with no duplicate rows per `(label, fqn)`. Scoped to BOTH the
    /// durable (`write_project`) and transient (`write_jsonl_and_reingest`)
    /// paths; **no scan runs** to reach this state.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn projection_equals_sources_two_way_nodes_and_edges() {
        let (wt_apg, repo, _wt) = mutation_fixture("projection-eq");

        // --- durable sources: one node file per kind, with paired edges ---
        let mut user = node("requirements", "user", "customer");
        let mut timer = node("requirements", "requirement", "timer");
        let mut clock = node("requirements", "requirement", "clock");
        let mut order = node("domain", "entity", "order");
        order
            .properties
            .insert(PROP_KIND.to_string(), "entity".to_string());
        let mut checkout = node("domain", "service", "checkout");
        let mut portal = node("solution", "system", "portal");
        let mut alice = node("solution", "person", "alice");
        let mut notes = node("global", "note", "notes");

        // contains: User ⊃ Requirement
        user.out
            .push(out_edge("contains", "requirements.requirement.timer"));
        timer
            .in_edges
            .push(in_edge("contains", "requirements.user.customer"));
        // depends-on: Requirement → Requirement
        timer
            .out
            .push(out_edge("depends-on", "requirements.requirement.clock"));
        clock
            .in_edges
            .push(in_edge("depends-on", "requirements.requirement.timer"));
        // drives: Requirement → Service
        timer
            .out
            .push(out_edge("drives", "domain.service.checkout"));
        checkout
            .in_edges
            .push(in_edge("drives", "requirements.requirement.timer"));
        // realised-by: Service → System
        checkout
            .out
            .push(out_edge("realised-by", "solution.system.portal"));
        portal
            .in_edges
            .push(in_edge("realised-by", "domain.service.checkout"));
        // implemented-by: System → code (no in-half — code has no node file)
        portal
            .out
            .push(out_edge("implemented-by", "go.fixture.mod.Store"));
        // represents: User → Entity
        user.out.push(out_edge("represents", "domain.entity.order"));
        order
            .in_edges
            .push(in_edge("represents", "requirements.user.customer"));
        // uses: Person → System
        alice.out.push(out_edge("uses", "solution.system.portal"));
        portal
            .in_edges
            .push(in_edge("uses", "solution.person.alice"));
        // details: Note → Requirement
        notes
            .out
            .push(out_edge("details", "requirements.requirement.timer"));
        timer.in_edges.push(in_edge("details", "global.note.notes"));

        write_project(
            &wt_apg,
            &[user, timer, clock, order, checkout, portal, alice, notes],
            &[],
        )
        .unwrap();

        // --- transient sources: plan store + feedback mirror ---
        let plan_path = apg::specs::plan_jsonl_path(&wt_apg, "foo");
        let plan_records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "Foo".into(),
                strategy: "S".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-02".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-02".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "requirements.requirement.timer".into(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&wt_apg, &plan_path, "foo", &plan_records).unwrap();

        let mirror = apg::specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        let feedback = vec![
            Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "review".into(),
                status: "open".into(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "requirements.requirement.timer".into(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&wt_apg, &mirror, "foo", &feedback).unwrap();

        // --- sources → expected (label, fqn) and (table, from, to) sets ---
        let (scanned, planned) = apg::artifacts::code_universes_from_export(&wt_apg).unwrap();
        let mut source_records = apg::layers::ingest_tree(&wt_apg, &scanned, &planned).unwrap();
        for f in apg::specs::plan_files(&wt_apg)
            .into_iter()
            .chain(apg::specs::trans_mirror_files(&wt_apg))
        {
            source_records.extend(apg::specs::read_jsonl(&f).unwrap());
        }
        let expected_nodes: BTreeSet<(String, String)> = source_records
            .iter()
            .filter_map(|r| {
                apg::artifacts::node_label_fqn(r).map(|(l, f)| (l.to_string(), f.to_string()))
            })
            .collect();
        let metadata_fqns: BTreeSet<&str> =
            expected_nodes.iter().map(|(_, f)| f.as_str()).collect();
        let expected_edges: BTreeSet<(String, String, String)> = source_records
            .iter()
            .filter_map(apg::artifacts::edge_merge)
            .filter(|(_, from, _)| metadata_fqns.contains(from))
            .map(|(t, f, to)| (t.to_string(), f.to_string(), to.to_string()))
            .collect();

        // --- db → observed sets, scoped to the metadata labels/tables ---
        const METADATA_LABELS: [&str; 17] = [
            "Requirement",
            "Note",
            "Feedback",
            "Plan",
            "PlanPhase",
            "Task",
            "Stakeholder",
            "Entity",
            "System",
            "Container",
            "Component",
            "User",
            "DomainGroup",
            "Value",
            "Service",
            "Person",
            "Constraint",
        ];
        let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        let mut db_rows: Vec<(String, String)> = Vec::new();
        for label in METADATA_LABELS {
            let conn = db.conn().unwrap();
            let result = conn
                .query(&format!("MATCH (n:{label}) RETURN n.fqn"))
                .unwrap();
            for row in result {
                db_rows.push((
                    label.to_string(),
                    row.first().map(|v| v.to_string()).unwrap_or_default(),
                ));
            }
        }
        let db_nodes: BTreeSet<(String, String)> = db_rows.iter().cloned().collect();
        assert_eq!(
            db_rows.len(),
            db_nodes.len(),
            "no duplicate node row per (label, fqn): {db_rows:?}"
        );

        const EDGE_TABLES: [&str; 14] = [
            "Contains",
            "Drives",
            "RealisedBy",
            "SpecImplementedBy",
            "Uses",
            "Represents",
            "Details",
            "DependsOn",
            "Gates",
            "Satisfies",
            "Reviews",
            "Publishes",
            "Subscribes",
            "Calls",
        ];
        let mut db_edges: BTreeSet<(String, String, String)> = BTreeSet::new();
        for table in EDGE_TABLES {
            let conn = db.conn().unwrap();
            let result = conn
                .query(&format!("MATCH (a)-[:{table}]->(b) RETURN a.fqn, b.fqn"))
                .unwrap();
            for row in result {
                let from = row.first().map(|v| v.to_string()).unwrap_or_default();
                let to = row.get(1).map(|v| v.to_string()).unwrap_or_default();
                if metadata_fqns.contains(from.as_str()) {
                    db_edges.insert((table.to_string(), from, to));
                }
            }
        }

        assert_eq!(
            db_nodes, expected_nodes,
            "nodes: db − sources and sources − db must both be empty"
        );
        assert_eq!(
            db_edges, expected_edges,
            "edges: db − sources and sources − db must both be empty"
        );
        drop(db);

        testutil::remove(&repo);
    }

    /// A durable node/edge mutation must NOT drop a pre-existing transient
    /// `Feedback -[:Reviews]-> <node>` pairing. The durable projection apply
    /// DETACH-deletes every changed FQN; without re-merging the transient
    /// records that DETACH takes the Reviews edge with it, and the pairing
    /// stays gone until some later `apg plan`/`apg review` write (the
    /// fix-reviews-edge bug). No `apg scan` and no transient write runs between
    /// the durable mutation and the fresh read here.
    #[test]
    #[ignore = "e2e tier: real I/O (temp dir/db.lbug/git); run via cargo test-e2e"]
    fn durable_mutation_preserves_transient_review_pairing() {
        let (wt_apg, repo, _wt) = mutation_fixture("review-pairing");
        let req_fqn = "requirements.requirement.timer";

        // The durable node, via the real `apg node add` command shape.
        let add = apg::node_cmd::build_change(
            &wt_apg,
            "node",
            &[
                "add".to_string(),
                "requirements".to_string(),
                "requirement".to_string(),
                "timer".to_string(),
            ],
        )
        .unwrap();
        write_project(&wt_apg, &add.writes, &add.deletes).unwrap();

        // A transient review of that durable node: both halves (the Feedback
        // record AND its Reviews edge) live in the node's tier mirror —
        // `.trans/requirements/foo.jsonl` (SPEC §5). The write-through merges
        // the whole transient set, so the pairing is visible before the
        // durable mutation under test.
        let mirror = apg::specs::transient_feedback_path(&wt_apg, "foo", Layer::Requirements);
        artifacts::write_jsonl_and_reingest(
            &wt_apg,
            &mirror,
            "foo",
            &[
                Record::Feedback {
                    fqn: "foo/feedback-1".to_string(),
                    body: "review".to_string(),
                    status: "open".to_string(),
                    disposition: String::new(),
                },
                Record::Reviews {
                    from: "foo/feedback-1".to_string(),
                    to: req_fqn.to_string(),
                },
            ],
        )
        .unwrap();
        {
            let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("foo/feedback-1"));
            let out = db
                .q(&format!(
                    "MATCH (:Feedback {{fqn: 'foo/feedback-1'}})-[:Reviews]->(:Requirement {{fqn: '{req_fqn}'}}) RETURN count(*)"
                ))
                .unwrap();
            assert_eq!(
                out.lines().last().map(str::trim),
                Some("1"),
                "the transient Reviews pairing must exist before the durable mutation: {out}"
            );
        }

        // A DURABLE mutation on the same node (`apg node update`, the real
        // command shape) — no plan/review write in between. Its changed-FQN
        // DETACH removes the node and the incident Reviews edge; the projection
        // apply must re-merge the transient records in the same transaction.
        let update = apg::node_cmd::build_change(
            &wt_apg,
            "node",
            &[
                "update".to_string(),
                "requirements".to_string(),
                "requirement".to_string(),
                "timer".to_string(),
                "--body".to_string(),
                "revised".to_string(),
            ],
        )
        .unwrap();
        write_project(&wt_apg, &update.writes, &update.deletes).unwrap();

        // A fresh `ArtifactDb::open` — equivalent to a new `apg_query` process —
        // still returns the Feedback node and its Reviews edge to the durable
        // node.
        let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        assert!(
            db.has_node("foo/feedback-1"),
            "the transient Feedback node must survive the durable mutation"
        );
        let out = db
            .q(&format!(
                "MATCH (:Feedback {{fqn: 'foo/feedback-1'}})-[:Reviews]->(:Requirement {{fqn: '{req_fqn}'}}) RETURN count(*)"
            ))
            .unwrap();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the transient Reviews pairing must survive the durable mutation: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }
}
