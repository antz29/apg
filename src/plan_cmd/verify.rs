//! The `apg plan verify` coherence gate: planned-node realization, resolved
//! feedback, and spine-scoped derived solution coverage.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::artifacts::{self, parse_args};
use crate::schema::Record;
use crate::specs;

use super::{load_plan, require_apg_root};

/// `apg plan verify <project>` (R5 — renamed from `apply`): the pre-merge
/// coherence gate (PlanCompletion-SPEC.md). Runs against the branch's graph:
///
/// - every planned Implementation node is realized in the code graph (a
///   planned node with no real code at its FQN blocks verify);
/// - every phase and the whole-plan review are green (all `Feedback` resolved);
/// - spine-scoped derived solution coverage holds (SPEC §5): every solution
///   node reached from a satisfied requirement, plus every solution node added
///   on this branch, has its `implemented-by` FQN touched by a plan task
///   ([`coverage_check`]);
/// - the human gate has passed (the navigator's summary; outside the CLI).
///
/// The gate is all this command checks — it performs NO merge and NO graph
/// mutation. Guarded (R5): the verdict is only meaningful against the
/// project's branch graph — outside the project context, or against a stale
/// branch DB, verify refuses. On green it prints the merge handoff; the merge
/// act itself is `apg project merge <project>` (git2-operated from the main
/// checkout).
///
/// Invariants are deliberately NOT evaluated here (wont-fix, REVIEW.md): an
/// invariant's body is free prose, so a mechanical pass could not check it,
/// and Invariants-SPEC's "Correctness never depends on them" makes a
/// gate-blocking invariant incoherent with the emergent model — the navigator
/// verifies the GuardedBy set (`apg invariants`) as part of the human gate.
pub(crate) fn plan_verify(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan verify <project>");
    };
    let apg_root = require_apg_root()?;
    plan_verify_at(&apg_root, project)
}

/// The FQN candidates whose realization satisfies the planned-node gate: the
/// authored `fqn` itself, its language-agnostic identity, and every
/// `<root>.<identity>` variant. Pure — the caller checks each against the
/// branch DB, so a planned node named bare (`apg.cache`) is realized by rooted
/// scanned code (`rust.apg.cache`) and vice versa. An `UnresolvedTarget` at a
/// candidate is never real: the caller's `impl_label` check admits only the four
/// Implementation labels.
pub(crate) fn realization_candidates(fqn: &str) -> Vec<String> {
    let identity = crate::layers::code_identity(fqn);
    let mut out = vec![fqn.to_string(), identity.to_string()];
    for root in crate::layers::LANGUAGE_ROOTS {
        out.push(format!("{root}.{identity}"));
    }
    out.sort();
    out.dedup();
    out
}

/// Core of `plan verify` — the coherence gate. Returns the merge handoff
/// message on green, or errors listing every blocker (unrealized planned
/// nodes, unresolved feedback, coverage gaps). Guarded: refuses outside the
/// project context and against a stale branch DB (a verdict is only
/// meaningful against the branch's graph — R5).
pub fn plan_verify_at(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    crate::git::require_membership(apg_root, project)?;
    if crate::git::is_stale(apg_root) {
        anyhow::bail!(
            "cannot verify `{project}`: the branch graph is stale — run `apg scan` inside the project worktree first (a verdict is only meaningful against a fresh branch graph)"
        );
    }
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
    let records = load_plan(apg_root, project)?;
    let db = artifacts::ArtifactDb::open(apg_root)?;

    // 1. Every planned Implementation node in the branch is realized: a scan
    // found real (present) code at its FQN and replaced the placeholder. A
    // planned node still marked `planned`, or with no node at all, blocks
    // verify (PlanCompletion-SPEC.md — the planned-node realization gate).
    // Realization means one of the four Implementation labels — an
    // `UnresolvedTarget` at the FQN is NOT real code (REVIEW: the gate used to
    // accept any node label).
    let mut blocked: Vec<String> = Vec::new();
    let planned: Vec<(&str, &str)> = records
        .iter()
        .filter_map(|r| match r {
            Record::PlannedNode { fqn, kind, .. } => Some((fqn.as_str(), kind.as_str())),
            _ => None,
        })
        .collect();
    for (fqn, kind) in planned {
        // Language-root tolerant (mirrors `classify_code_ref`): realize on ANY
        // `<root>.` variant of the planned node's identity, so the parent
        // (scanned bare) and the child (scanned rooted) agree on the same target.
        let real = |name: &str| db.impl_label(name).is_some() && !db.is_planned(name);
        let realized = realization_candidates(fqn).iter().any(|c| real(c));
        if !realized {
            blocked.push(format!(
                "planned {kind} node `{fqn}` is not realized — a scan must find real code at its FQN before verify (missing or dangling code blocks verify)"
            ));
        }
    }

    // 2. All feedback resolved — every scope. Feedback lives in the project's
    // six transient files: the plan store plus the five tier mirrors (SPEC
    // §5). Feedback attached to a durable layer node or a code node never
    // lands in the plan store, so the gate reads every file itself — skipping
    // absent mirrors (`specs::read_jsonl` errors on a missing path). Every
    // unresolved Feedback (`open` or `actioned`) blocks verify, named by FQN;
    // `apg plan complete` stays phase-scoped (it reads only the plan store).
    let mut feedback_records: Vec<Record> = Vec::new();
    for f in specs::project_transient_files(apg_root, project) {
        if f.exists() {
            feedback_records.extend(specs::read_jsonl(&f)?);
        }
    }
    let unresolved: Vec<String> = feedback_records
        .iter()
        .filter_map(|r| match r {
            Record::Feedback { fqn, status, .. } if status != "resolved" => Some(fqn.clone()),
            _ => None,
        })
        .collect();

    // Report ALL blocker classes together: every unrealized planned node,
    // every unresolved feedback, and every coverage gap, in one gate refusal.
    let mut problems: Vec<String> = Vec::new();
    problems.extend(blocked);
    if !unresolved.is_empty() {
        problems.push(format!(
            "unresolved review feedback: {} — resolve every `Feedback` before verify",
            unresolved.join(", ")
        ));
    }

    // 3. Spine-scoped derived solution coverage (SPEC §5, requirement
    // coverage-spine-scoped): coverage is scoped to the plan's OWN spine plus
    // this branch's delta. For every requirement a phase `Satisfies`, walk
    // Requirement `Drives` Domain `RealisedBy` Solution and require each
    // reached solution node's `implemented-by` FQNs to be touched by a plan
    // task; PLUS every solution node added on this branch (the default-branch
    // delta). Pre-existing nodes unreachable from a satisfied requirement are
    // exempt — a prior project's implemented nodes force no fake `modifies`
    // tasks. Task targets come from the transient plan records (note-26: the
    // DB's Task table is static — the plan JSONL is the source of truth), the
    // solution nodes from the durable layers store. The suggestion names the
    // verb the uncovered FQN's status calls for: a `modifies` over code that
    // already resolves, a `creates` over a planned or still-absent FQN.
    let nodes = crate::layers::read_existing_nodes(apg_root)?;
    let satisfied: BTreeSet<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Satisfies { to, .. } => Some(to.clone()),
            _ => None,
        })
        .collect();
    let branch_added = solution_nodes_added_on_branch(apg_root, &nodes)?;
    let coverage = coverage_check(&records, &nodes, &satisfied, &branch_added);
    if !coverage.gaps.is_empty() {
        let (scanned, planned) = artifacts::code_universes(apg_root)?;
        let mut lines: Vec<String> = coverage
            .gaps
            .iter()
            .map(|g| {
                let verb = match crate::layers::classify_code_ref(&g.fqn, &scanned, &planned) {
                    crate::layers::CodeRefStatus::Real => "modifies",
                    _ => "creates",
                };
                format!(
                    "solution node `{}` implemented-by `{}` is touched by no plan task (add one with `--verb {verb} --fqn {}`)",
                    g.solution, g.fqn, g.fqn
                )
            })
            .collect();
        if !coverage.no_claims.is_empty() {
            lines.push(format!(
                "solution nodes with no implemented-by edge (exempt — nothing to touch, but no code claims them): {}",
                coverage.no_claims.join(", ")
            ));
        }
        problems.push(format!("coverage incomplete: {}", lines.join("; ")));
    }
    if !problems.is_empty() {
        anyhow::bail!("verify coherence gate blocked: {}", problems.join("; "));
    }
    if !coverage.no_claims.is_empty() {
        eprintln!(
            "apg: warning: solution nodes with no implemented-by edge (exempt from coverage — nothing to touch, but no code claims them): {}",
            coverage.no_claims.join(", ")
        );
    }
    println!(
        "Verify gate passed for {project}: every planned node is realized, all feedback resolved, solution coverage holds."
    );
    println!(
        "Merge: `apg project merge {project}` from the main checkout (verify gate → merge → main rebuild; push/tag remain human-approved acts)."
    );
    Ok(())
}

/// One uncovered `implemented-by` code FQN with the solution node that claims
/// it (SPEC §5 derived coverage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageGap {
    /// The owning solution node FQN (`solution.system.<name>` /
    /// `solution.container.<name>` / `solution.component.<name>`).
    pub solution: String,
    /// The `implemented-by` code FQN no plan task touches.
    pub fqn: String,
}

/// The derived-coverage verdict (SPEC §5): whether every in-scope solution
/// node's `implemented-by` FQN is touched by at least one plan task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    /// The uncovered `implemented-by` FQNs with their owning solution node —
    /// non-empty iff coverage does NOT hold (the coherence gate refuses).
    pub gaps: Vec<CoverageGap>,
    /// In-scope solution nodes declaring NO `implemented-by` edge — exempt
    /// from the coverage rule (with no FQN there is nothing to touch),
    /// surfaced as a warning so the bridge gap ("no code claims this node")
    /// stays visible.
    pub no_claims: Vec<String>,
}

/// The **branch delta** — the solution-layer node FQNs present on the current
/// branch but NOT on the repo's default branch. The default branch is read
/// from the public [`crate::git::repo_identity`] (`default_branch`); the
/// private `origin_default_branch` helper is never consulted here.
///
/// This is the one impure piece of the coverage rule (it opens the repo's
/// object database); it returns a plain FQN set the pure [`coverage_check`]
/// consumes. When no default branch resolves (a detached/unborn main
/// checkout), the delta is empty and coverage falls back to spine
/// reachability alone.
fn solution_nodes_added_on_branch(
    apg_root: &Path,
    nodes: &[crate::layers::NodeFile],
) -> anyhow::Result<BTreeSet<String>> {
    let identity = crate::git::repo_identity(apg_root)?;
    let Some(default) = identity.default_branch.as_deref() else {
        return Ok(BTreeSet::new());
    };
    // The default branch lives in the main checkout's ref store (shared with
    // every linked worktree), so resolve it from `main_root`.
    let repo = git2::Repository::open(&identity.main_root)?;
    let default_ref = repo
        .find_reference(&format!("refs/heads/{default}"))
        .or_else(|_| repo.find_reference(&format!("refs/remotes/origin/{default}")))
        .map_err(|_| {
            anyhow::anyhow!(
                "cannot compute the branch delta: default branch `{default}` is not a local ref"
            )
        })?;
    let default_tree = default_ref.peel_to_commit()?.tree()?;
    // The repo-relative layout dir (`apg`), so the layer files resolve inside
    // the default branch's tree. Canonicalized on both sides so a symlinked
    // temp dir does not produce a spurious prefix mismatch.
    let layout = std::fs::canonicalize(apg_root)
        .ok()
        .and_then(|p| {
            p.strip_prefix(&identity.checkout_root)
                .ok()
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from(specs::LAYOUT));
    let mut added = BTreeSet::new();
    for n in nodes.iter().filter(|n| n.layer == "solution") {
        let rel = layout
            .join(crate::layers::LAYERS_DIR)
            .join(&n.layer)
            .join(&n.node_type)
            .join(format!("{}.json", n.name));
        if default_tree.get_path(&rel).is_err() {
            added.insert(crate::layers::fqn(
                crate::layers::Layer::Solution,
                &n.node_type,
                &n.name,
            ));
        }
    }
    Ok(added)
}

/// Spine-scoped derived solution coverage (SPEC §5, requirement
/// coverage-spine-scoped). The solution nodes in scope are:
///
/// 1. every solution node reached from a satisfied requirement (`satisfied`,
///    a PlanPhase's `Satisfies` targets) through the spine — Requirement
///    `Drives` Domain, Domain `RealisedBy` Solution; and
/// 2. every solution node added on this branch (`branch_added`, the
///    default-branch delta [`solution_nodes_added_on_branch`] computes).
///
/// Each in-scope solution node's `implemented-by` code FQNs must be touched
/// by at least one plan task. A pre-existing solution node unreachable from a
/// satisfied requirement is EXEMPT — it belongs to an earlier project's spine
/// and must not force a fake `modifies` task.
///
/// A task touches an FQN when its verb's subject equals it: `target` for
/// every verb, plus `new_fqn` for a renames/moves pair (the destination FQN
/// the code lands at). Coverage is verb-agnostic and status-agnostic: a
/// `creates` over a still-planned FQN counts exactly like a `modifies` over
/// real code — the task's claim is what coverage measures, and the
/// realization gate separately ensures the code actually lands.
///
/// A solution node with NO `implemented-by` edge is exempt — the rule is over
/// the node's implemented-by FQNs, and with none there is nothing to touch —
/// but it is reported in [`CoverageReport::no_claims`] (a warning, never a
/// blocker), whether it is spine-reached or branch-added.
///
/// Pure — no I/O. The caller supplies the transient plan records (note-26:
/// the `.trans/plans/<project>.jsonl`, not the DB's static Task table, is the
/// task source of truth), the durable node files
/// ([`crate::layers::read_existing_nodes`]), the satisfied requirement FQNs,
/// and the branch-added solution FQNs.
pub fn coverage_check(
    records: &[Record],
    nodes: &[crate::layers::NodeFile],
    satisfied: &BTreeSet<String>,
    branch_added: &BTreeSet<String>,
) -> CoverageReport {
    // Language-agnostic identity (commit 7a6ed03e + `code_identity`): the durable
    // plan may name a rooted FQN (`rust.apg.cache`) while the solution node's
    // `implemented-by` target is bare (`apg.cache`), or vice versa — compare
    // IDENTITIES, not raw strings, so coverage is root-agnostic.
    let mut touched: BTreeSet<&str> = BTreeSet::new();
    for r in records {
        if let Record::Task {
            verb,
            target,
            new_fqn,
            ..
        } = r
        {
            if !target.is_empty() {
                touched.insert(crate::layers::code_identity(target));
            }
            if matches!(verb.as_str(), "renames" | "moves") && !new_fqn.is_empty() {
                touched.insert(crate::layers::code_identity(new_fqn));
            }
        }
    }

    // Resolve every loaded node by its derived FQN so the spine walk can
    // follow `out` edges between node files.
    let mut by_fqn: std::collections::BTreeMap<String, &crate::layers::NodeFile> =
        std::collections::BTreeMap::new();
    for n in nodes {
        let Some(layer) = crate::layers::Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == n.layer)
            .copied()
        else {
            continue;
        };
        let fqn = crate::layers::fqn(layer, &n.node_type, &n.name);
        by_fqn.insert(fqn, n);
    }

    // The spine: from each satisfied requirement follow Drives to its domain
    // node(s), then RealisedBy to the solution node(s) they are realised by.
    let mut required: BTreeSet<String> = BTreeSet::new();
    for req in satisfied {
        let Some(req_node) = by_fqn.get(req) else {
            continue;
        };
        for d in req_node.out.iter().filter(|e| e.kind == "drives") {
            let Some(domain) = by_fqn.get(&d.target) else {
                continue;
            };
            for rb in domain.out.iter().filter(|e| e.kind == "realised-by") {
                required.insert(rb.target.clone());
            }
        }
    }
    required.extend(branch_added.iter().cloned());

    let mut gaps: Vec<CoverageGap> = Vec::new();
    let mut no_claims: Vec<String> = Vec::new();
    for n in nodes {
        if n.layer != "solution"
            || !["system", "container", "component"].contains(&n.node_type.as_str())
        {
            continue;
        }
        let solution = crate::layers::fqn(crate::layers::Layer::Solution, &n.node_type, &n.name);
        if !required.contains(&solution) {
            // A pre-existing solution node outside this plan's spine (and not
            // added on this branch) is exempt.
            continue;
        }
        let refs: Vec<&str> = n
            .out
            .iter()
            .filter(|oe| oe.kind == "implemented-by")
            .map(|oe| oe.target.as_str())
            .collect();
        if refs.is_empty() {
            no_claims.push(solution);
            continue;
        }
        for fqn in refs {
            if !touched.contains(crate::layers::code_identity(fqn)) {
                gaps.push(CoverageGap {
                    solution: solution.clone(),
                    fqn: fqn.to_string(),
                });
            }
        }
    }
    // Deterministic order for the verdict and the tests.
    gaps.sort_by(|a, b| (&a.solution, &a.fqn).cmp(&(&b.solution, &b.fqn)));
    no_claims.sort();
    CoverageReport { gaps, no_claims }
}

/// Whether the layers store has a requirement with this FQN
/// (`requirements.requirement.<name>` — one node file per requirement under
/// `apg/layers/requirements/requirement/`, no project prefix; the legacy spec
/// JSONL is gone).
pub(crate) fn spec_has_requirement(
    apg_root: &Path,
    _project: &str,
    req_fqn: &str,
) -> anyhow::Result<bool> {
    Ok(crate::layers::read_existing_nodes(apg_root)?
        .iter()
        .any(|n| {
            n.layer == "requirements"
                && n.node_type == "requirement"
                && crate::layers::fqn(crate::layers::Layer::Requirements, &n.node_type, &n.name)
                    == req_fqn
        }))
}
