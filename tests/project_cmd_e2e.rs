//! Relocated e2e tests for `apg project` (from `src/project_cmd.rs`'s inline
//! `#[cfg(test)] mod tests`). The `project_cmd` module is e2e-only — every test
//! does real I/O — so there is no sibling `src/project_cmd/tests.rs` and this
//! crate holds both the tests and their `mod tests`-root support helpers.

mod common;

use apg::artifacts;
use apg::git;
use apg::layers::{self, InEdge, NodeFile, OutEdge};
use apg::plan_cmd;
use apg::project_cmd::*;
use apg::schema::Record;
use apg::specs;
use apg::testutil::{self, Repo};
use common::wt_commit;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The module/fqn namespace the payload fixtures use.
const MOD: &str = "fixture.mod";
const FILE: &str = "/abs/store.go";
/// PHASE_09: the hermetic scan (`testutil::scan_checkout`, lang_switch `go`)
/// roots the frontend-emitted module identity, so the canonical FQN of the
/// fixture's scanned code is `go.fixture.mod.*` — the form every scanned-code
/// expectation and every authored `implemented-by` target must use.
const MOD_FQN: &str = "go.fixture.mod";

fn start_scan(dir: &Path) -> anyhow::Result<()> {
    testutil::scan_checkout(dir)
}

/// Rewrites the fixture's committed `apg/config.json` (the whole file)
/// and commits it, so the layout declares `version` (`None` → the
/// unversioned, pre-versioning shape).
fn set_layout_version(repo: &Repo, version: Option<&str>) {
    let json = match version {
        Some(v) => {
            format!("{{\n  \"default\": \"src\",\n  \"types\": [],\n  \"version\": \"{v}\"\n}}\n")
        }
        None => "{ \"default\": \"src\", \"types\": [] }\n".to_string(),
    };
    repo.write("apg/config.json", &json);
    repo.commit_all("set layout version");
}

/// The binary's version with a patch bump — same major.minor, so the R10
/// gate must proceed (patch differences never block).
fn patch_shifted_version() -> String {
    let v: Vec<u64> = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|p| p.parse().unwrap())
        .collect();
    format!("{}.{}.{}", v[0], v[1], v[2] + 1)
}

/// A version whose major.minor is guaranteed older than the binary's
/// (e.g. 0.9.x for a 0.10.4 binary).
fn older_minor_version() -> String {
    let v: Vec<u64> = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|p| p.parse().unwrap())
        .collect();
    if v[1] > 0 {
        format!("{}.{}.0", v[0], v[1] - 1)
    } else {
        format!("{}.99.0", v[0].saturating_sub(1))
    }
}

/// A version whose major.minor is guaranteed newer than the binary's
/// (e.g. 0.11.x / 1.x for a 0.10.4 binary).
fn newer_minor_version() -> String {
    let v: Vec<u64> = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|p| p.parse().unwrap())
        .collect();
    format!("{}.{}.0", v[0], v[1] + 1)
}

// ------------------------------------------------------------------
// task-4 AC (int): one command yields worktree + branch + branch DB off
// the DEFAULT branch; in-context re-run no-ops and prints the path;
// identity correct in worktree vs main.
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// task-12 (e2e): start on main -> mutate -> verify -> merge -> main
// rebuild; verify rejects unrealized planned nodes, dangling targets,
// and unresolved feedback.
// ------------------------------------------------------------------

/// Authors the durable spec as a node file under `apg/layers/` (the
/// new-model store) rather than the retired `apg/specs/` JSONL: one
/// requirement node, written through the node-file mutation funnel (guard →
/// validate → atomic write → commit → re-merge).
fn write_spec_node(wt_apg: &Path) {
    let node = layers::NodeFile {
        layer: "requirements".to_string(),
        node_type: "requirement".to_string(),
        name: "timer".to_string(),
        body: "A workitem can be started".to_string(),
        properties: std::collections::BTreeMap::from([("id".to_string(), "R1".to_string())]),
        out: Vec::new(),
        in_edges: Vec::new(),
    };
    layers::write_project(wt_apg, &[node], &[]).unwrap();
}

/// A plan whose single task plans `fixture.mod.Widget` (not yet real
/// code) and carries one open feedback on the task.
fn plan_records(with_feedback: bool) -> Vec<Record> {
    let mut r: Vec<Record> = vec![
        Record::Plan {
            fqn: "foo/plan".into(),
            title: "Foo plan".into(),
            strategy: String::new(),
        },
        Record::PlanPhase {
            fqn: "foo/plan.phase-01".into(),
            number: 1,
            title: "P1".into(),
            deliverable: "D".into(),
            status: "pending".into(),
        },
        Record::Contains {
            from: "foo/plan".into(),
            to: "foo/plan.phase-01".into(),
            properties: layers::NodeProperties::default(),
        },
        Record::Satisfies {
            from: "foo/plan.phase-01".into(),
            to: "requirements.requirement.timer".into(),
        },
        Record::Task {
            fqn: "foo/plan.phase-01.task-1".into(),
            title: "T".into(),
            kind: "source".into(),
            tier: String::new(),
            status: "pending".into(),
            verb: "creates".into(),
            target: String::new(),
            new_fqn: String::new(),
        },
        Record::Contains {
            from: "foo/plan.phase-01".into(),
            to: "foo/plan.phase-01.task-1".into(),
            properties: layers::NodeProperties::default(),
        },
        Record::PlannedNode {
            fqn: format!("{MOD_FQN}.Widget"),
            kind: "struct".into(),
            name: "Widget".into(),
            parent: MOD_FQN.into(),
        },
    ];
    if with_feedback {
        r.push(Record::Feedback {
            fqn: "foo/feedback-1".into(),
            body: "unresolved".into(),
            status: "open".into(),
            disposition: String::new(),
        });
        r.push(Record::Reviews {
            from: "foo/feedback-1".into(),
            to: "foo/plan.phase-01.task-1".into(),
        });
    }
    r
}

// ------------------------------------------------------------------
// phase-5 task-1 (e2e): bootstrap dogfood — the apg-projects change-set
// re-materializes in the layers model through the REAL project flow
// (SPEC §6: "this spec ... will be re-materialized in the new model
// later" — this is that re-materialization): start from the main
// checkout (worktree + branch + copied scan) -> author the tiers as node
// files through the write_project funnel (the same funnel `apg node`/
// `apg edge` use) -> the transient plan ingests alongside -> verify ->
// merge -> main rebuild. Plus the SPEC §6 guard: the test worktree
// `apg/.worktrees/test` stays until before shipping and cleanup deletes
// no branch.
// ------------------------------------------------------------------

/// The code FQNs the solution tier's `implemented-by` edges claim — all
/// resolve in the fixture's scanned graph, and the plan's tasks must touch
/// every one of them for derived solution coverage (SPEC §5).
const IMPL_FQNS: [&str; 5] = [
    "go.fixture.mod.ProjectStart",
    "go.fixture.mod.MutationGuard",
    "go.fixture.mod.LayersSerializer",
    "go.fixture.mod.PlanBridge",
    "go.fixture.mod.InitVersionGate",
];

/// A bare node file (identity + prose + metadata; edges added by the
/// pairing helpers below).
fn nf(layer: &str, node_type: &str, name: &str, body: &str, props: &[(&str, &str)]) -> NodeFile {
    NodeFile {
        layer: layer.to_string(),
        node_type: node_type.to_string(),
        name: name.to_string(),
        body: body.to_string(),
        properties: props
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        out: Vec::new(),
        in_edges: Vec::new(),
    }
}

fn out_e(kind: &str, target: &str) -> OutEdge {
    OutEdge {
        kind: kind.to_string(),
        target: target.to_string(),
        properties: BTreeMap::new(),
    }
}

fn in_e(kind: &str, source: &str) -> InEdge {
    InEdge {
        kind: kind.to_string(),
        source: source.to_string(),
        properties: BTreeMap::new(),
    }
}

/// Pushes a node and records its FQN (`<layer>.<type>.<name>`) in `idx`.
fn tier_push(nodes: &mut Vec<NodeFile>, idx: &mut BTreeMap<String, usize>, n: NodeFile) {
    let f = format!("{}.{}.{}", n.layer, n.node_type, n.name);
    idx.insert(f, nodes.len());
    nodes.push(n);
}

/// Adds one edge to BOTH endpoint files (SPEC §4.1 pairwise rule — out in
/// the source's file, matching in in the target's).
fn tier_edge(
    nodes: &mut [NodeFile],
    idx: &BTreeMap<String, usize>,
    kind: &str,
    from: &str,
    to: &str,
) {
    nodes[idx[from]].out.push(out_e(kind, to));
    nodes[idx[to]].in_edges.push(in_e(kind, from));
}

/// The re-materialized apg-projects tiers in the layers model: tier 1
/// (stakeholder/user + the 20 requirements R1–R20 with their depends-on
/// edges, AC constraints, background/design notes), tier 2 (the
/// change-sets domain: group/entities/events/value + the four domain laws
/// as constraints), tier 3 (the apg-cli C4 solution: system + five
/// containers with `implemented-by` code refs), threaded through the
/// strictly sequential spine (Requirement —Drives→ Domain —RealisedBy→
/// Solution —ImplementedBy→ code). Bodies are condensed from
/// plans/SPEC-apg-projects.md §1–6 and the old-model bootstrap
/// apg/specs/apg-projects.jsonl — the lossy mapping is the spec-writer's
/// judgement (SPEC §4.1 "re-materialize instead").
fn apg_projects_tier_nodes() -> Vec<NodeFile> {
    let mut nodes: Vec<NodeFile> = Vec::new();
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();

    // --- Tier 1: requirements -------------------------------------
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "stakeholder",
            "maintainer",
            "The apg maintainers — anyone with an interest in the projects model and its dogfooding (SPEC §1: a thing that has an opinion).",
            &[],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "user",
            "agent",
            "The codebase agents (navigator/implementer) — a thing that uses the system (SPEC §6 agent flow: the navigator runs `apg project start <name>` from the main checkout and operates with cwd inside the worktree).",
            &[],
        ),
    );
    // (id, feature, body) — condensed from the old-model bootstrap
    // requirements (apg/specs/apg-projects.jsonl), source cites kept.
    let reqs: &[(&str, &str, &str)] = &[
        (
            "R1",
            "project-start",
            "`apg project start <name>` creates the project context in one command: worktree + branch off the repo's DEFAULT branch (symbolic HEAD, not literal main) + auto-scan (worktree + branch + branch DB). Worktree location fixed at <main>/apg/.worktrees/<project>. Always a branch — no escape hatch. Idempotent ONLY when <name> matches the current project context; otherwise hard fail. AC: one command yields worktree + branch + branch DB. Source: SPEC-apg-projects.md §2.1.",
        ),
        (
            "R2",
            "project-start",
            "Hard refusals at project start: non-git dirs (suggest `apg init`); dirty main at start; unborn HEAD; invalid branch names NEVER sanitized (project names inherit git refname constraints); every collision is a case-specific \"project already exists\" naming the actual state and the fix command. `project start` is a main-checkout operation: run from inside a worktree → hard fail (\"you work one project at a time\"). AC: refusal exits 1 with a fix line. Source: SPEC-apg-projects.md §2.1.",
        ),
        (
            "R3",
            "mutation-guard",
            "The project membership guard: guards WRITES only — reads are always allowed; `project start` itself is unguarded (the entry point). Writes refuse outside a project context. Membership = \"the project's worktree, on the project's branch\": current branch == project name AND current checkout is the project's worktree; failure messages name which half failed. Main is never a mutation place — delivered or not. AC: a failure names which half failed. Source: SPEC-apg-projects.md §2.2.",
        ),
        (
            "R4",
            "mutation-guard",
            "The guard lives in the central mutation funnel — write_jsonl_and_reingest (src/artifacts.rs) — beside the existing staleness gate: one check covers all mutations. Consequence: non-git test fixtures gain a real project context (real git fixtures with branch + worktree). Source: SPEC-apg-projects.md §2.3.",
        ),
        (
            "R5",
            "mutation-guard",
            "`apg plan apply` is renamed `apg plan verify` — the binary applies nothing; verify is the pre-merge coherence gate (no remaining planned/dangling nodes + all feedback resolved), guarded (a verdict is only meaningful against the branch's graph). `apg project merge` = verify gate → merge → main rebuild: binary-operated via git2 from the main checkout — the project's terminal lifecycle act; the rebuild is a plain unguarded scan on main. Source: SPEC-apg-projects.md §2.2 + §2.3.",
        ),
        (
            "R6",
            "binary-plumbing",
            "git2 crate with default-features = false (no https/ssh → no OpenSSL). The git CLI is never shelled out to; push/tag remain human-approved acts. VI: cargo check and cargo test pass green with git2 default-features=false. Source: SPEC-apg-projects.md §2.3.",
        ),
        (
            "R7",
            "binary-plumbing",
            "Root resolution split: keep the existing walk-up for checkout-local apg/ (correct in worktrees by construction); git2 for identity (branch, checkout path, main path, worktree existence). Invariant: the git checkout root contains the walked-up apg/ — verify cheaply, error on divergence. Source: SPEC-apg-projects.md §2.3.",
        ),
        (
            "R8",
            "binary-plumbing",
            "Graph mutations auto-commit on the project branch via git2: one commit per logical mutation (single-file diffs); after an auto-commit the staleness gate's recorded scan_meta is re-anchored (DB and tree in sync by construction). Plan mutations NEVER commit — .trans is gitignored and transient. Source: SPEC-apg-projects.md §2.3 + §4.2.",
        ),
        (
            "R9",
            "init-version-gate",
            "`apg init` scaffolds apg/.worktrees/ + its gitignore entry and writes the binary version into apg/config.json as a binary-managed `version` field (user code_type rules untouched). `project start` self-heals the dir/gitignore if absent. Source: SPEC-apg-projects.md §2.4.",
        ),
        (
            "R10",
            "init-version-gate",
            "The version gate BLOCKS, never warns: same major.minor → proceed (patch diff fine); missing `version` → block; major or minor mismatch in EITHER direction → block with upgrade guidance. Applies to `apg scan` and `apg project start` (the layout-touching ops); `apg init` is the upgrade act. Source: SPEC-apg-projects.md §2.4.",
        ),
        (
            "R11",
            "tier-model",
            "One model per layer (2.0 catalog): requirements = Stakeholder/User/Requirement/Note/Constraint with the requirement tree at all depths (theme → epic → feature → story is ONE node type; decompose until each requirement is atomic/testable); domain = Group/Entity/Value/Service/Note/Constraint; solution = System/Container/Component/Person/Note/Constraint; plans = the bridge, .trans-only; implementation = code — scanned, never serialized (Note/Constraint attach only); global = Constraint (the laws) + Notes. Source: SPEC-apg-projects.md §3.1.",
        ),
        (
            "R12",
            "tier-model",
            "Domain semantics use PLAIN names (DDD nomenclature is cryptic): Group hierarchical (groups in groups), attributes core/supporting/generic + optional root (aggregate-groups); BoundedContext/Subdomain/Aggregate/DomainRule collapse into it. Entity kind entity|event (events are ephemeral entities with motion, not a type). Value immutable. Service stateless behaviour. Container kind app/service/db/queue. Solution = C4 only; Person is the C4 view of User/Stakeholder. Stakeholder = anyone with an interest (\"a thing that has an opinion\"); User ⊂ Stakeholder (\"a thing that uses the system\"). Source: SPEC-apg-projects.md §3.1.",
        ),
        (
            "R13",
            "tier-model",
            "Spine strictly sequential — no tier skips (lint): Stakeholder ⊃ Requirement —Drives→ Domain —RealisedBy→ Solution —ImplementedBy→ code. Write-time edge-kind validation matrix (§3.3): contains/drives/realised-by/implemented-by/calls/publishes/subscribes/depends-on/uses/represents/details with their exact source/target shapes. Node rules: name allowlist [a-z0-9][a-z0-9-]* (refuse, never sanitize); type must exist in its layer; Entity requires kind; Group takes core/supporting/generic + optional root; Container takes app/service/db/queue; FQN = <layer>.<type>.<name>; names unique per (layer, type); contains/depends-on trees acyclic; dangling FQN references are write-time errors. Source: SPEC-apg-projects.md §3.2 + §3.3.",
        ),
        (
            "R14",
            "tier-model",
            "Coupling is DERIVED, never stored: A and B are coupled iff a service/event edge chain connects them. The DDD context-map flavors (direct/published/translated/shared/coevolving) are EDGE ATTRIBUTES on calls/publishes/subscribes — never node types, never Group→Group edges. Constraints are PROSE (\"X must hold\") over things that EXIST: the binary validates structure and references at write time; global constraints guard the whole graph; local constraints attach to any tier-1–3 node; SATISFACTION IS ASSESSED BY REVIEW, never executed. Source: SPEC-apg-projects.md §3.1 + §3.3.",
        ),
        (
            "R15",
            "serialization",
            "Layout + identity: the SIX-layer catalog with storage policy separate from it — plans serialize only under apg/.trans/plans/ (transient, per branch); implementation = the code (scanned, never serialized; attach-only note/constraint durable dir); the other four durable. apg/layers/ tree plus the complete .trans mirrors (all six tiers incl. global). One file per node; the file name IS the identity: FQN = <layer>.<type>.<name>, no project prefix. Node-file schema: layer/type/name/body/properties/in/out. Short ids may exist as metadata only, never as identity. Source: SPEC-apg-projects.md §4.1.",
        ),
        (
            "R16",
            "serialization",
            "Edge pairing + atomic write-throughs: BOTH in and out edges live in the node file; an in/out edge in one file without the matching out/in edge (same source, kind, target, AND properties) in the other endpoint's file is an ERROR caught at ingestion; outgoing edges are canonical. Transient-to-durable relationships stay ENTIRELY in .trans. Code endpoints are EXEMPT from the pairwise rule — implemented-by is recorded spec-side only, validated against the scanned graph: resolves → real; planned → pending, not an error; gone → error (spec drift). Renames/deletions are atomic write-throughs: one logical mutation updates ALL affected files and commits once; restore the previous state on failure. Source: SPEC-apg-projects.md §4.1.",
        ),
        (
            "R17",
            "serialization",
            "No migration: legacy apg/specs/*.jsonl and apg/notes/ are NOT read; the version gate blocks old layouts; re-materialize instead — the lossy mapping is the spec-writer's judgement, not a converter's. The old `apg spec` / `apg invariant` command surfaces and the old-model graph vocabulary are removed from the binary — node kinds are exactly the §3.1 catalog plus the code kinds and the transient plan/review kinds; edge kinds are exactly the §3.3 matrix plus the §5 plan/feedback edges. Source: SPEC-apg-projects.md §4.1.",
        ),
        (
            "R18",
            "plans-transient",
            "Plans are tier 4 — the bridge. Plan nodes (PlanPhase, Task, planned Implementation nodes) exist ONLY in apg/.trans/plans/ per branch, never durable. Plan edges: contains (Plan ⊃ PlanPhase ⊃ Task), gates (PlanPhase→PlanPhase), satisfies (PlanPhase→Requirement), and Task→Implementation verbs (creates/modifies/deletes/renames/moves — further verbs reveal themselves through dogfooding). Feedback is transient — branch-lifecycle data, never committed; .trans mirrors the layers structure; Reviews edges link feedback to durable nodes; review state dies with the branch, the reviewed nodes persist. Verification items are the plan's test tier (unit/int/e2e), not graph content. Source: SPEC-apg-projects.md §5.",
        ),
        (
            "R19",
            "plans-transient",
            "Coverage is derived and enforced: every solution node's implemented-by FQN must be touched by at least one plan task; the plan is the HOW for the whole solution; the bridge is complete iff coverage holds. Source: SPEC-apg-projects.md §5.",
        ),
        (
            "R20",
            "rollout",
            "Standalone change-set + dogfood operational flow: the apg repo dogfoods the model — THIS spec is the bootstrap dogfood (authored with the v0.10.4 binary in the old model, within its boundaries; branch/worktree created manually because `apg project start` does not exist yet; from the next feature onward the binary handles it). Agent flow: the navigator runs `apg project start <name>` from the main checkout; the binary prints the worktree path; the navigator operates with cwd inside the worktree; suite tools work unchanged — walk-up discovery finds the worktree's own apg/. Test worktree apg/.worktrees/test stays until before shipping; cleanup deletes no branch. Source: SPEC-apg-projects.md §6.",
        ),
    ];
    for (id, feature, body) in reqs {
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "requirement",
                &id.to_lowercase(),
                body,
                &[("id", id), ("feature", feature)],
            ),
        );
    }
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "constraint",
            "ac-start-one-command",
            "R1 AC (§2.1): one `apg project start <name>` command yields the worktree + branch + branch DB — the branch DB exists after start.",
            &[],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "constraint",
            "ac-refusals-name-fix",
            "R2 AC (§2.1): every hard-refusal case exits 1 with a fix line on stderr naming the actual state and the command that fixes it.",
            &[],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "constraint",
            "ac-membership-names-half",
            "R3 AC (§2.2): a refused mutation's error names which membership half failed (branch half vs worktree half) plus one actionable fix line.",
            &[],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "note",
            "bootstrap-dogfood",
            "This session is the bootstrap dogfood: branch/worktree created manually because `apg project start` does not exist yet; from the next feature onward the binary handles it. This change-set is standalone (SPEC §6).",
            &[("kind", "background")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "note",
            "worktree-safety",
            "The fixed worktree location <main>/apg/.worktrees/<project> is a gitignored path INSIDE the main checkout — tested and verified safe; the test worktree apg/.worktrees/test stays until before shipping; cleanup deletes no branch (SPEC §2.1/§6).",
            &[("kind", "background")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "requirements",
            "note",
            "write-through-regression",
            "Regression target (bootstrap note-18): a node rewrite must preserve/rewrite ALL incident edges — never drop them silently. The old-model body-upsert did exactly that; the 2.0 atomic write-through (R16, §4.1) is the fix.",
            &[("kind", "design")],
        ),
    );

    // --- Tier 2: domain -------------------------------------------
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "domain",
            "group",
            "change-sets",
            "The projects-model domain (SPEC §1): a project is a container for a change-set over the graph (and the code that underpins it) — a project ≠ a spec ≠ a plan, it CONTAINS those things. The worktree/branch context, the membership guard, the node-file serialization semantics, and the .trans plan bridge all express this one domain concept.",
            &[("attribute", "core")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "domain",
            "entity",
            "project",
            "The change-set container created by `apg project start <name>`: a git worktree at <main>/apg/.worktrees/<project> on a branch named after the project — branch name == project name is the membership mechanism — off the repo's DEFAULT branch (SPEC §2.1).",
            &[("kind", "entity")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "domain",
            "entity",
            "graph-node",
            "The universal graph node the serialization and pairwise-edge rules talk about: code nodes (scanned, never serialized) and tier-1–3 nodes (one file per node under the apg/layers/ tree) (SPEC §3.1/§5).",
            &[("kind", "entity")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "domain",
            "entity",
            "project-started",
            "The event a change-set comes into being: `apg project start <name>` creates the project context — worktree + branch + auto-scan branch DB in one command (SPEC §2.1).",
            &[("kind", "event")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "domain",
            "entity",
            "node-file-written",
            "The event a node file lands in the worktree: every node/edge mutation writes its file(s) and auto-commits on the project branch — one commit per logical mutation (SPEC §4.2).",
            &[("kind", "event")],
        ),
    );
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "domain",
            "value",
            "node-name",
            "A node's file name IS its identity — the name allowlist [a-z0-9][a-z0-9-]* (refuse, never sanitize); FQN = <layer>.<type>.<name>, no project prefix (SPEC §3.3/§4.1).",
            &[],
        ),
    );
    for (name, body) in [
        (
            "mutation-requires-project",
            "\"It is not possible to mutate the graph without a project — at all\" (SPEC §2.2): writes refuse outside a project context — membership = the project's worktree on the project's branch.",
        ),
        (
            "pairwise-edge-matching",
            "An edge appears in BOTH endpoint files (out in the source's, in in the target's); a match requires the same source, kind, target, AND properties; outgoing edges are canonical (SPEC §4.1).",
        ),
        (
            "plan-covers-solution",
            "Every solution node's implemented-by FQN must be touched by at least one plan task; the bridge is complete iff coverage holds (SPEC §5).",
        ),
        (
            "drift-is-error",
            "An implemented-by code FQN gone from the scanned graph is spec drift and an error — the scanned graph is the stronger check (resolves → real, planned → pending, gone → error) (SPEC §4.1).",
        ),
    ] {
        tier_push(
            &mut nodes,
            &mut idx,
            nf("domain", "constraint", name, body, &[]),
        );
    }

    // --- Tier 3: solution -----------------------------------------
    tier_push(
        &mut nodes,
        &mut idx,
        nf(
            "solution",
            "system",
            "apg-cli",
            "The apg binary itself — the single C4 system that hosts the project commands, the mutation guard, the layers serializer, the plan bridge and the init/version gate (SPEC §2.3/§6).",
            &[],
        ),
    );
    for (name, kind, body) in [
        (
            "project-commands",
            "app",
            "The `apg project` command surface: `apg project start <name>` (§2.1) and `apg project merge` = verify gate → merge → main rebuild, binary-operated via git2 from the main checkout (§2.3).",
        ),
        (
            "mutation-guard",
            "service",
            "The cross-cutting enforcement point: the project membership guard in the central mutation funnel write_jsonl_and_reingest beside the staleness gate (§2.2/§2.3), plus the pre-merge coherence gate behind the renamed `apg plan verify` (§2.2).",
        ),
        (
            "layers-serializer",
            "app",
            "The node-file serializer behind §3 (tier model, edge-kind validation matrix) and §4 (apg/layers/ layout, one file per node, pairwise edges, atomic write-throughs, auto-commit).",
        ),
        (
            "plan-bridge",
            "app",
            "The transient tier-4 bridge (§5): PlanPhase/Task/planned Implementation nodes live only in apg/.trans/plans/ per branch, never durable; Task→Implementation verbs; feedback transient in .trans mirrors.",
        ),
        (
            "init-version-gate",
            "app",
            "`apg init` scaffolding apg/.worktrees/ + gitignore entry + binary-managed `version` in apg/config.json (§2.4), and the version gate (blocks not warns; applies to apg scan + apg project start; init is the upgrade act).",
        ),
    ] {
        tier_push(
            &mut nodes,
            &mut idx,
            nf("solution", "container", name, body, &[("kind", kind)]),
        );
    }

    // --- The spine + trees (paired both halves, SPEC §3.2/§3.3) ----
    let req_fqns: Vec<String> = (1..=20)
        .map(|i| format!("requirements.requirement.r{i}"))
        .collect();
    // contains: User ⊃ every Requirement.
    for r in &req_fqns {
        tier_edge(&mut nodes, &idx, "contains", "requirements.user.agent", r);
    }
    // depends-on (SPEC §5 source ordering; the R8→R6/R7 pair mirrors the
    // bootstrap feedback-1 resolution).
    for (from, to) in [
        ("r2", "r1"),
        ("r3", "r1"),
        ("r4", "r3"),
        ("r5", "r4"),
        ("r7", "r6"),
        ("r8", "r6"),
        ("r8", "r7"),
        ("r10", "r9"),
        ("r15", "r10"),
        ("r16", "r15"),
        ("r17", "r10"),
        ("r19", "r18"),
    ] {
        tier_edge(
            &mut nodes,
            &idx,
            "depends-on",
            &format!("requirements.requirement.{from}"),
            &format!("requirements.requirement.{to}"),
        );
    }
    // drives: every Requirement → the change-sets domain group.
    for r in &req_fqns {
        tier_edge(&mut nodes, &idx, "drives", r, "domain.group.change-sets");
    }
    // contains: the change-sets group hosts its entities/events/value.
    for (name, node_type) in [
        ("project", "entity"),
        ("graph-node", "entity"),
        ("project-started", "entity"),
        ("node-file-written", "entity"),
        ("node-name", "value"),
    ] {
        tier_edge(
            &mut nodes,
            &idx,
            "contains",
            "domain.group.change-sets",
            &format!("domain.{node_type}.{name}"),
        );
    }
    // realised-by: Domain → Solution (the strictly sequential spine).
    tier_edge(
        &mut nodes,
        &idx,
        "realised-by",
        "domain.group.change-sets",
        "solution.system.apg-cli",
    );
    tier_edge(
        &mut nodes,
        &idx,
        "realised-by",
        "domain.entity.project",
        "solution.container.project-commands",
    );
    tier_edge(
        &mut nodes,
        &idx,
        "realised-by",
        "domain.entity.graph-node",
        "solution.container.layers-serializer",
    );
    // contains: System ⊃ the five Containers.
    for c in [
        "project-commands",
        "mutation-guard",
        "layers-serializer",
        "plan-bridge",
        "init-version-gate",
    ] {
        tier_edge(
            &mut nodes,
            &idx,
            "contains",
            "solution.system.apg-cli",
            &format!("solution.container.{c}"),
        );
    }
    // implemented-by: Solution → code FQNs (code-exempt — spec-side only,
    // validated against the scanned graph at write time and at scan).
    for (i, container) in [
        "project-commands",
        "mutation-guard",
        "layers-serializer",
        "plan-bridge",
        "init-version-gate",
    ]
    .iter()
    .enumerate()
    {
        nodes[idx[&format!("solution.container.{container}")]]
            .out
            .push(out_e("implemented-by", IMPL_FQNS[i]));
    }
    // details: notes attach to the nodes they explain.
    tier_edge(
        &mut nodes,
        &idx,
        "details",
        "requirements.note.bootstrap-dogfood",
        "requirements.requirement.r20",
    );
    tier_edge(
        &mut nodes,
        &idx,
        "details",
        "requirements.note.worktree-safety",
        "domain.entity.project",
    );
    tier_edge(
        &mut nodes,
        &idx,
        "details",
        "requirements.note.write-through-regression",
        "domain.constraint.pairwise-edge-matching",
    );

    nodes
}

/// The transient plan that pairs with the re-materialized tiers (SPEC §5):
/// one phase satisfying the rollout requirement, and one `modifies` task
/// per solution `implemented-by` FQN — derived coverage holds, no planned
/// nodes, no feedback, so the verify gate passes green. FQNs are
/// project-prefixed (`<project>/plan…`), matching the branch/project name
/// the plan JSONL lives under (`.trans/plans/<project>.jsonl`).
fn apg_projects_plan_records(project: &str) -> Vec<Record> {
    let mut r: Vec<Record> = vec![
        Record::Plan {
            fqn: format!("{project}/plan"),
            title: "apg-projects plan".to_string(),
            strategy:
                "Bootstrap dogfood: re-materialize the change-set in the layers model (SPEC §6)"
                    .to_string(),
        },
        Record::PlanPhase {
            fqn: format!("{project}/plan.phase-01"),
            number: 1,
            title: "rollout".to_string(),
            deliverable: "re-materialized tiers".to_string(),
            status: "pending".to_string(),
        },
        Record::Contains {
            from: format!("{project}/plan"),
            to: format!("{project}/plan.phase-01"),
            properties: layers::NodeProperties::default(),
        },
        Record::Satisfies {
            from: format!("{project}/plan.phase-01"),
            to: "requirements.requirement.r20".to_string(),
        },
    ];
    for i in 1..=19 {
        r.push(Record::Satisfies {
            from: format!("{project}/plan.phase-01"),
            to: format!("requirements.requirement.r{i}"),
        });
    }
    for (i, code) in IMPL_FQNS.iter().enumerate() {
        let task = format!("{project}/plan.phase-01.task-{}", i + 1);
        r.push(Record::Task {
            fqn: task.clone(),
            title: format!("touch {code}"),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: code.to_string(),
            new_fqn: String::new(),
        });
        r.push(Record::Contains {
            from: format!("{project}/plan.phase-01"),
            to: task,
            properties: layers::NodeProperties::default(),
        });
    }
    r
}

/// Every durable FQN the re-materialization authors — the e2e asserts the
/// branch DB holds all of them (tier 1 + tier 2 + tier 3).
fn expected_tier_fqns() -> Vec<String> {
    let mut fqns: Vec<String> = (1..=20)
        .map(|i| format!("requirements.requirement.r{i}"))
        .collect();
    fqns.extend([
        "requirements.stakeholder.maintainer".to_string(),
        "requirements.user.agent".to_string(),
        "requirements.constraint.ac-start-one-command".to_string(),
        "requirements.constraint.ac-refusals-name-fix".to_string(),
        "requirements.constraint.ac-membership-names-half".to_string(),
        "requirements.note.bootstrap-dogfood".to_string(),
        "requirements.note.worktree-safety".to_string(),
        "requirements.note.write-through-regression".to_string(),
        "domain.group.change-sets".to_string(),
        "domain.entity.project".to_string(),
        "domain.entity.graph-node".to_string(),
        "domain.entity.project-started".to_string(),
        "domain.entity.node-file-written".to_string(),
        "domain.value.node-name".to_string(),
        "domain.constraint.mutation-requires-project".to_string(),
        "domain.constraint.pairwise-edge-matching".to_string(),
        "domain.constraint.plan-covers-solution".to_string(),
        "domain.constraint.drift-is-error".to_string(),
        "solution.system.apg-cli".to_string(),
        "solution.container.project-commands".to_string(),
        "solution.container.mutation-guard".to_string(),
        "solution.container.layers-serializer".to_string(),
        "solution.container.plan-bridge".to_string(),
        "solution.container.init-version-gate".to_string(),
    ]);
    fqns
}

/// Opens the project worktree's repository, mutates one tracked path in a
/// new commit, and returns its head sha — the "project work in progress"
/// state a delete abandons.
fn wt_append_commit(wt: &Path, rel: &str, content: &str, msg: &str) -> String {
    let p = wt.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    wt_commit(wt, &[rel], msg)
}

/// Asserts the project `name` of the fixture `repo` is fully present:
/// branch exists, worktree registered, worktree dir exists.
fn assert_project_present(repo: &testutil::Repo, name: &str) {
    let main_repo = git2_repo_test(repo);
    assert!(
        main_repo.find_branch(name, git2::BranchType::Local).is_ok(),
        "project `{name}` branch must exist"
    );
    assert!(
        main_repo.find_worktree(name).is_ok(),
        "project `{name}` worktree must be registered"
    );
    assert!(
        repo.project_worktree_dir(name).is_dir(),
        "project `{name}` worktree dir must exist"
    );
}

/// Asserts the project `name` of the fixture `repo` is fully gone:
/// branch deleted, worktree unregistered, worktree dir removed.
fn assert_project_gone(repo: &testutil::Repo, name: &str) {
    let main_repo = git2_repo_test(repo);
    assert!(
        main_repo
            .find_branch(name, git2::BranchType::Local)
            .is_err(),
        "project `{name}` branch must be deleted"
    );
    assert!(
        main_repo.find_worktree(name).is_err(),
        "project `{name}` worktree must be unregistered"
    );
    assert!(
        !repo.project_worktree_dir(name).exists(),
        "project `{name}` worktree dir must be removed"
    );
}

fn git2_repo_test(repo: &testutil::Repo) -> git2::Repository {
    git2::Repository::open(&repo.root).unwrap()
}

/// A scratch git repo with real Go sources + a versioned `apg/` layout,
/// driven by the REAL candidate binary. The isolated `home` is a sibling
/// OUTSIDE the repo, so the checkout stays clean for `project start`.
fn warm_scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("apg-warm-start-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo_dir = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
    std::fs::create_dir_all(&repo_dir).unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("refs/heads/main");
    let repo = git2::Repository::init_opts(&repo_dir, &opts).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "apg warm test").unwrap();
        cfg.set_str("user.email", "apg-warm@example.com").unwrap();
    }
    std::fs::write(
        repo_dir.join(".gitignore"),
        "apg/.trans/\napg/.worktrees/\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo_dir.join(specs::LAYOUT).join(specs::TRANS)).unwrap();
    std::fs::write(
        repo_dir.join(specs::LAYOUT).join("config.json"),
        format!(
            "{{\n  \"default\": \"src\",\n  \"types\": [],\n  \"version\": \"{}\"\n}}\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    std::fs::write(repo_dir.join("go.mod"), "module scratch\n\ngo 1.21\n").unwrap();
    let fixture = [
        (
            "a/a.go",
            "package a\n\ntype A struct {\n\tX int\n}\n\nfunc Leaf() int { return 1 }\n",
        ),
        (
            "b/b.go",
            "package b\n\nimport \"scratch/a\"\n\ntype B struct {\n\tA a.A\n}\n\nfunc Foo() int { return a.Leaf() }\n",
        ),
        (
            "c/c.go",
            "package c\n\nimport \"scratch/b\"\n\nfunc Bar() int { return b.Foo() }\n",
        ),
    ];
    for (rel, body) in fixture {
        let p = repo_dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    warm_commit_all(&repo_dir, "init source");
    (base, repo_dir, home)
}

fn warm_commit_all(dir: &Path, msg: &str) {
    let repo = git2::Repository::open(dir).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = repo.signature().unwrap();
    let head = repo.head().ok().map(|h| h.peel_to_commit().unwrap());
    let parents: Vec<&git2::Commit> = head.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, parents.as_slice())
        .unwrap();
}

/// Runs the candidate binary with the isolated `HOME`.
fn warm_run(repo_dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    testutil::ApgCommand::new(args)
        .cwd(repo_dir)
        .env("HOME", &home.to_string_lossy())
        .output()
}

/// Every `graph.jsonl` record except the leading `scan_meta` (the code +
/// layer/transient records a full scan and the warm assembly must agree on).
fn graph_records(dir: &Path) -> std::collections::BTreeSet<String> {
    let path = dir
        .join(specs::LAYOUT)
        .join(specs::TRANS)
        .join("graph.jsonl");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .filter(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap_or(serde_json::Value::Null);
            v.get("type").and_then(|t| t.as_str()) != Some("scan_meta")
        })
        .map(str::to_string)
        .collect()
}

/// e2e tier -- real I/O: every test here creates/merges/deletes real git
/// worktrees and branches, or drives the candidate `apg` binary. Each is
/// `#[ignore]`d, so a plain `cargo test` never runs one; the only entry
/// point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    /// fast-project-start task-5 (e2e): `apg project start` at a
    /// freshly-scanned main copies main's `apg/.trans` into the branch
    /// verbatim — the branch `db.lbug` + `graph.jsonl` are byte-identical
    /// to main's, and NO new frontend runs (the copied `apg-frontend.log`
    /// still carries main's single frontend run and nothing is appended).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
    fn warm_cache_project_start_spawns_no_frontend_and_equals_a_full_scan() {
        let (base, repo_dir, home) = warm_scratch("warm");

        // Full scan on the main checkout: populates main's `.trans`
        // (db.lbug + graph.jsonl + the frontend log) at the recorded HEAD.
        let scan = warm_run(&repo_dir, &home, &["scan", "."]);
        assert!(
            scan.status.success(),
            "main scan: {}",
            String::from_utf8_lossy(&scan.stderr)
        );
        // The baseline genuinely spawned the go frontend.
        let main_trans = repo_dir.join(specs::LAYOUT).join(specs::TRANS);
        let main_log = std::fs::read_to_string(main_trans.join("apg-frontend.log")).unwrap();
        assert!(
            main_log.contains("running go frontend"),
            "the baseline full scan must spawn the go frontend: {main_log}"
        );
        let main_db = std::fs::read(main_trans.join("db.lbug")).unwrap();
        let main_graph = std::fs::read(main_trans.join("graph.jsonl")).unwrap();
        let full_scan_records = graph_records(&repo_dir);
        assert!(
            !full_scan_records.is_empty(),
            "the full scan must emit a graph"
        );

        // Project start: worktree + branch + branch DB copied from main —
        // no frontend scan, no new frontend process.
        let start = warm_run(&repo_dir, &home, &["project", "start", "foo"]);
        assert!(
            start.status.success(),
            "project start: {}",
            String::from_utf8_lossy(&start.stderr)
        );

        let wt = repo_dir.join(specs::LAYOUT).join(".worktrees").join("foo");
        let wt_trans = wt.join(specs::LAYOUT).join(specs::TRANS);
        assert!(
            wt_trans.join("db.lbug").exists(),
            "the branch db.lbug must be seeded"
        );

        // Byte-identical copy: the branch DB and graph ARE main's scan, and
        // the copied log is unchanged — start appended nothing, so no new
        // frontend ran.
        assert_eq!(
            std::fs::read(wt_trans.join("db.lbug")).unwrap(),
            main_db,
            "the branch db.lbug must be byte-identical to main's"
        );
        assert_eq!(
            std::fs::read(wt_trans.join("graph.jsonl")).unwrap(),
            main_graph,
            "the branch graph.jsonl must be byte-identical to main's"
        );
        assert_eq!(
            std::fs::read_to_string(wt_trans.join("apg-frontend.log")).unwrap(),
            main_log,
            "the copied frontend log must be byte-identical — no new frontend ran"
        );

        // Same code nodes and edges as the full scan of the commit.
        assert_eq!(
            graph_records(&wt),
            full_scan_records,
            "the branch graph must equal a full scan of the commit"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// fast-project-start task-6 (e2e): `apg project start` refuses when
    /// the main checkout has no scan OR a stale one, naming `apg scan` in
    /// the main checkout. The refusal sits before the gitignore self-heal,
    /// so it leaves main (and its `.gitignore`) untouched and creates no
    /// branch/worktree.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
    fn start_refuses_when_main_scan_is_stale_or_missing() {
        // (a) No main scan at all (no `.trans/db.lbug`). The worktrees
        // ignore entry is dropped too, so a refusal that reached the
        // self-heal would visibly mutate main's `.gitignore`.
        let repo = Repo::new("start-scan-missing");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.write(".gitignore", "apg/.trans/\n");
        repo.commit_all("seed code, drop worktrees ignore");

        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("stale or missing"), "{msg}");
        assert!(msg.contains("apg scan"), "{msg}");
        assert!(msg.contains("main checkout"), "{msg}");
        // The refusal precedes the self-heal: `.gitignore` is untouched and
        // main is still clean.
        let ignore = std::fs::read_to_string(repo.root.join(".gitignore")).unwrap();
        assert!(
            !ignore.contains("apg/.worktrees/"),
            "a freshness refusal must not run the gitignore self-heal: {ignore}"
        );
        assert!(repo.is_clean(), "the refusal must leave main untouched");
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert!(
            main_repo
                .find_branch("foo", git2::BranchType::Local)
                .is_err(),
            "no project branch may be created"
        );
        assert!(
            main_repo.find_worktree("foo").is_err(),
            "no project worktree may be created"
        );
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);

        // (b) A stale recorded scan: a fresh scan, then a later commit on
        // main makes the recorded sha/key no longer match HEAD.
        let repo = Repo::new("start-scan-stale");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        start_scan(&repo.root).unwrap();
        assert!(
            git::is_fresh(&repo.apg_root()),
            "the fixture scan must be fresh"
        );
        repo.write(
            "code/more.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Later"]),
        );
        repo.commit_all("later change");
        assert!(
            !git::is_fresh(&repo.apg_root()),
            "the recorded scan is now stale for HEAD"
        );

        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("stale or missing"), "{msg}");
        assert!(msg.contains("apg scan"), "{msg}");
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert!(
            main_repo
                .find_branch("foo", git2::BranchType::Local)
                .is_err(),
            "no project branch may be created"
        );
        assert!(
            main_repo.find_worktree("foo").is_err(),
            "no project worktree may be created"
        );
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // task-4 AC (int): one command yields worktree + branch + branch DB off
    // the DEFAULT branch; in-context re-run no-ops and prints the path;
    // identity correct in worktree vs main.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_creates_worktree_branch_and_branch_db() {
        let repo = Repo::new("start-ac");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        let main_sha = repo.commit_all("seed code");
        // start seeds the branch from main's scan, so main must be scanned.
        start_scan(&repo.root).unwrap();

        let wt = project_start_at(&repo.apg_root(), "foo").unwrap();
        assert_eq!(wt, repo.project_worktree_dir("foo").canonicalize().unwrap());
        assert!(wt.is_dir());

        // The branch exists, off the default branch (main), at main's tip.
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        assert!(!branch.is_head());
        let branch_commit = branch.get().peel_to_commit().unwrap();
        assert_eq!(branch_commit.id().to_string(), main_sha);

        // The worktree's HEAD is the project branch.
        let wt_repo = git2::Repository::open(&wt).unwrap();
        assert_eq!(wt_repo.head().unwrap().shorthand(), Some("foo"));

        // The branch DB exists after start (AC-1).
        assert!(wt.join("apg").join(specs::TRANS).join("db.lbug").exists());

        // Identity correct in worktree vs main (R7).
        let id_wt = git::repo_identity(&wt.join(specs::LAYOUT)).unwrap();
        assert!(id_wt.is_worktree);
        assert_eq!(id_wt.branch.as_deref(), Some("foo"));
        assert_eq!(id_wt.default_branch.as_deref(), Some("main"));
        let id_main = git::repo_identity(&repo.apg_root()).unwrap();
        assert!(!id_main.is_worktree);
        assert_eq!(id_main.branch.as_deref(), Some("main"));

        // In-context re-run: no-op, prints the path, still Ok.
        let again = project_start_at(&wt.join(specs::LAYOUT), "foo").unwrap();
        assert_eq!(again, wt);

        // From the main checkout the same name is a hard collision.
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_branches_off_the_repo_default_not_literal_main() {
        // The default branch is the main checkout's symbolic HEAD (fallback:
        // main). The project branch must sit exactly on it.
        let repo = Repo::new("start-default");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        // Rename the default to `trunk` (like a repo whose default is trunk).
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let mut branch = main_repo
            .find_branch("main", git2::BranchType::Local)
            .unwrap();
        branch.rename("trunk", true).unwrap();
        main_repo.set_head("refs/heads/trunk").unwrap();
        // start copies main's scan, so scan after the rename (the sha is
        // unchanged, but scan on the checkout's current HEAD).
        start_scan(&repo.root).unwrap();

        let _wt = project_start_at(&repo.apg_root(), "foo").unwrap();
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        let branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        assert_eq!(
            branch.get().peel_to_commit().unwrap().id(),
            head.id(),
            "project branch must sit on the default branch tip"
        );
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // task-6 AC (unit): every refusal exits 1 naming the actual state and
    // one actionable fix command; run-from-worktree hard-fails.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_non_git_with_apg_init_suggestion() {
        let dir = std::env::temp_dir().join(format!("apg-start-nongit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        let err = project_start_at(&dir.join("apg"), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("git repository"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_unborn_head() {
        let root = std::env::temp_dir().join(format!("apg-start-unborn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("refs/heads/main");
        git2::Repository::init_opts(&root, &opts).unwrap();
        std::fs::create_dir_all(root.join("apg").join(specs::TRANS)).unwrap();
        let err = project_start_at(&root.join("apg"), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unborn HEAD"), "{msg}");
        assert!(msg.contains("commit an initial state"), "{msg}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_inside_a_worktree() {
        let repo = Repo::new("start-wt-escape");
        repo.start_project("foo");
        let wt_apg = repo.project_apg_root("foo");
        let err = project_start_at(&wt_apg, "bar").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("one project at a time"), "{msg}");
        assert!(msg.contains("main checkout"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_dirty_main() {
        let repo = Repo::new("start-dirty");
        repo.write("junk.txt", "untracked junk");
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("dirty"), "{msg}");
        assert!(msg.contains("git status"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_invalid_names_never_sanitized() {
        let repo = Repo::new("start-names");
        for bad in ["", "a/b", "bad name", "bad~name", "..", "HEAD", ".lock"] {
            let err = project_start_at(&repo.apg_root(), bad).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("refused") && (msg.contains("not a valid") || msg.contains("empty")),
                "name `{bad}` must refuse with a validity message: {msg}"
            );
        }
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_branch_without_worktree_collision() {
        let repo = Repo::new("start-collision-branch");
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        main_repo.branch("foo", &head, false).unwrap();
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("no worktree hosts it"), "{msg}");
        assert!(msg.contains("git branch -D foo"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_branch_checked_out_in_main_checkout() {
        // Bootstrap-style: the branch is the main checkout's HEAD.
        let repo = Repo::new("start-collision-head");
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        main_repo.branch("foo", &head, false).unwrap();
        main_repo.set_head("refs/heads/foo").unwrap();
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("checked out in the main checkout"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_branch_checked_out_in_another_worktree() {
        let repo = Repo::new("start-collision-wt");
        repo.start_project("foo");
        // From the main checkout, starting `foo` again: checked out in the
        // project's worktree.
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("checked out in worktree"), "{msg}");
        assert!(
            msg.contains(
                repo.project_worktree_dir("foo")
                    .display()
                    .to_string()
                    .as_str()
            ),
            "{msg}"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_refuses_dir_that_is_not_a_worktree() {
        let repo = Repo::new("start-collision-dir");
        // A plain directory at the worktree location (no branch, no worktree).
        std::fs::create_dir_all(repo.project_worktree_dir("foo")).unwrap();
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a project worktree"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_self_heals_missing_worktrees_gitignore_entry() {
        // A repo whose .gitignore dropped the worktrees entry (or was
        // cloned before it existed): start must scaffold it + commit the
        // scaffold (the main checkout stays clean), then proceed.
        let repo = Repo::new("start-selfheal");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.write(".gitignore", "apg/.trans/\n");
        repo.commit_all("drop worktrees ignore");
        // start seeds from main's scan, so main needs a fresh scan.
        start_scan(&repo.root).unwrap();
        let wt = project_start_at(&repo.apg_root(), "foo").unwrap();
        assert!(wt.is_dir());
        // The entry is back in the main checkout's .gitignore — committed,
        // so the main checkout is clean again (a later start would refuse a
        // dirty main).
        let ignore = std::fs::read_to_string(repo.root.join(".gitignore")).unwrap();
        assert!(ignore.contains("apg/.worktrees/"), "{ignore}");
        assert!(repo.is_clean(), "self-heal must commit the scaffold");
        // The worktree (checked out from the scaffolded HEAD) carries it too.
        let wt_ignore = std::fs::read_to_string(wt.join(".gitignore")).unwrap();
        assert!(wt_ignore.contains("apg/.worktrees/"), "{wt_ignore}");
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // R10 version gate on start (task-3/task-5): blocks — never warns —
    // on missing version and on major/minor mismatch in either direction,
    // with upgrade guidance; a patch diff proceeds.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_blocks_unversioned_layout_with_init_guidance() {
        let repo = Repo::new("start-gate-unversioned");
        set_layout_version(&repo, None);
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        for needle in [
            "no layout version",
            "apg init",
            "re-run `apg project start foo`",
            "apg-upgrade.md",
        ] {
            assert!(msg.contains(needle), "{msg}");
        }
        // Nothing was created.
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_blocks_older_layout_with_upgrade_guidance() {
        let repo = Repo::new("start-gate-older");
        set_layout_version(&repo, Some(&older_minor_version()));
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("predates"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        assert!(msg.contains("apg-upgrade.md"), "{msg}");
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_blocks_newer_layout_with_upgrade_guidance() {
        let repo = Repo::new("start-gate-newer");
        set_layout_version(&repo, Some(&newer_minor_version()));
        let err = project_start_at(&repo.apg_root(), "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("NEWER apg"), "{msg}");
        assert!(msg.contains("upgrade apg"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        assert!(msg.contains("apg-upgrade.md"), "{msg}");
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn start_proceeds_on_patch_diff_layout() {
        let repo = Repo::new("start-gate-patch");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        set_layout_version(&repo, Some(&patch_shifted_version()));
        start_scan(&repo.root).unwrap();
        let wt = project_start_at(&repo.apg_root(), "foo").unwrap();
        assert!(wt.is_dir(), "same major.minor must proceed");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn merge_round_trip_start_mutate_verify_merge_rebuild() {
        // Fold every scan (main pre-scan + the realize scan + the merge
        // rebuild) into ONE CWD_LOCK hold. Each scan would otherwise
        // re-queue on the process-wide lock behind every other scanning e2e
        // test, and that re-queueing — not the scan itself — is what pushed
        // this test past libtest's 60s warning.
        let _guard = testutil::CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fn start_scan_locked(dir: &Path) -> anyhow::Result<()> {
            testutil::scan_checkout_locked(dir)
        }
        let repo = Repo::new("merge-e2e");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");

        // Main must carry a fresh scan: start seeds the branch from it.
        start_scan_locked(&repo.root).unwrap();

        // start -> worktree + branch + branch DB (payload has only Store).
        let wt = project_start_at(&repo.apg_root(), "foo").unwrap();
        let wt_apg = wt.join(specs::LAYOUT);

        // mutate: author the durable spec THROUGH A LIVE SESSION — the
        // mandatory-session path a real caller uses (`apg node add` is admitted
        // into the write-back buffer, `apg session save` makes it durable in
        // one commit, `apg session end` releases) — then author the transient
        // plan directly (`.trans` runs regardless of a live session). This pins
        // that the live-session guard does NOT block a legitimate merge once
        // the session has saved and ended.
        let home = repo.root.join("home");
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
            "timer",
            "--body",
            "A workitem can be started",
            "--property",
            "id=R1",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "node add through the session: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        // The add is BUFFERED: no node file and no commit until save.
        assert!(
            !layers::node_file_path(&wt_apg, layers::Layer::Requirements, "requirement", "timer")
                .exists(),
            "a buffered add must not write a node file before save"
        );

        // The single durability point: `apg session save` flushes the buffer
        // (one atomic node-file write + one commit); `apg session end`
        // releases the session.
        let save = testutil::spawn_apg(&["session", "save"], &wt);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "session end: {}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "session process: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "session end must release the session"
        );

        let plan_path = wt_apg.join(specs::TRANS).join("plans").join("foo.jsonl");
        artifacts::write_jsonl_and_reingest(&wt_apg, &plan_path, "foo", &plan_records(true))
            .unwrap();
        // The session's save commit holds the spec node file; the plan mutation
        // committed nothing (R8 — .trans never commits).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let foo_tip = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            foo_tip
                .tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/requirement/timer.json"))
                .is_ok(),
            "spec node file must be committed on the project branch"
        );
        assert!(
            foo_tip
                .tree()
                .unwrap()
                .get_path(Path::new("apg/.trans/plans/foo.jsonl"))
                .is_err(),
            "plan JSONL must never be committed (.trans is transient)"
        );

        // verify rejects: unrealized planned node (dangling — no code at its
        // FQN yet) AND unresolved feedback, in one refusal listing both.
        let err = plan_cmd::plan_verify_at(&wt_apg, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Widget"), "unrealized planned node: {msg}");
        assert!(msg.contains("unresolved review feedback"), "{msg}");

        // resolve the feedback (writer action + reviewer resolve, via the
        // funnel on the transient plan).
        let mut recs = plan_records(false);
        recs.push(Record::Feedback {
            fqn: "foo/feedback-1".into(),
            body: "unresolved".into(),
            status: "resolved".into(),
            disposition: "fixed".into(),
        });
        recs.push(Record::Reviews {
            from: "foo/feedback-1".into(),
            to: "foo/plan.phase-01.task-1".into(),
        });
        artifacts::write_jsonl_and_reingest(&wt_apg, &plan_path, "foo", &recs).unwrap();

        // realize the planned node: the implementer's code lands on the
        // branch (payload gains Widget), is committed, and the branch DB is
        // rebuilt with a scan.
        let payload = testutil::code_payload(MOD, FILE, &["Store", "Widget"]);
        std::fs::write(wt.join("code/seed.scan.jsonl"), payload).unwrap();
        wt_commit(&wt, &["code/seed.scan.jsonl"], "implement Widget");
        start_scan_locked(&wt).unwrap();
        let foo_tip = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();

        // verify passes: planned node realized, all feedback resolved.
        plan_cmd::plan_verify_at(&wt_apg, "foo").unwrap();

        // merge from the main checkout: verify gate -> ff merge -> main
        // rebuild (the rebuild is a plain unguarded scan on main).
        project_merge_at(&repo.apg_root(), "foo", Some(&start_scan_locked)).unwrap();

        // The default branch now holds the project's tip.
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let main_tip = main_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            main_tip.id(),
            foo_tip,
            "main must fast-forward to the project tip"
        );
        // The main checkout carries the merged content (the spec node file).
        assert!(
            repo.root
                .join("apg/layers/requirements/requirement/timer.json")
                .exists()
        );
        assert!(repo.is_clean(), "merged main must be clean");

        // Main rebuild: the main DB has the code (Store + Widget) and the
        // merged spec; the transient plan did not cross the merge.
        let main_apg = repo.apg_root();
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        assert!(db.has_node("requirements.requirement.timer"));
        assert!(db.has_node(format!("{MOD_FQN}.Store").as_str()));
        assert!(db.has_node(format!("{MOD_FQN}.Widget").as_str()));
        assert!(!db.has_node("foo/plan"), "transient plans never reach main");
        drop(db);
        // The main DB is fresh (scan_meta re-anchored by the rebuild scan).
        assert!(!git::is_stale(&main_apg));
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // phase-5 task-4 (e2e): the FULL dogfood round trip — suite-tool
    // lookups and mutations with cwd inside the worktree (SPEC §6: walk-up
    // discovery finds the worktree's own apg/ + branch DB, even though the
    // worktree lives INSIDE the main checkout's apg/), verify, merge, main
    // rebuilds unguarded. The main checkout's apg/ is untouched by every
    // in-worktree operation.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn full_dogfood_round_trip_suite_tool_ops_inside_the_worktree() {
        // Fold the scans (main pre-scan, merge rebuild) into ONE CWD_LOCK
        // hold. Each scan would otherwise
        // re-queue on the process-wide lock behind every other scanning e2e
        // test, and that re-queueing — not the scan itself — is what pushed
        // this test past libtest's 60s warning.
        let _guard = testutil::CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fn start_scan_locked(dir: &Path) -> anyhow::Result<()> {
            testutil::scan_checkout_locked(dir)
        }
        // A main checkout whose scanned code carries the structs the solution
        // tier's implemented-by edges claim (resolves -> real), plus one
        // function so the branch-DB lookups assert the
        // module/struct/function triple from the scanned payload.
        let repo = Repo::new("dogfood-full-e2e");
        let mut payload = testutil::code_payload(
            MOD,
            FILE,
            &[
                "Store",
                "ProjectStart",
                "MutationGuard",
                "LayersSerializer",
                "PlanBridge",
                "InitVersionGate",
            ],
        );
        payload.push_str(&testutil::function_line("n7", MOD, "Lookup", FILE));
        repo.write("code/seed.scan.jsonl", &payload);
        repo.commit_all("seed code");

        // A main-checkout scan first: the "untouched" assertions compare
        // against a real main DB (its db.lbug + graph.jsonl must not move
        // during in-worktree operation).
        start_scan_locked(&repo.root).unwrap();
        let main_apg = repo.apg_root();
        let main_db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        assert!(main_db.has_node(format!("{MOD_FQN}.Store").as_str()));
        assert!(main_db.has_node(format!("{MOD_FQN}.Lookup").as_str()));
        drop(main_db);
        let main_db_bytes = std::fs::read(main_apg.join(specs::TRANS).join("db.lbug")).unwrap();
        let main_graph_bytes =
            std::fs::read(main_apg.join(specs::TRANS).join("graph.jsonl")).unwrap();
        let main_tip = repo.head_sha();

        // 1. start from main: one command yields worktree + branch + branch
        // DB. The worktree lives INSIDE main's apg/ (at
        // <main>/apg/.worktrees/round-trip), so walk-up discovery has a real
        // choice to make — the worktree's own apg/ vs main's apg/ above it.
        let wt = project_start_at(&repo.apg_root(), "round-trip").unwrap();
        let wt_apg = wt.join(specs::LAYOUT);
        assert!(wt.is_dir());
        assert!(wt_apg.join(specs::TRANS).join("db.lbug").exists());
        let id_wt = git::repo_identity(&wt_apg).unwrap();
        assert!(id_wt.is_worktree);
        assert_eq!(id_wt.branch.as_deref(), Some("round-trip"));
        let id_main = git::repo_identity(&main_apg).unwrap();
        assert!(!id_main.is_worktree);
        assert_eq!(id_main.branch.as_deref(), Some("main"));

        // 2. operate in-worktree: the suite tools shell out to `apg` with cwd
        // inside the worktree, and the binary resolves the layout root by
        // walking up from current_dir. Simulated here by passing a deep
        // in-worktree cwd to the same walk-up (tests never mutate the
        // process-global cwd outside the scan lock).
        let deep_cwd = wt.join("code").join("deep");
        std::fs::create_dir_all(&deep_cwd).unwrap();
        let resolved = specs::find_apg_root(&deep_cwd)
            .expect("walk-up from an in-worktree cwd must find a layout root");
        assert_eq!(
            resolved, wt_apg,
            "walk-up must find the worktree's OWN apg/, not the main checkout's (its parent)"
        );
        assert_ne!(resolved, main_apg);
        // Negative control: from a cwd deep inside MAIN, the same walk-up
        // finds main's apg/ — the discovery is checkout-local, not global.
        let main_deep = repo.root.join("code").join("deep");
        std::fs::create_dir_all(&main_deep).unwrap();
        assert_eq!(specs::find_apg_root(&main_deep), Some(main_apg.clone()));

        // 2a. lookups — the `apg query`-equivalent: open the branch DB found
        // by walk-up and query it; the module/struct/function triple from the
        // scanned payload is there, and no authored tiers yet (the branch DB
        // is the fresh start-scan).
        let db = artifacts::ArtifactDb::open(&resolved).unwrap();
        assert!(db.has_node(MOD_FQN), "module from the scanned payload");
        assert!(
            db.has_node(format!("{MOD_FQN}.Store").as_str()),
            "struct from the scanned payload"
        );
        assert!(
            db.has_node(format!("{MOD_FQN}.Lookup").as_str()),
            "function from the scanned payload"
        );
        assert!(!db.has_node("requirements.requirement.r1"));
        let count = |q: &str| -> i64 {
            db.q(q)
                .unwrap()
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0)
        };
        assert_eq!(count("MATCH (n:Function) RETURN count(*)"), 1);
        drop(db);

        // 2b. mutations — the `apg node add`-equivalent (the exact node_cmd
        // shape through layers::write_project) against the walk-up root:
        // membership guard -> validate -> atomic write -> auto-commit -> DB
        // re-merge, and a fresh query sees the new node.
        layers::write_project(
            &resolved,
            &[nf(
                "requirements",
                "note",
                "dogfood-log",
                "The task-4 dogfood node: authored through the apg node add-equivalent surface with cwd inside the worktree.",
                &[("kind", "background")],
            )],
            &[],
        )
        .unwrap();
        let node_file = wt_apg
            .join(layers::LAYERS_DIR)
            .join("requirements")
            .join("note")
            .join("dogfood-log.json");
        assert!(node_file.exists(), "{} must exist", node_file.display());
        let db = artifacts::ArtifactDb::open(&resolved).unwrap();
        assert!(
            db.has_node("requirements.note.dogfood-log"),
            "the mutation's DB re-merge must make the new node visible to a fresh query"
        );
        drop(db);
        // The node file auto-committed on the project branch (R8).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        assert!(
            wt_repo
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/note/dogfood-log.json"))
                .is_ok(),
            "the node file must be committed on the project branch"
        );

        // 2c. author the re-materialized dogfood tiers (task-1 builder reused)
        // + the transient plan (task-1 builder, project name parametrized).
        layers::write_project(&resolved, &apg_projects_tier_nodes(), &[]).unwrap();
        let plan_path = resolved
            .join(specs::TRANS)
            .join("plans")
            .join("round-trip.jsonl");
        let tip_before_plan = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
        artifacts::write_jsonl_and_reingest(
            &resolved,
            &plan_path,
            "round-trip",
            &apg_projects_plan_records("round-trip"),
        )
        .unwrap();
        // The plan mutation commits nothing (.trans is transient — R8): the
        // branch tip is unchanged and the plan JSONL never enters a commit.
        assert!(plan_path.exists(), "the plan JSONL lands under .trans");
        let wt_tip = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            wt_tip.id(),
            tip_before_plan,
            "plan mutations never commit on the project branch"
        );
        assert!(
            wt_tip
                .tree()
                .unwrap()
                .get_path(Path::new("apg/.trans/plans/round-trip.jsonl"))
                .is_err(),
            "the plan JSONL must never be committed"
        );

        // 2d. the MAIN checkout's apg/ is untouched by all of it: its db.lbug
        // and graph.jsonl are byte-identical, no apg/layers was created there,
        // and main's branch never moved.
        assert_eq!(
            std::fs::read(main_apg.join(specs::TRANS).join("db.lbug")).unwrap(),
            main_db_bytes,
            "main's db.lbug must be unchanged by the in-worktree operations"
        );
        assert_eq!(
            std::fs::read(main_apg.join(specs::TRANS).join("graph.jsonl")).unwrap(),
            main_graph_bytes,
            "main's graph.jsonl must be unchanged by the in-worktree operations"
        );
        assert!(
            !main_apg.join("layers").exists(),
            "no apg/layers may be created under the main checkout"
        );
        assert_eq!(repo.head_sha(), main_tip, "main's branch must not move");

        // 3. the branch DB already holds code + tiers + plan together: the
        // copied main scan supplied the code, and the step 2b/2c mutations
        // merged the durable tiers and the transient plan into the live DB
        // (`write_project`'s `ingest_tree` and `write_jsonl_and_reingest`'s
        // `merge_records`). Asserting here — without a redundant full
        // worktree rebuild — is the speedup; the merge rebuild below still
        // exercises a full scan of code + tiers.
        let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        for f in expected_tier_fqns() {
            assert!(db.has_node(&f), "branch DB must hold tier node `{f}`");
        }
        assert!(db.has_node("requirements.note.dogfood-log"));
        for f in [
            "round-trip/plan",
            "round-trip/plan.phase-01",
            "round-trip/plan.phase-01.task-1",
            "round-trip/plan.phase-01.task-5",
        ] {
            assert!(
                db.has_node(f),
                "branch DB must hold transient plan node `{f}`"
            );
        }
        // The spine is real in the branch DB: 20 drives edges, the
        // domain→solution realised-by hop, the 5 implemented-by claims onto
        // scanned structs, and the User→Requirement contains tree.
        let count = |q: &str| -> i64 {
            db.q(q)
                .unwrap()
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0)
        };
        assert_eq!(
            count("MATCH (:Requirement)-[:Drives]->(:DomainGroup) RETURN count(*)"),
            20
        );
        assert_eq!(
            count("MATCH (:DomainGroup)-[:RealisedBy]->(:System) RETURN count(*)"),
            1
        );
        assert_eq!(
            count("MATCH (:Container)-[:SpecImplementedBy]->(:Struct) RETURN count(*)"),
            5
        );
        assert_eq!(
            count("MATCH (:User)-[:Contains]->(:Requirement) RETURN count(*)"),
            20
        );
        assert_eq!(
            count("MATCH (:Note)-[:Details]->(:Requirement) RETURN count(*)"),
            1
        );
        drop(db);
        assert_eq!(
            std::fs::read(main_apg.join(specs::TRANS).join("db.lbug")).unwrap(),
            main_db_bytes,
            "in-worktree operations must not touch main's DB"
        );

        // 4. verify: the coherence gate passes green — no planned nodes, no
        // feedback, and derived solution coverage holds.
        plan_cmd::plan_verify_at(&wt_apg, "round-trip").unwrap();

        // 5. merge from the main checkout: verify gate -> fast-forward -> main
        // rebuilds unguarded (a plain scan of the main checkout).
        project_merge_at(&repo.apg_root(), "round-trip", Some(&start_scan_locked)).unwrap();

        // The default branch holds the project tip; the merged main checkout
        // carries the node files (the tiers + the dogfood node).
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert_eq!(
            main_repo.head().unwrap().peel_to_commit().unwrap().id(),
            wt_tip.id(),
            "main must fast-forward to the project tip"
        );
        assert!(
            repo.root
                .join("apg/layers/solution/container/project-commands.json")
                .exists(),
            "the merged main checkout carries the tier node files"
        );
        assert!(
            repo.root
                .join("apg/layers/requirements/note/dogfood-log.json")
                .exists(),
            "the merged main checkout carries the dogfood node"
        );
        assert!(repo.is_clean(), "merged main must be clean");

        // Main rebuild: the main DB has the code + the merged tiers, does NOT
        // hold the transient plan, and its scan_meta is fresh (not stale).
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        for f in [
            "requirements.requirement.r1",
            "requirements.requirement.r20",
            "requirements.note.dogfood-log",
            "domain.group.change-sets",
            "solution.system.apg-cli",
            "solution.container.project-commands",
            format!("{MOD_FQN}.Store").as_str(),
            format!("{MOD_FQN}.Lookup").as_str(),
            format!("{MOD_FQN}.ProjectStart").as_str(),
        ] {
            assert!(db.has_node(f), "main DB must hold `{f}` after the rebuild");
        }
        assert!(
            !db.has_node("round-trip/plan"),
            "transient plans never reach main"
        );
        drop(db);
        assert!(!git::is_stale(&main_apg), "main's scan_meta must be fresh");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn merge_keeps_worktree_and_branch_cleanup_deletes_no_branch() {
        // merge-self-cleanup: `apg project merge <name>` cleans up after
        // itself. AC (a): a full verify → merge → main rebuild round-trip
        // removes the merged project's worktree at
        // `<main>/apg/.worktrees/<name>` and deletes its branch
        // `refs/heads/<name>`; AC (b): a refused/failed merge leaves both
        // untouched; AC (c): the default branch and the main checkout are
        // preserved (never-touch-default-branch).
        //
        // Fold the scans (main pre-scan + the merge rebuild) into
        // ONE CWD_LOCK hold. Each scan would otherwise re-queue on the
        // process-wide lock behind every other scanning e2e test, and that
        // re-queueing — not the scan itself — is what pushed this test past
        // libtest's 60s warning.
        let _guard = testutil::CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fn start_scan_locked(dir: &Path) -> anyhow::Result<()> {
            testutil::scan_checkout_locked(dir)
        }
        let repo = Repo::new("self-cleanup");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");

        // Main must carry a fresh scan: both starts seed from it.
        start_scan_locked(&repo.root).unwrap();

        // Refusal path first (AC-b): a merge of a project whose verify gate
        // fails — a planned node that the branch scan never realized — must
        // refuse before anything is merged, leaving the worktree, the branch,
        // and the main checkout untouched.
        let wt = project_start_at(&repo.apg_root(), "fail").unwrap();
        let wt_apg = wt.join(specs::LAYOUT);
        // A transient plan carrying an unrealized planned node makes the
        // verify gate refuse (`fixture.mod.Widget` never becomes real code —
        // the fixture payload only has `Store`).
        let plan_path = wt_apg.join(specs::TRANS).join("plans").join("fail.jsonl");
        artifacts::write_jsonl_and_reingest(
            &wt_apg,
            &plan_path,
            "fail",
            &[
                Record::Plan {
                    fqn: "fail/plan".to_string(),
                    title: "fail plan".to_string(),
                    strategy: String::new(),
                },
                Record::PlanPhase {
                    fqn: "fail/plan.phase-01".to_string(),
                    number: 1,
                    title: "P1".to_string(),
                    deliverable: "D".to_string(),
                    status: "pending".to_string(),
                },
                Record::Contains {
                    from: "fail/plan".to_string(),
                    to: "fail/plan.phase-01".to_string(),
                    properties: layers::NodeProperties::default(),
                },
                Record::PlannedNode {
                    fqn: format!("{MOD_FQN}.Widget"),
                    kind: "struct".into(),
                    name: "Widget".into(),
                    parent: MOD.into(),
                },
            ],
        )
        .unwrap();
        // No rebuild: `write_jsonl_and_reingest` already projected the plan
        // (including the planned node) into the live branch DB, so it is
        // fresh and complete.
        let err = plan_cmd::plan_verify_at(&wt_apg, "fail").unwrap_err();
        assert!(
            format!("{err:#}").contains("not realized"),
            "verify must report the unrealized planned node: {err:#}"
        );
        let main_tip_before_refusal = repo.head_sha();
        let err = project_merge_at(&repo.apg_root(), "fail", Some(&start_scan_locked)).unwrap_err();
        assert!(format!("{err:#}").contains("not realized"), "{err:#}");
        assert_eq!(
            repo.head_sha(),
            main_tip_before_refusal,
            "a refused merge must not move the main checkout"
        );
        assert!(
            wt.is_dir(),
            "a refused merge must leave the worktree at apg/.worktrees/fail in place"
        );
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert!(
            main_repo.find_worktree("fail").is_ok(),
            "a refused merge must leave the worktree registered"
        );
        assert!(
            main_repo
                .find_branch("fail", git2::BranchType::Local)
                .is_ok(),
            "a refused merge must leave the project branch in place"
        );

        // Success path (AC-a): a project with durable content + a minimal
        // plan (no planned nodes, no feedback) passes the verify gate, merges,
        // rebuilds main, and then cleans up after itself.
        let wt = project_start_at(&repo.apg_root(), "test").unwrap();
        let wt_apg = wt.join(specs::LAYOUT);
        write_spec_node(&wt_apg);
        let plan_path = wt_apg.join(specs::TRANS).join("plans").join("test.jsonl");
        artifacts::write_jsonl_and_reingest(
            &wt_apg,
            &plan_path,
            "test",
            &[
                Record::Plan {
                    fqn: "test/plan".to_string(),
                    title: "test plan".to_string(),
                    strategy: String::new(),
                },
                Record::PlanPhase {
                    fqn: "test/plan.phase-01".to_string(),
                    number: 1,
                    title: "P1".to_string(),
                    deliverable: "D".to_string(),
                    status: "pending".to_string(),
                },
                Record::Contains {
                    from: "test/plan".to_string(),
                    to: "test/plan.phase-01".to_string(),
                    properties: layers::NodeProperties::default(),
                },
                Record::Satisfies {
                    from: "test/plan.phase-01".to_string(),
                    to: "requirements.requirement.timer".to_string(),
                },
            ],
        )
        .unwrap();
        // No rebuild: `write_spec_node` (durable tier) and
        // `write_jsonl_and_reingest` (plan) already projected their records
        // into the live branch DB and re-anchored scan_meta, so it is fresh
        // and complete.
        plan_cmd::plan_verify_at(&wt_apg, "test").unwrap();

        let tip_before_merge = main_repo.head().unwrap().peel_to_commit().unwrap().id();
        project_merge_at(&repo.apg_root(), "test", Some(&start_scan_locked)).unwrap();

        // The merge landed main at the project tip (fast-forward), the main
        // rebuild is fresh, and the merged project cleaned up after itself.
        assert_ne!(
            main_repo.head().unwrap().peel_to_commit().unwrap().id(),
            tip_before_merge,
            "the merge must advance the default branch to the project tip"
        );
        assert!(
            !wt.exists(),
            "the merged project's worktree apg/.worktrees/test must be removed (AC-a)"
        );
        assert!(
            main_repo.find_worktree("test").is_err(),
            "the merged project's worktree must be unregistered (AC-a)"
        );
        assert!(
            main_repo
                .find_branch("test", git2::BranchType::Local)
                .is_err(),
            "the merged project's branch refs/heads/test must be deleted (AC-a)"
        );
        assert!(
            !git::is_stale(&repo.apg_root()),
            "main's scan_meta must be fresh"
        );
        assert!(
            repo.is_clean(),
            "after merge self-cleanup the main checkout must be clean"
        );

        // AC (c): the default branch and the main checkout are preserved.
        assert!(
            main_repo
                .find_branch("main", git2::BranchType::Local)
                .is_ok(),
            "the default branch must survive merge self-cleanup"
        );
        assert!(repo.root.is_dir(), "the main checkout must survive");
        assert!(
            main_repo.head().unwrap().shorthand() == Some("main"),
            "the main checkout must stay on the default branch"
        );
        assert!(
            repo.root
                .join("apg/layers/requirements/requirement/timer.json")
                .exists(),
            "the merged main checkout carries the merged tier node file"
        );
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // phase-02: `apg project delete <name>` — the abandon path. Every
    // refusal names the actual state + one fix command; success removes the
    // worktree + deletes the branch (commits discarded); never the default
    // branch or the main checkout.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn delete_refuses_invalid_name_and_default_branch() {
        // delete-refuses-unsafe AC-(a) + never-touch-default-branch: an
        // invalid project name and `<name>` equal to the default branch are
        // hard refusals naming the actual state + one fix command.
        let repo = Repo::new("del-invalid");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");

        let err = project_delete_at(&repo.apg_root(), "bad/name").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a valid project name"), "{msg}");
        assert!(msg.contains("apg project start"), "{msg}");

        let err = project_delete_at(&repo.apg_root(), "main").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("default branch"), "{msg}");
        assert!(msg.contains("never deleted"), "{msg}");

        // Nothing was removed.
        assert!(repo.is_clean(), "refusals must not dirty the main checkout");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn delete_refuses_missing_and_mismatched_projects() {
        // delete-refuses-unsafe AC-(c) and AC-(d): a project that does not
        // exist (no branch), and a leftover branch / mismatched-worktree
        // state, are refused with a manual fix — never guessed.
        let repo = Repo::new("del-missing");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");

        // AC-(c): no branch, no worktree at the fixed location.
        let err = project_delete_at(&repo.apg_root(), "ghost").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("branch `ghost` does not exist"), "{msg}");
        assert!(msg.contains("apg project start ghost"), "{msg}");

        // AC-(d): branch exists but no worktree dir at the fixed location
        // (leftover branch — the state refuse_start already reports).
        let main_repo = git2_repo_test(&repo);
        main_repo
            .branch(
                "leftover",
                &main_repo.head().unwrap().peel_to_commit().unwrap(),
                false,
            )
            .unwrap();
        let err = project_delete_at(&repo.apg_root(), "leftover").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("expected worktree"), "{msg}");
        assert!(msg.contains("apg project start leftover"), "{msg}");
        assert!(
            main_repo
                .find_branch("leftover", git2::BranchType::Local)
                .is_ok(),
            "a refused delete must leave the leftover branch in place"
        );

        // AC-(d): worktree dir exists but hosts a DIFFERENT branch (a
        // mismatched-worktree state). Start project `first`, then check out a
        // different branch inside its worktree so the worktree at the fixed
        // location for `first` holds `injected` instead.
        let wt_first = repo.start_project("first");
        let wt_repo = git2::Repository::open(&wt_first).unwrap();
        wt_repo
            .branch(
                "injected",
                &wt_repo.head().unwrap().peel_to_commit().unwrap(),
                false,
            )
            .unwrap();
        wt_repo.set_head("refs/heads/injected").unwrap();
        wt_repo
            .checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let err = project_delete_at(&repo.apg_root(), "first").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not branch `first`'s worktree"),
            "the mismatched-worktree refusal must name the actual state: {msg}"
        );
        assert!(msg.contains("never guessed"), "{msg}");
        assert_project_present(&repo, "first");
        assert!(
            repo.is_clean(),
            "the refusal must leave the main checkout clean"
        );
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn delete_refuses_dirty_worktree_naming_commit_or_stash() {
        // delete-refuses-unsafe AC-(e): a project worktree with tracked
        // uncommitted changes is refused (commit/stash first).
        let repo = Repo::new("del-dirty");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        let wt = repo.start_project("dirty");
        // A tracked uncommitted change: modify a committed file, do not commit.
        let p = wt.join("code/seed.scan.jsonl");
        let cur = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("{cur}\n// dirty\n")).unwrap();

        let err = project_delete_at(&repo.apg_root(), "dirty").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("tracked uncommitted changes"), "{msg}");
        assert!(msg.contains("commit or stash"), "{msg}");
        assert_project_present(&repo, "dirty");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn delete_abandons_an_unmerged_project() {
        // delete-subcommand AC-(b)/(c): delete is the abandon path — it
        // removes the worktree at <main>/apg/.worktrees/<name> and deletes
        // the branch, discarding the branch's unmerged commits; the default
        // branch and the main checkout are untouched.
        let repo = Repo::new("del-success");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        let wt = repo.start_project("abandon");
        // Seed the worktree's branch graph (a started project has a DB).
        testutil::scan_checkout(&wt).unwrap();
        // Make the project's branch DIVERGE from default (unmerged by design):
        // a committed project change that delete will discard.
        wt_append_commit(&wt, "code/extra.txt", "unmerged work\n", "project work");
        let main_tip_before = repo.head_sha();
        let main_repo = git2_repo_test(&repo);

        project_delete_at(&repo.apg_root(), "abandon").unwrap();

        // The project is gone: worktree unregistered + dir removed, branch
        // deleted (its unmerged commits discarded).
        assert_project_gone(&repo, "abandon");
        assert!(!wt.exists(), "the abandoned worktree dir must be removed");
        // The default branch and the main checkout survive untouched.
        assert_eq!(repo.head_sha(), main_tip_before, "main must not move");
        assert!(
            main_repo
                .find_branch("main", git2::BranchType::Local)
                .is_ok(),
            "the default branch must survive delete"
        );
        assert!(
            main_repo.head().unwrap().shorthand() == Some("main"),
            "the main checkout must stay on the default branch"
        );
        assert!(repo.is_clean(), "the main checkout must stay clean");
        testutil::remove(&repo);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (worktree/branch/merge/rebuild); run via cargo test-e2e"]
    fn delete_refuses_when_branch_is_main_checkouts_current() {
        // delete-refuses-unsafe AC-(b): a branch that is the main checkout's
        // current branch is refused (nothing to delete from here) — the
        // branch-without-worktree bootstrap state. origin/HEAD pins the
        // default to `main` while the main checkout holds `current` (like a
        // real remote-backed repo), so the refusal is the current-branch
        // refusal, not the default-branch one.
        let repo = Repo::new("del-current");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        let main_repo = git2_repo_test(&repo);
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        main_repo
            .reference("refs/remotes/origin/main", head.id(), true, "origin main")
            .unwrap();
        main_repo
            .reference_symbolic(
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
                true,
                "origin head",
            )
            .unwrap();
        main_repo.branch("current", &head, false).unwrap();
        main_repo.set_head("refs/heads/current").unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        main_repo.checkout_head(Some(&mut checkout)).unwrap();

        let err = project_delete_at(&repo.apg_root(), "current").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("main checkout's current branch"), "{msg}");
        assert!(msg.contains("git checkout main"), "{msg}");
        assert!(
            main_repo
                .find_branch("current", git2::BranchType::Local)
                .is_ok(),
            "a refused delete must leave the current branch in place"
        );
        testutil::remove(&repo);
    }

    /// spec-splice-guard phase-03 task-1 (e2e): a SPEC-ONLY `apg project
    /// merge` — no code change at all — must land the merged authored
    /// node/edge in main's `db.lbug` (the query index), not merely in the
    /// `graph.jsonl` export it publishes beside it.
    ///
    /// The defect path: `project start` seeds the branch DB from main's scan;
    /// the branch then authors a durable requirement + value + `drives` edge
    /// through a LIVE session (the mandatory-session path) and saves — a
    /// SPEC-ONLY change, with NO worktree scan, so the branch DB still matches
    /// main's seed content identity. Merging fast-forwards main to the branch;
    /// its rebuild takes the win-C splice fast path, whose `apply` writes no
    /// authored/transient row (it applies only the code delta) while
    /// `publish` serializes `graph.jsonl` from the FULL assembled graph. Main's
    /// DB therefore silently loses the merged spec, and the splice stamps the
    /// merged HEAD so a later `apg scan` fast-paths as fresh and never corrects
    /// it.
    ///
    /// The phase-01 authored-identity guard makes the seed ineligible (the
    /// assembled graph's authored rows are not represented in the seed DB), so
    /// the correctness-reference FULL LOAD runs and main's DB and export agree.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/live session/db.lbug); run via cargo test-e2e"]
    fn merge_spec_only_lands_authored_rows_in_main_db() {
        let req_fqn = "requirements.requirement.timer";
        let val_fqn = "domain.value.tick";

        // A real scratch git repo with Go sources + a versioned `apg/` layout,
        // driven by the REAL candidate binary (the frontends resolve relative
        // to the spawned binary, so the whole pipeline is genuinely exercised).
        let (base, repo_dir, home) = warm_scratch("spec-only-merge");
        let repo_apg = repo_dir.join(specs::LAYOUT);

        // Main must carry a fresh scan: `project start` seeds the branch DB
        // from it, AND the full scan records the SHARED content-addressed store
        // that the merge rebuild's splice seed is validated against.
        let scan = warm_run(&repo_dir, &home, &["scan", "."]);
        assert!(
            scan.status.success(),
            "main scan: {}",
            String::from_utf8_lossy(&scan.stderr)
        );

        // start -> worktree + branch + branch DB copied from main's scan.
        let start = warm_run(&repo_dir, &home, &["project", "start", "spec-only"]);
        assert!(
            start.status.success(),
            "project start: {}",
            String::from_utf8_lossy(&start.stderr)
        );
        let wt = repo_dir
            .join(specs::LAYOUT)
            .join(".worktrees")
            .join("spec-only");
        let wt_apg = wt.join(specs::LAYOUT);
        assert!(
            wt_apg.join(specs::TRANS).join("db.lbug").exists(),
            "start must seed the branch db.lbug"
        );

        // Author a durable authored node + edge THROUGH A LIVE SESSION — the
        // mandatory-session surface a real caller uses. Each admission is
        // projected into the live branch DB; `session save` is the single
        // durability point (one atomic node-file write + one commit) and
        // `session end` releases. SPEC-ONLY: no code edit and NO worktree scan
        // follows, so the branch DB stays at main's seed content identity (the
        // eligibility the pre-fix splice needs).
        let session = testutil::start_session_process(&wt, &home);
        let add_req = warm_run(
            &wt,
            &home,
            &[
                "node",
                "add",
                "requirements",
                "requirement",
                "timer",
                "--body",
                "A workitem can be started",
                "--property",
                "id=R1",
            ],
        );
        assert!(
            add_req.status.success(),
            "node add requirement: {}",
            String::from_utf8_lossy(&add_req.stderr)
        );
        let add_val = warm_run(
            &wt,
            &home,
            &[
                "node", "add", "domain", "value", "tick", "--body", "One tick",
            ],
        );
        assert!(
            add_val.status.success(),
            "node add value: {}",
            String::from_utf8_lossy(&add_val.stderr)
        );
        let add_edge = warm_run(&wt, &home, &["edge", "add", "drives", req_fqn, val_fqn]);
        assert!(
            add_edge.status.success(),
            "edge add: {}",
            String::from_utf8_lossy(&add_edge.stderr)
        );

        let save = warm_run(&wt, &home, &["session", "save"]);
        assert!(
            save.status.success(),
            "session save: {}",
            String::from_utf8_lossy(&save.stderr)
        );
        let end = warm_run(&wt, &home, &["session", "end"]);
        assert!(
            end.status.success(),
            "session end: {}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "session process: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !apg::session::live_session(&wt_apg),
            "session end must release the session"
        );

        // A minimal transient plan that passes `apg plan verify` with NO
        // planned node: one phase that `Satisfies` the changed requirement —
        // the only coverage obligation a spec-only delta creates — so the
        // verify gate has nothing to block on.
        let plan_records = vec![
            Record::Plan {
                fqn: "spec-only/plan".into(),
                title: "Spec-only merge".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "spec-only/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "the merged spec".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "spec-only/plan".into(),
                to: "spec-only/plan.phase-01".into(),
                properties: layers::NodeProperties::default(),
            },
            Record::Satisfies {
                from: "spec-only/plan.phase-01".into(),
                to: req_fqn.into(),
            },
            Record::Task {
                fqn: "spec-only/plan.phase-01.task-1".into(),
                title: "Author the spec".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "spec-only/plan.phase-01".into(),
                to: "spec-only/plan.phase-01.task-1".into(),
                properties: layers::NodeProperties::default(),
            },
        ];
        let plan_path = wt_apg
            .join(specs::TRANS)
            .join("plans")
            .join("spec-only.jsonl");
        artifacts::write_jsonl_and_reingest(&wt_apg, &plan_path, "spec-only", &plan_records)
            .unwrap();
        plan_cmd::plan_verify_at(&wt_apg, "spec-only").unwrap();

        // Merge from the main checkout with a REAL rebuild: the rebuild seam
        // spawns the candidate binary's `apg scan .` on main (the binary is
        // what resolves the staged frontends; the in-process default cannot).
        // That is the production incremental path whose win-C splice the guard
        // must refuse.
        // The trait object requires `'static`, so the closure owns its copy of
        // the isolated HOME (the test keeps `home` for the later re-scan).
        let rebuild_home = home.clone();
        let rebuild = move |dir: &Path| -> anyhow::Result<()> {
            let out = testutil::ApgCommand::new(&["scan", "."])
                .cwd(dir)
                .env("HOME", &rebuild_home.to_string_lossy())
                .output();
            if !out.status.success() {
                anyhow::bail!(
                    "main rebuild scan failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Ok(())
        };
        project_merge_at(&repo_apg, "spec-only", Some(&rebuild)).unwrap();

        // THE ASSERTION THAT ISOLATES THE DEFECT: main's `db.lbug` ITSELF (the
        // query index), not just `graph.jsonl`, carries the merged authored
        // node and edge. Pre-fix the splice published a DB without them while
        // the export, serialized from the full assembled graph, kept them.
        let main_apg = repo_dir.join(specs::LAYOUT);
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        assert!(
            db.has_node(req_fqn),
            "main's db.lbug must carry the merged authored requirement (the splice dropped it)"
        );
        assert!(
            db.has_node(val_fqn),
            "main's db.lbug must carry the merged authored value"
        );
        let count = |q: &str| -> i64 {
            db.q(q)
                .unwrap()
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0)
        };
        assert_eq!(
            count(&format!(
                "MATCH (:Requirement {{fqn: '{req_fqn}'}})-[:Drives]->(:Value {{fqn: '{val_fqn}'}}) RETURN count(*)"
            )),
            1,
            "main's db.lbug must carry the merged authored drives edge"
        );
        drop(db);

        // A subsequent scan must not MASK the loss: pre-fix the splice stamped
        // the merged HEAD, so an `apg scan` fast-pathed as fresh and never
        // corrected the missing row. The authored row must still be there.
        let rescan = warm_run(&repo_dir, &home, &["scan", "."]);
        assert!(
            rescan.status.success(),
            "post-merge re-scan: {}",
            String::from_utf8_lossy(&rescan.stderr)
        );
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        assert!(
            db.has_node(req_fqn),
            "a subsequent scan must not mask the authored-row loss"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&base);
    }
}
