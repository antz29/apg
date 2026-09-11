//! `apg plan` — the phased execution plan that bridges the spec to present
//! code (PHASE_03 execution + apply model). The plan is transient by design
//! (R22): it lives in the gitignored `apg/.trans/plans/<project>.jsonl` and is
//! branch-local. The plan-writer authors the tier-4 additions as **planned
//! Implementation nodes** (`apg plan add <project> planned <kind> <fqn>` —
//! Module/File/Struct/Function marked `status: planned` at the FQN where the
//! code will land, GraphModel-SPEC.md); a task declares how it touches the
//! Implementation tier through its **verb** (`creates`/`modifies`/`deletes`/
//! `renames`/`moves`, SPEC §5) with the target FQN(s) recorded on the task
//! itself. Nothing advances automatically with `plan done`/`plan complete` —
//! those are assertion + milestone only; a branch scan **replaces realized
//! planned nodes**. The plan survives until the apply act, whose coherence gate (every
//! planned node realized, all feedback resolved, derived solution coverage
//! holds — SPEC §5: every solution node reached from a satisfied requirement,
//! plus every solution node added on this branch, has its `implemented-by` FQN
//! touched by a plan task) precedes the merge + rebuild of `main`'s graph
//! (PlanCompletion-SPEC.md).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::artifacts::{self, parse_args};
use crate::schema::Record;
use crate::specs;

fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

fn load_plan(apg_root: &Path, project: &str) -> anyhow::Result<Vec<Record>> {
    let path = specs::plan_jsonl_path(apg_root, project);
    if !path.exists() {
        anyhow::bail!("no plan for project `{project}` — run `apg plan add {project}` first");
    }
    specs::read_jsonl(&path)
}

fn write_through(apg_root: &Path, project: &str, records: &[Record]) -> anyhow::Result<()> {
    artifacts::write_jsonl_and_reingest(
        apg_root,
        &specs::plan_jsonl_path(apg_root, project),
        project,
        records,
    )
}

pub fn cmd_plan(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg plan <add|update|rm|done|undone|note|complete|render|verify> …");
    };
    match sub {
        "add" => plan_add(&args[1..]),
        "update" => plan_update(&args[1..]),
        "rm" => plan_rm(&args[1..]),
        "done" => plan_done(&args[1..]),
        "undone" => plan_undone(&args[1..]),
        "note" => plan_note(&args[1..]),
        "complete" => plan_complete(&args[1..]),
        "render" => plan_render(&args[1..]),
        "verify" => plan_verify(&args[1..]),
        // R5: `apg plan apply` was renamed `verify` — the binary applies
        // nothing; verify is the pre-merge coherence gate.
        "apply" => anyhow::bail!(
            "`apg plan apply` was renamed `verify` (R5) — the binary applies nothing; run `apg plan verify <project>` (the merge act is `apg project merge <project>`)"
        ),
        other => anyhow::bail!("unknown apg plan subcommand: {other}"),
    }
}

/// Core of the plan-record `add` arm (`apg plan add <project>` — no second
/// positional): writes the Plan record into the transient plan store
/// (`.trans/plans/<project>.jsonl`) and refuses when that store already
/// exists. The plan is the tier-4 bridge for a project whose requirements
/// live in the layers store (`apg/layers/requirements/`, SPEC §5). The legacy
/// spec-exists gate (the old `apg/specs/<project>.jsonl` and `apg spec init`)
/// is gone: a plan without any requirement node files yet is allowed, but the
/// empty spec is surfaced as a warning — phases added with `--satisfies`
/// validate against the layers store and will refuse until the requirement
/// tier exists. Returns whether the layers store holds any requirement node
/// file; the CLI wrapper surfaces the warning when none exists yet (a
/// warning, never a blocker).
fn plan_init_at(
    apg_root: &Path,
    project: &str,
    title: &str,
    strategy: &str,
) -> anyhow::Result<bool> {
    let path = specs::plan_jsonl_path(apg_root, project);
    if path.exists() {
        anyhow::bail!("plan for `{project}` already exists at {}", path.display());
    }
    let has_requirements = crate::layers::read_existing_nodes(apg_root)?
        .iter()
        .any(|n| n.layer == "requirements" && n.node_type == "requirement");
    let records = vec![Record::Plan {
        fqn: format!("{project}/plan"),
        title: title.to_string(),
        strategy: strategy.to_string(),
    }];
    write_through(apg_root, project, &records)?;
    Ok(has_requirements)
}

/// Core of the plan-record `update` arm: MERGE the plan's `--title`/
/// `--strategy` in place. An omitted flag leaves that field unchanged; an
/// absent plan is refused (creation is `apg plan add <project>`). Only the
/// `Record::Plan`'s two text fields change — every phase/task/planned record
/// and every plan edge (`Contains`/`Gates`/`Satisfies`/`Reviews`) is
/// preserved byte-for-byte through the whole-record rewrite.
fn plan_update_at(
    apg_root: &Path,
    project: &str,
    title: Option<&str>,
    strategy: Option<&str>,
) -> anyhow::Result<()> {
    let mut records = load_plan(apg_root, project)?;
    let plan_fqn = format!("{project}/plan");
    let mut found = false;
    for r in records.iter_mut() {
        if let Record::Plan {
            fqn,
            title: rec_title,
            strategy: rec_strategy,
        } = r
            && fqn == &plan_fqn
        {
            if let Some(new_title) = title {
                *rec_title = new_title.to_string();
            }
            if let Some(new_strategy) = strategy {
                *rec_strategy = new_strategy.to_string();
            }
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!("no plan for project `{project}` — run `apg plan add {project}` first");
    }
    write_through(apg_root, project, &records)?;
    Ok(())
}

/// `apg plan update <project> [--title T] [--strategy S]` /
/// `apg plan update <project> phase <n> [--title …] [--deliverable …]
/// [--prereq <n>]* [--satisfies <req>]*` /
/// `apg plan update <project> task <phase> <k> [--title …] [--kind …]
/// [--tier …] [--verb …] [--fqn …] [--to …]` /
/// `apg plan update <project> planned <fqn> [--kind …] [--name …] [--parent …]`.
///
/// A missing flag leaves that field unchanged: the plan-record arm MERGEs
/// title/strategy, the phase/task arms update in place (every phase/task and
/// every edge survives — except the phase's replaced Satisfies/Gates sets),
/// and the planned arm repoints the parent `Contains` edge. Every arm refuses
/// an absent target. `apg plan link` is retired: its bridge set-semantics live
/// here (`phase` + `--satisfies`/`--prereq`).
fn plan_update(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!(
            "usage: apg plan update <project> [phase <n>|task <phase> <k>|planned <fqn>] [--title T] [--strategy S] …"
        );
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let Some(kind) = p.positional.get(1).map(|s| s.as_str()) else {
        // The plan-record arm: no second positional means "the plan itself".
        plan_update_at(
            &apg_root,
            project,
            p.get("title").as_deref(),
            p.get("strategy").as_deref(),
        )?;
        println!("Updated plan {project}");
        return Ok(());
    };
    let mut records = load_plan(&apg_root, project)?;
    match kind {
        "phase" => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!(
                    "usage: apg plan update <project> phase <n> [--title …] [--deliverable …] [--prereq <n>]* [--satisfies <req>]*"
                );
            };
            let prereqs = p.has("prereq").then(|| p.all("prereq"));
            let satisfies = p.has("satisfies").then(|| p.all("satisfies"));
            plan_update_phase_at(
                &apg_root,
                project,
                &mut records,
                n,
                p.get("title").as_deref(),
                p.get("deliverable").as_deref(),
                prereqs.as_deref(),
                satisfies.as_deref(),
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Updated phase {n} of plan {project}");
        }
        "task" => {
            let (Some(phase), Some(k)) = (
                p.positional.get(2).and_then(|s| s.parse::<u32>().ok()),
                p.positional.get(3).and_then(|s| s.parse::<u32>().ok()),
            ) else {
                anyhow::bail!(
                    "usage: apg plan update <project> task <phase> <k> [--title …] [--kind …] [--tier …] [--verb …] [--fqn …] [--to …]"
                );
            };
            plan_update_task_at(
                &apg_root,
                project,
                &mut records,
                phase,
                k,
                p.get("title").as_deref(),
                p.get("kind").as_deref(),
                p.get("tier").as_deref(),
                p.get("verb").as_deref(),
                p.get("fqn").as_deref(),
                p.get("to").as_deref(),
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Updated task {k} in plan.phase-{phase} of {project}");
        }
        "planned" => {
            let Some(fqn) = p.positional.get(2) else {
                anyhow::bail!(
                    "usage: apg plan update <project> planned <fqn> [--kind module|file|struct|function] [--name <name>] [--parent <parent-fqn>]"
                );
            };
            plan_update_planned_at(
                &apg_root,
                &mut records,
                fqn,
                p.get("kind").as_deref(),
                p.get("name").as_deref(),
                p.get("parent").as_deref(),
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Updated planned `{fqn}` in plan {project}");
        }
        other => anyhow::bail!("unknown plan update kind `{other}` — phase|task|planned"),
    }
    Ok(())
}

/// Core of the `update phase` arm: update a phase's title/deliverable IN PLACE
/// (the phase record + the plan `Contains` edge + every task `Contains` edge
/// survive) and fold `apg plan link`'s set-semantics into the same surface.
///
/// `satisfies`/`prereqs` are `Option` so an omitted flag leaves that bridge
/// dimension untouched while a passed set replaces the phase's outgoing edges
/// (via [`link_phase_edges`], unchanged). `--satisfies` targets are validated
/// against the layers requirement store BEFORE `link_phase_edges` runs, so a
/// bogus target refuses before any mutation. An absent phase is refused.
#[allow(clippy::too_many_arguments)]
fn plan_update_phase_at(
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
    n: u32,
    title: Option<&str>,
    deliverable: Option<&str>,
    prereqs: Option<&[String]>,
    satisfies: Option<&[String]>,
) -> anyhow::Result<()> {
    let fqn = format!("{project}/plan.phase-{n:02}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::PlanPhase { fqn: pf, .. } if pf == &fqn))
    {
        anyhow::bail!(
            "update phase: `{fqn}` is not a phase of `{project}` (author it first with `apg plan add {project} phase {n} …`)"
        );
    }
    // Validate the requirements before mutating anything — `link_phase_edges`
    // does not check them itself.
    if let Some(reqs) = satisfies {
        for req in reqs {
            let req_fqn = format!("requirements.requirement.{req}");
            if !spec_has_requirement(apg_root, project, &req_fqn)? {
                anyhow::bail!(
                    "satisfies target `{req}` is not a requirement of `{project}` — requirements live in apg/layers/requirements/"
                );
            }
        }
    }
    // In-place title/deliverable update: no remove/re-add, so the phase record
    // and every task `Contains` edge survive.
    for r in records.iter_mut() {
        if let Record::PlanPhase {
            fqn: pf,
            title: t,
            deliverable: d,
            ..
        } = r
            && pf == &fqn
        {
            if let Some(t2) = title {
                *t = t2.to_string();
            }
            if let Some(d2) = deliverable {
                *d = d2.to_string();
            }
        }
    }
    // Bridge edges only when a set was passed: an omitted dimension keeps the
    // phase's current outgoing edges by reconstructing them and handing both
    // dimensions to `link_phase_edges` (its set-semantics rewrite is unchanged).
    if satisfies.is_some() || prereqs.is_some() {
        let phase_prefix = format!("{project}/plan.phase-");
        let req_prefix = "requirements.requirement.";
        let cur_reqs: Vec<String> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == &fqn => {
                    to.strip_prefix(req_prefix).map(str::to_string)
                }
                _ => None,
            })
            .collect();
        let cur_prereqs: Vec<String> = records
            .iter()
            .filter_map(|r| match r {
                Record::Gates { from, to } if from == &fqn => {
                    to.strip_prefix(&phase_prefix).map(str::to_string)
                }
                _ => None,
            })
            .collect();
        let reqs = satisfies.unwrap_or(&cur_reqs);
        let prs = prereqs.unwrap_or(&cur_prereqs);
        link_phase_edges(&fqn, reqs, prs, records)?;
    }
    Ok(())
}

/// Core of the `update task` arm: update a task's fields IN PLACE, preserving
/// its `status` and every incident `Reviews` edge. The effective `(kind,
/// tier)` and `(verb, target, new_fqn)` triples (each supplied value or the
/// existing one) are re-validated exactly as `add` does, so an update can
/// never introduce an invalid classification or verb/target. An absent task is
/// refused before any mutation.
#[allow(clippy::too_many_arguments)]
fn plan_update_task_at(
    apg_root: &Path,
    project: &str,
    records: &mut [Record],
    phase: u32,
    k: u32,
    title: Option<&str>,
    kind: Option<&str>,
    tier: Option<&str>,
    verb: Option<&str>,
    target: Option<&str>,
    new_fqn: Option<&str>,
) -> anyhow::Result<()> {
    let fqn = format!("{project}/plan.phase-{phase:02}.task-{k}");
    let Some((cur_kind, cur_tier, cur_verb, cur_target, cur_new_fqn)) =
        records.iter().find_map(|r| match r {
            Record::Task {
                fqn: tf,
                kind,
                tier,
                verb,
                target,
                new_fqn,
                ..
            } if tf == &fqn => Some((
                kind.clone(),
                tier.clone(),
                verb.clone(),
                target.clone(),
                new_fqn.clone(),
            )),
            _ => None,
        })
    else {
        anyhow::bail!(
            "update task: `{fqn}` is not a task of `{project}` (author it first with `apg plan add {project} task {phase} {k} …`)"
        );
    };
    let eff_kind = kind.unwrap_or(cur_kind.as_str());
    let eff_tier = tier.unwrap_or(cur_tier.as_str());
    validate_task_kind_tier(eff_kind, eff_tier)?;
    let eff_verb = verb.unwrap_or(cur_verb.as_str());
    let eff_target = target.unwrap_or(cur_target.as_str());
    let eff_new_fqn = new_fqn.unwrap_or(cur_new_fqn.as_str());
    let (scanned, planned) = task_verb_universes(apg_root, records)?;
    validate_task_verb(eff_verb, eff_target, eff_new_fqn, &scanned, &planned)?;
    for r in records.iter_mut() {
        if let Record::Task {
            fqn: tf,
            title: t,
            kind: kd,
            tier: tr,
            verb: vb,
            target: tg,
            new_fqn: nf,
            ..
        } = r
            && tf == &fqn
        {
            if let Some(x) = title {
                *t = x.to_string();
            }
            if let Some(x) = kind {
                *kd = x.to_string();
            }
            if let Some(x) = tier {
                *tr = x.to_string();
            }
            if let Some(x) = verb {
                *vb = x.to_string();
            }
            if let Some(x) = target {
                *tg = x.to_string();
            }
            if let Some(x) = new_fqn {
                *nf = x.to_string();
            }
        }
    }
    Ok(())
}

/// Core of the `update planned` arm: update a planned node's kind/name/parent
/// IN PLACE, repointing the parent `Contains` edge (set-semantics for parent)
/// while preserving every other incident edge. Refuses a real code FQN (a
/// realized placeholder is no longer a plan) and an absent planned node.
fn plan_update_planned_at(
    apg_root: &Path,
    records: &mut Vec<Record>,
    fqn: &str,
    node_kind: Option<&str>,
    name: Option<&str>,
    parent: Option<&str>,
) -> anyhow::Result<()> {
    let Some((cur_kind, cur_name, cur_parent)) = records.iter().find_map(|r| match r {
        Record::PlannedNode {
            fqn: pf,
            kind,
            name,
            parent,
        } if pf == fqn => Some((kind.clone(), name.clone(), parent.clone())),
        _ => None,
    }) else {
        anyhow::bail!(
            "update planned: `{fqn}` is not declared in this plan (declare it first with `apg plan add <project> planned <kind> {fqn}`)"
        );
    };
    let eff_kind = node_kind.unwrap_or(cur_kind.as_str());
    if !["module", "file", "struct", "function"].contains(&eff_kind) {
        anyhow::bail!("planned node kind must be module/file/struct/function, got `{eff_kind}`");
    }
    // A planned FQN that has since become real scanned code is no longer a
    // placeholder — the scanner-replace superseded the plan's claim on it.
    let (scanned, planned) = task_verb_universes(apg_root, records)?;
    if crate::layers::classify_code_ref(fqn, &scanned, &planned)
        == crate::layers::CodeRefStatus::Real
    {
        let label = artifacts::ArtifactDb::open(apg_root)
            .ok()
            .and_then(|db| db.impl_label(fqn))
            .unwrap_or("code");
        anyhow::bail!(
            "planned node `{fqn}` already resolves to a `{label}` code node — a plan never plans existing code (plan the delta, not the present)"
        );
    }
    let eff_name = name.unwrap_or(cur_name.as_str());
    let eff_parent = parent.unwrap_or(cur_parent.as_str());
    for r in records.iter_mut() {
        if let Record::PlannedNode {
            fqn: pf,
            kind,
            name,
            parent,
        } = r
            && pf == fqn
        {
            *kind = eff_kind.to_string();
            *name = eff_name.to_string();
            *parent = eff_parent.to_string();
        }
    }
    // Repoint the parent `Contains` edge (set semantics): drop every Contains
    // edge targeting this planned node, then add the new parent when one is
    // set. No other incident edge is touched.
    records.retain(|r| !matches!(r, Record::Contains { to, .. } if to == fqn));
    if !eff_parent.is_empty() {
        records.push(Record::Contains {
            from: eff_parent.to_string(),
            to: fqn.to_string(),
        });
    }
    Ok(())
}

/// `apg plan rm <project> [phase <n>|task <phase> <k>|planned <fqn>] [--force]`
/// — the remove arm of the strict add/update/rm plan surface. A missing second
/// positional removes the plan itself; `phase`/`task`/`planned` remove one
/// sub-entity. Without `--force` a remove refuses while the entity still has
/// dependents (a plan with phases/tasks/planned nodes, a phase with tasks, a
/// `done` or feedback-bearing task, a `planned` node targeted by a `creates`
/// task), naming the dependent and the `--force` escape; `--force` cascades the
/// entity and its dependents. A non-existent entity is a hard error, never a
/// silent no-op. Every arm rewrites the whole-record JSONL exactly once (never
/// a half-deleted plan) — the core functions do the load → in-memory cascade →
/// single write-through, so a refusal or a mid-cascade failure leaves the store
/// byte-identical.
fn plan_rm(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!(
            "usage: apg plan rm <project> [phase <n>|task <phase> <k>|planned <fqn>] [--force]"
        );
    };
    let force = p.has("force");
    let apg_root = require_apg_root()?;
    match p.positional.get(1).map(|s| s.as_str()) {
        None => plan_rm_at(&apg_root, project, force),
        Some("phase") => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!("usage: apg plan rm <project> phase <n> [--force]");
            };
            plan_rm_phase_at(&apg_root, project, n, force)
        }
        Some("task") => {
            let (Some(phase), Some(k)) = (
                p.positional.get(2).and_then(|s| s.parse::<u32>().ok()),
                p.positional.get(3).and_then(|s| s.parse::<u32>().ok()),
            ) else {
                anyhow::bail!("usage: apg plan rm <project> task <phase> <k> [--force]");
            };
            plan_rm_task_at(&apg_root, project, phase, k, force)
        }
        Some("planned") => {
            let Some(fqn) = p.positional.get(2) else {
                anyhow::bail!("usage: apg plan rm <project> planned <fqn> [--force]");
            };
            plan_rm_planned_at(&apg_root, project, fqn, force)
        }
        Some(other) => anyhow::bail!("unknown plan rm kind `{other}` — phase|task|planned"),
    }
}

/// Shared remove cascade: drop every node record at `fqns` plus every incident
/// edge ([`artifacts::remove_node`]), then garbage-collect any `Feedback`/`Note`
/// record left with no remaining incident `Reviews`/`Details` edge — so no edge
/// points at a removed record and no orphan Feedback/Note survives. Pure: the
/// caller rewrites the store exactly once ([`persist_rm`]), so all the cascade
/// work is in memory and any failure before that single write leaves the
/// on-disk plan untouched (plan-rm-atomic).
fn cascade_remove(records: &mut Vec<Record>, fqns: &[String]) {
    for fqn in fqns {
        artifacts::remove_node(records, fqn);
    }
    // A Feedback/Note whose only attachment was a removed node is an orphan:
    // its Reviews/Details edge is gone, so the record must go too.
    let reviewed: BTreeSet<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Reviews { from, .. } => Some(from.clone()),
            _ => None,
        })
        .collect();
    let detailed: BTreeSet<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Details { from, .. } => Some(from.clone()),
            _ => None,
        })
        .collect();
    records.retain(|r| match r {
        Record::Feedback { fqn, .. } => reviewed.contains(fqn.as_str()),
        Record::Note { fqn, .. } => detailed.contains(fqn.as_str()),
        _ => true,
    });
}

/// Commit one rm: a single whole-record write-through of `records`. An emptied
/// store (a plan-level cascade removed every record) is DELETED rather than
/// left as an empty file — an empty `<project>.jsonl` would make the next
/// `apg plan add <project>` refuse as already-existing. The empty set is still
/// re-ingested first so the branch DB drops the removed `<project>/…` nodes.
fn persist_rm(apg_root: &Path, project: &str, records: &[Record]) -> anyhow::Result<()> {
    write_through(apg_root, project, records)?;
    if records.is_empty() {
        let path = specs::plan_jsonl_path(apg_root, project);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Core of the plan-level `rm` (`apg plan rm <project>`): refuse while the plan
/// still carries any phase/task/planned node (naming every dependent and the
/// `--force` escape); `--force` cascades the WHOLE plan — the Plan record,
/// every phase, task and planned node, every dependent Feedback/Note, and all
/// their incident Contains/Gates/Satisfies/Reviews/Details edges — in one
/// in-memory pass. The emptied store is deleted ([`persist_rm`]), so a
/// following `apg plan add <project>` recreates it. An absent plan is an error
/// and the stored file is left untouched (nothing is written before the whole
/// cascade is computed).
fn plan_rm_at(apg_root: &Path, project: &str, force: bool) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let dependents: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::PlanPhase { fqn, .. }
            | Record::Task { fqn, .. }
            | Record::PlannedNode { fqn, .. } => Some(fqn.clone()),
            _ => None,
        })
        .collect();
    if !dependents.is_empty() && !force {
        anyhow::bail!(
            "plan `{project}` still has dependent nodes: {} — pass --force to cascade the whole plan (phases, tasks, planned nodes, and every dependent feedback/note)",
            dependents.join(", ")
        );
    }
    let plan_nodes: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Plan { fqn, .. }
            | Record::PlanPhase { fqn, .. }
            | Record::Task { fqn, .. }
            | Record::PlannedNode { fqn, .. } => Some(fqn.clone()),
            _ => None,
        })
        .collect();
    cascade_remove(&mut records, &plan_nodes);
    persist_rm(apg_root, project, &records)
}

/// Core of the `rm phase` arm: refuse while the phase still has any task
/// (naming the task and the `--force` escape). BOTH paths cascade the phase plus
/// every incident edge and the dependent Feedback/Note records; the `--force`
/// path also removes the phase's tasks. A phase whose only dependents are
/// Feedback/Note is removable WITHOUT `--force`. An absent phase is an error.
fn plan_rm_phase_at(apg_root: &Path, project: &str, n: u32, force: bool) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let phase_fqn = format!("{project}/plan.phase-{n:02}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::PlanPhase { fqn, .. } if fqn == &phase_fqn))
    {
        anyhow::bail!("no phase {n} in plan `{project}` — nothing to remove (`{phase_fqn}`)");
    }
    let task_prefix = format!("{phase_fqn}.task-");
    let tasks: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Task { fqn, .. } if fqn.starts_with(&task_prefix) => Some(fqn.clone()),
            _ => None,
        })
        .collect();
    if !tasks.is_empty() && !force {
        anyhow::bail!(
            "phase {n} of `{project}` still has dependent tasks: {} — pass --force to cascade the phase and its tasks (plus dependent feedback/notes)",
            tasks.join(", ")
        );
    }
    let mut remove = vec![phase_fqn];
    if force {
        remove.extend(tasks);
    }
    cascade_remove(&mut records, &remove);
    persist_rm(apg_root, project, &records)
}

/// Core of the `rm task` arm: refuse when the task is `done` or has ANY
/// incident Feedback (naming the status/Feedback and the `--force` escape — a
/// removed task would otherwise strand its resolved Feedback mirror); `--force`
/// cascades the task, its Contains edge, its incident Notes/Reviews edges and
/// any Feedback/Note left orphaned. An absent task is an error.
fn plan_rm_task_at(
    apg_root: &Path,
    project: &str,
    phase: u32,
    k: u32,
    force: bool,
) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let fqn = format!("{project}/plan.phase-{phase:02}.task-{k}");
    let Some(status) = records.iter().find_map(|r| match r {
        Record::Task {
            fqn: tf, status, ..
        } if tf == &fqn => Some(status.clone()),
        _ => None,
    }) else {
        anyhow::bail!("no task {k} in phase {phase} of `{project}` — nothing to remove (`{fqn}`)");
    };
    let feedback: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Reviews { from, to } if to == &fqn => Some(from.clone()),
            _ => None,
        })
        .collect();
    if !force {
        let mut reasons: Vec<String> = Vec::new();
        if status == "done" {
            reasons.push(format!("it is `{status}`"));
        }
        if !feedback.is_empty() {
            reasons.push(format!("it has incident feedback: {}", feedback.join(", ")));
        }
        if !reasons.is_empty() {
            anyhow::bail!(
                "task `{fqn}` cannot be removed — {}; pass --force to cascade the task and its incident feedback/notes",
                reasons.join("; ")
            );
        }
    }
    cascade_remove(&mut records, &[fqn]);
    persist_rm(apg_root, project, &records)
}

/// Core of the `rm planned` arm: refuse while a `creates` task still targets
/// the FQN (naming the task and the `--force` escape); `--force` removes the
/// PlannedNode plus its parent Contains edge (and every other incident edge).
/// An absent planned node is an error.
fn plan_rm_planned_at(
    apg_root: &Path,
    project: &str,
    fqn: &str,
    force: bool,
) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    if !records
        .iter()
        .any(|r| matches!(r, Record::PlannedNode { fqn: pf, .. } if pf == fqn))
    {
        anyhow::bail!("no planned node `{fqn}` in plan `{project}` — nothing to remove");
    }
    let creators: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Task {
                fqn: tf,
                verb,
                target,
                ..
            } if target == fqn && (verb.is_empty() || verb == "creates") => Some(tf.clone()),
            _ => None,
        })
        .collect();
    if !creators.is_empty() && !force {
        anyhow::bail!(
            "planned node `{fqn}` is targeted by a creates task: {} — pass --force to remove the planned node and its parent Contains edge",
            creators.join(", ")
        );
    }
    cascade_remove(&mut records, &[fqn.to_string()]);
    persist_rm(apg_root, project, &records)
}

/// `apg plan add <project>` (the plan itself — no second positional) /
/// `apg plan add <project> phase <n> …` / `task <phase> <k> …` /
/// `planned <kind> <fqn> …` (R23, PHASE_02). The create arm refuses when the
/// plan already exists and warns when the requirement tier is empty.
fn plan_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan add <project> [phase|task|planned] …");
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let Some(kind) = p.positional.get(1).map(|s| s.as_str()) else {
        // The plan-record create arm: `apg plan add <project>` (no second
        // positional) creates the plan itself — the surface formerly spelled
        // `apg plan init`. It refuses when the plan already exists.
        let has_requirements = plan_init_at(
            &apg_root,
            project,
            &p.get("title")
                .unwrap_or_else(|| format!("Plan for {project}")),
            &p.get("strategy").unwrap_or_default(),
        )?;
        if !has_requirements {
            eprintln!(
                "apg: warning: no requirements under apg/layers/requirements/ — the plan for `{project}` has nothing to satisfy yet; author the requirement tier before adding phases with --satisfies"
            );
        }
        let path = specs::plan_jsonl_path(&apg_root, project);
        println!("Added plan {project} at {}", path.display());
        return Ok(());
    };
    let mut records = load_plan(&apg_root, project)?;
    let plan_fqn = format!("{project}/plan");
    match kind {
        "planned" => {
            let (Some(node_kind), Some(fqn)) =
                (p.positional.get(2).map(|s| s.as_str()), p.positional.get(3))
            else {
                anyhow::bail!(
                    "usage: apg plan add <project> planned <kind> <fqn> [--name <name>] [--parent <parent-fqn>]"
                );
            };
            plan_add_planned_at(
                &apg_root,
                &mut records,
                node_kind,
                fqn,
                &p.get("name").unwrap_or_default(),
                p.get("parent").as_deref(),
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Added planned {node_kind} `{fqn}` to plan {project}");
        }
        "phase" => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!(
                    "usage: apg plan add <project> phase <n> --title … [--deliverable …] [--prereq <n>]* [--satisfies <req-id>]*"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("phase requires --title");
            };
            let prereqs: Vec<u32> = p
                .all("prereq")
                .iter()
                .map(|g| {
                    g.parse::<u32>()
                        .map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))
                })
                .collect::<anyhow::Result<_>>()?;
            let satisfies: Vec<String> = p.all("satisfies");
            plan_add_phase_at(
                &apg_root,
                project,
                &mut records,
                &plan_fqn,
                n,
                &title,
                p.get("deliverable").unwrap_or_default().as_str(),
                &prereqs,
                &satisfies,
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Added phase {n} to plan {project}");
        }
        "task" => {
            let (Some(phase), Some(k)) = (
                p.positional.get(2).and_then(|s| s.parse::<u32>().ok()),
                p.positional.get(3).and_then(|s| s.parse::<u32>().ok()),
            ) else {
                anyhow::bail!(
                    "usage: apg plan add <project> task <phase> <k> --title … [--kind <source|test|gate|docs>] [--tier <unit|int|e2e>] [--verb <creates|modifies|deletes|renames|moves>] [--fqn <fqn>] [--to <new-fqn>]"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("task requires --title");
            };
            let kind = p.get("kind").unwrap_or_else(|| "source".to_string());
            let tier = p.get("tier").unwrap_or_default();
            let verb = p.get("verb").unwrap_or_default();
            let target = p.get("fqn").unwrap_or_default();
            let new_fqn = p.get("to").unwrap_or_default();
            plan_add_task_at(
                &apg_root,
                project,
                &mut records,
                phase,
                k,
                &title,
                &kind,
                &tier,
                &verb,
                &target,
                &new_fqn,
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Added task {k} to plan.phase-{phase} of {project}");
        }
        other => anyhow::bail!("unknown plan add kind `{other}` — phase|task|planned"),
    }
    Ok(())
}

/// Core of the `phase` add arm (extracted for tests): refuses an existing
/// phase (no implicit upsert — the strict surface is add/update/rm), then
/// appends the PlanPhase + Contains + Satisfies records and every prereq
/// `Gates` edge — each gated through the same cycle check `apg plan update
/// phase` uses (a self-gate or transitive Gates cycle is rejected before any
/// write).
#[allow(clippy::too_many_arguments)]
fn plan_add_phase_at(
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
    plan_fqn: &str,
    n: u32,
    title: &str,
    deliverable: &str,
    prereqs: &[u32],
    satisfies: &[String],
) -> anyhow::Result<()> {
    let fqn = format!("{project}/plan.phase-{n:02}");
    crate::layers::refuse_if_present(
        records
            .iter()
            .any(|r| matches!(r, Record::PlanPhase { fqn: pf, .. } if pf == &fqn)),
        &fqn,
        &format!("apg plan update {project} phase {n}"),
        &format!("apg plan rm {project} phase {n}"),
    )?;
    let mut recs = vec![Record::PlanPhase {
        fqn: fqn.clone(),
        number: n,
        title: title.to_string(),
        deliverable: deliverable.to_string(),
        status: "pending".to_string(),
    }];
    recs.push(Record::Contains {
        from: plan_fqn.to_string(),
        to: fqn.clone(),
    });
    for g in prereqs {
        push_gate(&fqn, &format!("{project}/plan.phase-{g:02}"), records)?;
        recs.push(Record::Gates {
            from: fqn.clone(),
            to: format!("{project}/plan.phase-{g:02}"),
        });
    }
    for req in satisfies {
        // Requirements live in the layers store: FQN `requirements.requirement.<name>`
        // (no project prefix — the old `<project>/spec.<id>` vocabulary is gone).
        let req_fqn = format!("requirements.requirement.{req}");
        if !spec_has_requirement(apg_root, project, &req_fqn)? {
            anyhow::bail!(
                "satisfies target `{req}` is not a requirement of `{project}` — requirements live in apg/layers/requirements/"
            );
        }
        recs.push(Record::Satisfies {
            from: fqn.clone(),
            to: req_fqn,
        });
    }
    records.extend(recs);
    Ok(())
}

/// Core of the `task` add arm (extracted for tests): verifies the target
/// phase exists (a task under a nonexistent `plan.phase-NN` is rejected
/// before any write), refuses an existing task (no implicit upsert), validates
/// kind/tier, validates the Task→Implementation verb + target FQN(s) against
/// the scanned graph and the planned-node universe, and appends the Task +
/// Contains records.
#[allow(clippy::too_many_arguments)]
fn plan_add_task_at(
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
    phase: u32,
    k: u32,
    title: &str,
    kind: &str,
    tier: &str,
    verb: &str,
    target: &str,
    new_fqn: &str,
) -> anyhow::Result<()> {
    let fqn = format!("{project}/plan.phase-{phase:02}.task-{k}");
    let phase_fqn = format!("{project}/plan.phase-{phase:02}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::PlanPhase { fqn: pf, .. } if pf == &phase_fqn))
    {
        anyhow::bail!(
            "task under phase {phase} of `{project}` — no such phase: `{phase_fqn}` (author the phase first with `apg plan add {project} phase {phase} …`)"
        );
    }
    crate::layers::refuse_if_present(
        records
            .iter()
            .any(|r| matches!(r, Record::Task { fqn: tf, .. } if tf == &fqn)),
        &fqn,
        &format!("apg plan update {project} task {phase} {k}"),
        &format!("apg plan rm {project} task {phase} {k}"),
    )?;
    validate_task_kind_tier(kind, tier)?;
    let verb = if verb.is_empty() { "creates" } else { verb };
    let (scanned, planned) = task_verb_universes(apg_root, records)?;
    validate_task_verb(verb, target, new_fqn, &scanned, &planned)?;
    let mut recs = vec![Record::Task {
        fqn: fqn.clone(),
        title: title.to_string(),
        kind: kind.to_string(),
        tier: tier.to_string(),
        status: "pending".to_string(),
        verb: verb.to_string(),
        target: target.to_string(),
        new_fqn: new_fqn.to_string(),
    }];
    recs.push(Record::Contains {
        from: phase_fqn,
        to: fqn.clone(),
    });
    records.extend(recs);
    Ok(())
}

/// The project a plan record set belongs to — the `{project}` segment of the
/// Plan record's `{project}/plan` FQN. Names the follow-up `update`/`rm`
/// commands in [`plan_add_planned_at`]'s refusal (that core takes no separate
/// `project` argument).
fn plan_project(records: &[Record]) -> String {
    records
        .iter()
        .find_map(|r| match r {
            Record::Plan { fqn, .. } => fqn.strip_suffix("/plan").map(str::to_string),
            _ => None,
        })
        .unwrap_or_default()
}

/// Core of the `planned` add arm (extracted for tests): validates the node
/// kind, refuses a FQN that is **real scanned code** and a FQN the plan
/// already declares (no implicit upsert — the strict surface is add/update/rm),
/// then appends the `Record::PlannedNode` + its parent `Contains` edge.
///
/// The refusal reuses the task verbs' two code-reference universes
/// (`task_verb_universes`): `scanned` = every real code FQN the last scan
/// produced, `planned` = the planned-node universe (DB `status: planned`
/// nodes UNION the plan records' `Record::PlannedNode` FQNs). A
/// `CodeRefStatus::Real` FQN is refused (a plan never plans existing code);
/// a `CodeRefStatus::Pending` FQN is already declared and is refused too (the
/// re-declaration parent correction now lives in `plan update planned`). An
/// absent FQN is `Drift` (a planned node may be declared before anything
/// exists), and an `UnresolvedTarget` at the FQN is never read
/// (`code_universes` reads only the four Implementation labels — an unresolved
/// reference is not code).
fn plan_add_planned_at(
    apg_root: &Path,
    records: &mut Vec<Record>,
    node_kind: &str,
    fqn: &str,
    name: &str,
    parent: Option<&str>,
) -> anyhow::Result<()> {
    if !["module", "file", "struct", "function"].contains(&node_kind) {
        anyhow::bail!("planned node kind must be module/file/struct/function, got `{node_kind}`");
    }
    // A plan never plans code that already exists: only a FQN that resolves
    // to REAL scanned code makes the planned placeholder incoherent (the
    // scanner-replace only ever supersedes, never the reverse).
    let (scanned, planned) = task_verb_universes(apg_root, records)?;
    let status = crate::layers::classify_code_ref(fqn, &scanned, &planned);
    if status == crate::layers::CodeRefStatus::Real {
        let label = artifacts::ArtifactDb::open(apg_root)
            .ok()
            .and_then(|db| db.impl_label(fqn))
            .unwrap_or("code");
        anyhow::bail!(
            "planned node `{fqn}` already resolves to a `{label}` code node — a plan never plans existing code (plan the delta, not the present)"
        );
    }
    let project = plan_project(records);
    crate::layers::refuse_if_present(
        status == crate::layers::CodeRefStatus::Pending,
        fqn,
        &format!("apg plan update {project} planned {fqn}"),
        &format!("apg plan rm {project} planned {fqn}"),
    )?;
    let rec = Record::PlannedNode {
        fqn: fqn.to_string(),
        kind: node_kind.to_string(),
        name: name.to_string(),
        parent: parent.unwrap_or_default().to_string(),
    };
    records.push(rec);
    if let Some(parent) = parent {
        records.push(Record::Contains {
            from: parent.to_string(),
            to: fqn.to_string(),
        });
    }
    Ok(())
}

/// Validate the two-axis task classification: `kind` is the owning role
/// (orthogonal), `tier` the verification depth (a hierarchy, meaningful only
/// for `kind = test`). Mirrors the `Note.kind` validation
/// pattern in `spec_cmd`. Tasks are implementer-workable by design (`source`/
/// `test`/`gate`/`docs`) — the human's decision point is plan end, never a
/// phase task.
fn validate_task_kind_tier(kind: &str, tier: &str) -> anyhow::Result<()> {
    if !["source", "test", "gate", "docs"].contains(&kind) {
        anyhow::bail!("invalid task kind `{kind}` — one of source/test/gate/docs");
    }
    if kind == "test" && tier.is_empty() {
        anyhow::bail!("test task requires --tier (unit|int|e2e)");
    }
    if kind != "test" && !tier.is_empty() {
        anyhow::bail!("tier is only valid for test tasks");
    }
    if !tier.is_empty() && !["unit", "int", "e2e"].contains(&tier) {
        anyhow::bail!("invalid tier `{tier}` — one of unit/int/e2e");
    }
    Ok(())
}

/// The Task→Implementation verbs (SPEC §5): how a task touches the
/// Implementation tier. `creates` is the default; further verbs may reveal
/// themselves through dogfooding.
const TASK_VERBS: [&str; 5] = ["creates", "modifies", "deletes", "renames", "moves"];

/// Validate a task's Task→Implementation verb + target FQN(s) (SPEC §5)
/// against the two code-reference universes: `scanned` = every real code FQN
/// in the live graph, `planned` = the planned-node universe (a `status:
/// planned` DB node or a `Record::PlannedNode` in `.trans/plans`). The rules:
///
/// - `creates` (the default) — builds a *planned* node: the target must NOT
///   resolve in the scanned graph (it may be a planned node or still absent);
///   a creates against existing real code is refused.
/// - `modifies` / `deletes` — the target must resolve in the scanned graph;
///   an unresolvable (or still-planned) FQN is refused.
/// - `renames` / `moves` — FQN changes: the source must resolve in the
///   scanned graph and the new FQN must not collide with existing real code;
///   the pair is recorded on the task.
///
/// An empty `target` is a target-less task (the pre-verb record shape) — only
/// a `creates` may omit its target. An empty `verb` means `creates`. Pure —
/// no I/O; the caller supplies both universes.
fn validate_task_verb(
    verb: &str,
    target: &str,
    new_fqn: &str,
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let verb = if verb.is_empty() { "creates" } else { verb };
    if !TASK_VERBS.contains(&verb) {
        anyhow::bail!("invalid task verb `{verb}` — one of creates/modifies/deletes/renames/moves");
    }
    if !new_fqn.is_empty() && !matches!(verb, "renames" | "moves") {
        anyhow::bail!("--to <new-fqn> is only valid for renames/moves tasks (got `{verb}`)");
    }
    if target.is_empty() {
        if verb != "creates" {
            anyhow::bail!("{verb} task requires --fqn <fqn> (the code it touches)");
        }
        return Ok(());
    }
    let status = crate::layers::classify_code_ref(target, scanned, planned);
    match verb {
        "creates" => {
            if status == crate::layers::CodeRefStatus::Real {
                anyhow::bail!(
                    "creates target `{target}` already resolves to scanned code — a creates builds a planned node; plan the delta, not the present"
                );
            }
        }
        "modifies" | "deletes" => match status {
            crate::layers::CodeRefStatus::Real => {}
            crate::layers::CodeRefStatus::Pending => anyhow::bail!(
                "{verb} target `{target}` is a planned node, not scanned code — only a creates builds a planned node"
            ),
            crate::layers::CodeRefStatus::Drift => anyhow::bail!(
                "{verb} target `{target}` does not resolve in the scanned graph — {verb} changes existing code (a creates would plan it)"
            ),
        },
        "renames" | "moves" => {
            if new_fqn.is_empty() {
                anyhow::bail!("{verb} task requires --to <new-fqn> (the destination FQN)");
            }
            if status != crate::layers::CodeRefStatus::Real {
                anyhow::bail!(
                    "{verb} source `{target}` does not resolve in the scanned graph — {verb} changes an existing FQN"
                );
            }
            if crate::layers::classify_code_ref(new_fqn, scanned, planned)
                == crate::layers::CodeRefStatus::Real
            {
                anyhow::bail!(
                    "{verb} target `{new_fqn}` already resolves to scanned code — the new FQN must not collide with existing code"
                );
            }
        }
        _ => unreachable!("validated against TASK_VERBS"),
    }
    Ok(())
}

/// The two universes `validate_task_verb` classifies a task's target against:
/// `scanned` = every real code FQN the last scan produced (a missing DB
/// counts as no scanned graph — every target is absent), `planned` = the
/// planned-node universe: the DB's `status: planned` Implementation nodes
/// UNION the `Record::PlannedNode` FQNs declared in the plan records
/// themselves (a planned node just authored in `.trans/plans` is a planned
/// target even before its re-ingest).
fn task_verb_universes(
    apg_root: &Path,
    records: &[Record],
) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let (scanned, mut planned) = if apg_root.join(specs::TRANS).join("db.lbug").exists() {
        artifacts::code_universes(apg_root)?
    } else {
        (BTreeSet::new(), BTreeSet::new())
    };
    planned.extend(records.iter().filter_map(|r| match r {
        Record::PlannedNode { fqn, .. } => Some(fqn.clone()),
        _ => None,
    }));
    Ok((scanned, planned))
}

// `apg plan link` is retired: its bridge set-semantics live in
// `plan_update_phase_at` (`apg plan update <project> phase <n> --satisfies …
// --prereq …`), which validates the Satisfies targets and then reuses
// `link_phase_edges` unchanged.

/// Set semantics for a phase's edges: replaces the phase's own outgoing
/// Satisfies/Gates once, then adds every target. The removal is scoped to
/// outgoing edges only (never incident — incoming Gates from a later phase on
/// an earlier one would be severed) and hoisted out of the loop (in-loop
/// removal would drop every edge added by an earlier iteration, keeping only
/// the last). `reqs` are the requirement ids, `prereqs` the phase numbers.
fn link_phase_edges(
    phase_fqn: &str,
    reqs: &[String],
    prereqs: &[String],
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let project = phase_fqn
        .split('/')
        .next()
        .ok_or_else(|| anyhow::anyhow!("bad phase fqn `{phase_fqn}`"))?;
    records.retain(|r| !matches!(r, Record::Satisfies { from, .. } if from.as_str() == phase_fqn));
    for req in reqs {
        // The new-model requirement FQN: `requirements.requirement.<name>`
        // (layer-prefixed, no project segment).
        let req_fqn = format!("requirements.requirement.{req}");
        records.push(Record::Satisfies {
            from: phase_fqn.to_string(),
            to: req_fqn,
        });
    }
    records.retain(|r| !matches!(r, Record::Gates { from, .. } if from.as_str() == phase_fqn));
    for g in prereqs {
        let g = g
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
        let target = format!("{project}/plan.phase-{g:02}");
        push_gate(phase_fqn, &target, records)?;
        records.push(Record::Gates {
            from: phase_fqn.to_string(),
            to: target,
        });
    }
    Ok(())
}

/// Validates one `Gates` edge `from → to` against the plan's current records:
/// rejects a self-gate and any transitive Gates cycle (the same
/// `cycle_closing_path` machinery `apg spec add phase` / `apg plan update
/// phase` use) before the edge is ever accumulated into the records — so the
/// JSONL/DB write-through never runs on a cycle. `records` must already have
/// the phase's stale outgoing gates removed (the phase add/update callers do
/// this).
fn push_gate(from: &str, to: &str, records: &[Record]) -> anyhow::Result<()> {
    let project = from
        .split('/')
        .next()
        .ok_or_else(|| anyhow::anyhow!("bad phase fqn `{from}`"))?;
    if let Some(path) = artifacts::cycle_closing_path(records, from, to, |r| match r {
        Record::Gates { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    }) {
        let short: Vec<String> = path
            .iter()
            .map(|f| {
                f.strip_prefix(&format!("{project}/plan.phase-"))
                    .unwrap_or(f)
                    .to_string()
            })
            .collect();
        let to_short = to
            .strip_prefix(&format!("{project}/plan.phase-"))
            .unwrap_or(to)
            .to_string();
        anyhow::bail!(
            "adding gate {} → {} would create a cycle: {}",
            from.strip_prefix(&format!("{project}/plan.phase-"))
                .unwrap_or(from),
            to_short,
            short.join(" → ")
        );
    }
    Ok(())
}

/// `apg plan done <project> <task-fqn>` (PHASE_03) — the implementer's
/// assertion. Marks a task done with NO promotion and NO code-graph
/// verification: the plan's planned nodes stay declared until the
/// apply act (PlanCompletion-SPEC.md), whose coherence gate verifies every
/// planned node is realized against the merged graph before applying.
/// `apg plan undone` remains the reversal.
fn plan_done(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan done <project> <task-fqn>");
    };
    let apg_root = require_apg_root()?;
    plan_done_at(&apg_root, project, task_fqn)
}

/// Core of `plan_done`: flips a task's status to `done` in the transient plan
/// JSONL (assertion only — no promotion, no graph verification).
fn plan_done_at(apg_root: &Path, project: &str, task_fqn: &str) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let mut found = false;
    for r in &mut records {
        match r {
            Record::Task { fqn, status, .. } if fqn == task_fqn => {
                *status = "done".to_string();
                found = true;
            }
            _ => {}
        }
    }
    if !found {
        anyhow::bail!("task `{task_fqn}` not found in plan `{project}`");
    }
    write_through(apg_root, project, &records)?;
    println!("Task done (assertion): {task_fqn}");
    Ok(())
}

/// `apg plan undone <project> <task-fqn>` — a checklist correction; does not
/// recreate retired planned nodes (the code is already in the present).
fn plan_undone(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan undone <project> <task-fqn>");
    };
    let apg_root = require_apg_root()?;
    plan_undone_at(&apg_root, project, task_fqn)
}

/// Core of `plan_undone`: flips a task's status back to `pending`.
fn plan_undone_at(apg_root: &Path, project: &str, task_fqn: &str) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let mut found = false;
    for r in &mut records {
        match r {
            Record::Task { fqn, status, .. } if fqn == task_fqn => {
                *status = "pending".to_string();
                found = true;
            }
            _ => {}
        }
    }
    if !found {
        anyhow::bail!("task `{task_fqn}` not found in plan `{project}`");
    }
    write_through(apg_root, project, &records)?;
    println!("Task undone: {task_fqn}");
    Ok(())
}

/// `apg plan note <project> <task-fqn> --body … [--kind …]` (PHASE_03) — an
/// implementer attaches a note to a task for a concern or deviation that arose
/// during execution (PlanExecution-SPEC.md). `Task` is an allowable `Details`
/// target. The note serializes into the transient plan JSONL (branch-local,
/// survives until apply), not the durable spec JSONL — task notes are
/// execution context, surfaced to the human at the apply gate.
fn plan_note(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan note <project> <task-fqn> --body …");
    };
    let Some(body) = p.get("body") else {
        anyhow::bail!("plan note requires --body");
    };
    let kind = p.get("kind").unwrap_or_else(|| "note".to_string());
    if kind != "note" {
        anyhow::bail!(
            "plan note kind must be `note` (task notes are implementation annotations) — got `{kind}`"
        );
    }
    let apg_root = require_apg_root()?;
    plan_note_at(&apg_root, project, task_fqn, &body, &kind)
}

/// Core of `plan_note`: appends a `Note` + `Details` edge to the transient plan
/// JSONL (branch-local). A task note documents a concern/deivation during
/// implementation; it survives with the plan until apply.
fn plan_note_at(
    apg_root: &Path,
    project: &str,
    task_fqn: &str,
    body: &str,
    kind: &str,
) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    if !records
        .iter()
        .any(|r| matches!(r, Record::Task { fqn, .. } if fqn == task_fqn))
    {
        anyhow::bail!("task `{task_fqn}` not found in plan `{project}`");
    }
    let n = records
        .iter()
        .filter_map(|r| match r {
            Record::Note { fqn, .. } if fqn.starts_with(&format!("{project}/plan.note-")) => fqn
                .rsplit_once("-")
                .and_then(|(_, s)| s.parse::<u64>().ok()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    let fqn = format!("{project}/plan.note-{n}");
    records.push(Record::Note {
        fqn: fqn.clone(),
        body: body.to_string(),
        kind: kind.to_string(),
    });
    records.push(Record::Details {
        from: fqn.clone(),
        to: task_fqn.to_string(),
    });
    write_through(apg_root, project, &records)?;
    println!("Added task note `{fqn}` on `{task_fqn}`");
    Ok(())
}

/// `apg plan complete <project> <phase-n>` (PHASE_03) — a milestone only.
/// Requires every phase task `done` and no unresolved feedback on the phase or
/// its tasks; then marks the phase complete. NO `Implements` materialization
/// and NO plan retirement: the plan survives until the apply act
/// (PlanCompletion-SPEC.md), which materializes all delivery records in one act
/// (coherence gate → merge → rebuild). Non-code deliverables are recorded
/// consciously at apply time.
fn plan_complete(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(phase)) = (
        p.positional.first(),
        p.positional.get(1).and_then(|s| s.parse::<u32>().ok()),
    ) else {
        anyhow::bail!("usage: apg plan complete <project> <phase-n>");
    };
    let apg_root = require_apg_root()?;
    plan_complete_at(&apg_root, project, phase)
}

/// Core of `plan_complete` — milestone only. The gate (all tasks done + no
/// unresolved feedback) is enforced; then the phase's durable `status` flips
/// `pending → done` in the plan JSONL (the milestone record — distinguishable
/// from an uncompleted phase whose tasks are all done and feedback resolved).
/// NO `Implements` materialization and NO plan retirement.
fn plan_complete_at(apg_root: &Path, project: &str, phase: u32) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let phase_fqn = format!("{project}/plan.phase-{phase:02}");

    let tasks: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Contains { from, to } if from == &phase_fqn => Some(to.clone()),
            _ => None,
        })
        .filter(|t| {
            records
                .iter()
                .any(|r| matches!(r, Record::Task { fqn, .. } if fqn == t))
        })
        .collect();

    let mut not_done = Vec::new();
    for t in &tasks {
        let status = records
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == t => Some(status.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if status != "done" {
            not_done.push(t.clone());
        }
    }
    if !not_done.is_empty() {
        anyhow::bail!(
            "phase {phase} is not complete: tasks not done: {} — every task must be `done` first",
            not_done.join(", ")
        );
    }

    // R27: no unresolved Feedback may review the phase or its tasks.
    let targets = std::iter::once(&phase_fqn).chain(tasks.iter());
    let unresolved: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Reviews { from, to } if targets.clone().any(|t| t == to) => Some(from.clone()),
            _ => None,
        })
        .filter(|f| {
            records.iter().any(|r| {
                matches!(r, Record::Feedback { fqn, status, .. } if fqn == f && status != "resolved")
            })
        })
        .collect();
    if !unresolved.is_empty() {
        anyhow::bail!(
            "phase {phase} has unresolved review feedback: {} — resolve every `Feedback` before completing (R27)",
            unresolved.join(", ")
        );
    }

    // Milestone-only: the phase's built code and Satisfies targets are not
    // materialized here. The plan (with the phase now complete) survives until
    // the apply act. The durable milestone IS recorded: the phase's `status`
    // flips to `done`.
    let mut marked = false;
    for r in &mut records {
        if let Record::PlanPhase {
            fqn: _,
            status,
            number: n,
            ..
        } = r
            && *n == phase
        {
            *status = "done".to_string();
            marked = true;
        }
    }
    debug_assert!(marked, "phase {phase} missing from plan `{project}`");
    write_through(apg_root, project, &records)?;
    println!("Completed phase {phase} of plan {project} (milestone — plan survives until apply)");
    Ok(())
}

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
fn plan_verify(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan verify <project>");
    };
    let apg_root = require_apg_root()?;
    plan_verify_at(&apg_root, project)
}

/// Core of `plan verify` — the coherence gate. Returns the merge handoff
/// message on green, or errors listing every blocker (unrealized planned
/// nodes, unresolved feedback, coverage gaps). Guarded: refuses outside the
/// project context and against a stale branch DB (a verdict is only
/// meaningful against the branch's graph — R5).
pub(crate) fn plan_verify_at(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    crate::git::require_membership(apg_root, project)?;
    if crate::git::is_stale(apg_root) {
        anyhow::bail!(
            "cannot verify `{project}`: the branch graph is stale — run `apg scan` inside the project worktree first (a verdict is only meaningful against a fresh branch graph)"
        );
    }
    artifacts::acquire_spec_lock(apg_root)?;
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
        let realized = db.impl_label(fqn).is_some() && !db.is_planned(fqn);
        if !realized {
            blocked.push(format!(
                "planned {kind} node `{fqn}` is not realized — a scan must find real code at its FQN before verify (missing or dangling code blocks verify)"
            ));
        }
    }

    // 2. All feedback resolved — plan/phase/task scope.
    let targets: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Plan { fqn, .. } | Record::PlanPhase { fqn, .. } | Record::Task { fqn, .. } => {
                Some(fqn.clone())
            }
            _ => None,
        })
        .collect();
    let unresolved: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Reviews { from, to } if targets.contains(to) => Some(from.clone()),
            _ => None,
        })
        .filter(|f| {
            records.iter().any(|r| {
                matches!(r, Record::Feedback { fqn, status, .. } if fqn == f && status != "resolved")
            })
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
        "Merge: `apg project merge {project}` from the main checkout (verify gate → merge → main rebuild; push/tag remain human)."
    );
    Ok(())
}

/// One uncovered `implemented-by` code FQN with the solution node that claims
/// it (SPEC §5 derived coverage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageGap {
    /// The owning solution node FQN (`solution.system.<name>` /
    /// `solution.container.<name>` / `solution.component.<name>`).
    pub solution: String,
    /// The `implemented-by` code FQN no plan task touches.
    pub fqn: String,
}

/// The derived-coverage verdict (SPEC §5): whether every in-scope solution
/// node's `implemented-by` FQN is touched by at least one plan task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageReport {
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
pub(crate) fn coverage_check(
    records: &[Record],
    nodes: &[crate::layers::NodeFile],
    satisfied: &BTreeSet<String>,
    branch_added: &BTreeSet<String>,
) -> CoverageReport {
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
                touched.insert(target.as_str());
            }
            if matches!(verb.as_str(), "renames" | "moves") && !new_fqn.is_empty() {
                touched.insert(new_fqn.as_str());
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
            if !touched.contains(fqn) {
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

/// `apg plan render <project> [--out <path>|-]` — PLAN.md-style markdown with
/// the strategy, the phase table, and each phase's tasks as a checkable list.
fn plan_render(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan render <project> [--out <path>|-]");
    };
    let apg_root = require_apg_root()?;
    let records = load_plan(&apg_root, project)?;
    let Some((title, strategy)) = records.iter().find_map(|r| match r {
        Record::Plan {
            title, strategy, ..
        } => Some((title, strategy)),
        _ => None,
    }) else {
        anyhow::bail!("plan `{project}` has no plan node");
    };
    let mut out = String::new();
    out.push_str(&format!("# Plan — {title}\n\n"));
    if !strategy.is_empty() {
        out.push_str(&format!("## Strategy\n{strategy}\n\n"));
    }
    let mut phases: Vec<u32> = records
        .iter()
        .filter_map(|r| match r {
            Record::PlanPhase { number, .. } => Some(*number),
            _ => None,
        })
        .collect();
    phases.sort_unstable();
    out.push_str("## Phases\n\n| Phase | Status | Deliverable | Satisfies | Prereq |\n|---|---|---|---|---|\n");
    for n in &phases {
        let pfqn = format!("{project}/plan.phase-{n:02}");
        let (deliverable, status) = records
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase {
                    fqn,
                    deliverable,
                    status,
                    ..
                } if fqn == &pfqn => Some((deliverable.clone(), status.clone())),
                _ => None,
            })
            .unwrap_or_default();
        let satisfies: Vec<String> = records
            .iter()
            .filter_map(|e| match e {
                Record::Satisfies { from, to } if from == &pfqn => Some(short_id(to, project)),
                _ => None,
            })
            .collect();
        let prereqs: Vec<String> = records
            .iter()
            .filter_map(|e| match e {
                Record::Gates { from, to } if from == &pfqn => Some(short_id(to, project)),
                _ => None,
            })
            .collect();
        out.push_str(&format!(
            "| {n} | {status} | {deliverable} | {} | {} |\n",
            satisfies.join(", "),
            prereqs.join(", ")
        ));
    }
    out.push('\n');
    for n in &phases {
        let pfqn = format!("{project}/plan.phase-{n:02}");
        let pstatus = records
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase { fqn, status, .. } if fqn == &pfqn => Some(status.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let marker = if pstatus == "done" { " (complete)" } else { "" };
        out.push_str(&format!("## Phase {n}{marker}\n\n"));
        out.push_str(&render_phase_tasks(&records, &pfqn));
        out.push('\n');
    }
    match p.get("out").as_deref() {
        Some("-") => print!("{out}"),
        Some(path) => {
            std::fs::write(path, &out)?;
            println!("Rendered plan {project} to {path}");
        }
        None => {
            let out_path = apg_root
                .join(specs::TRANS)
                .join("plans")
                .join(format!("{project}.md"));
            std::fs::write(&out_path, &out)?;
            println!("Rendered plan {project} to {}", out_path.display());
        }
    }
    Ok(())
}

/// The markdown task list for one phase, grouped by owning kind. Verification
/// depth renders as `test/<tier>` for test tasks, plain kind otherwise.
fn render_phase_tasks(records: &[Record], pfqn: &str) -> String {
    let tasks: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Contains { from, to } if from == pfqn => Some(to.clone()),
            _ => None,
        })
        .filter(|t| {
            records
                .iter()
                .any(|r| matches!(r, Record::Task { fqn, .. } if fqn == t))
        })
        .collect();
    const KIND_ORDER: [&str; 4] = ["source", "test", "gate", "docs"];
    type TaskLine = (String, String, String, String);
    let mut by_kind: Vec<(String, Vec<TaskLine>)> = Vec::new();
    for t in tasks {
        let (title, kind, tier, status) = records
            .iter()
            .find_map(|r| match r {
                Record::Task {
                    fqn,
                    title,
                    kind,
                    tier,
                    status,
                    ..
                } if fqn == &t => Some((title.clone(), kind.clone(), tier.clone(), status.clone())),
                _ => None,
            })
            .unwrap_or_default();
        let depth = if kind == "test" && !tier.is_empty() {
            format!("test/{tier}")
        } else {
            kind.clone()
        };
        match by_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, items)) => items.push((t, title, depth, status)),
            None => by_kind.push((kind, vec![(t, title, depth, status)])),
        }
    }
    by_kind.sort_by_key(|(k, _)| KIND_ORDER.iter().position(|x| x == k).unwrap_or(usize::MAX));
    let mut out = String::new();
    for (kind, items) in by_kind {
        out.push_str(&format!("**{kind}**\n"));
        for (t, title, depth, status) in items {
            let check = if status == "done" { "[x]" } else { "[ ]" };
            out.push_str(&format!("- {check} `{t}` — {title} ({depth})\n"));
        }
    }
    out
}

fn short_id(fqn: &str, project: &str) -> String {
    let prefix = format!("{project}/");
    fqn.strip_prefix(&prefix).unwrap_or(fqn).to_string()
}

/// Whether the layers store has a requirement with this FQN
/// (`requirements.requirement.<name>` — one node file per requirement under
/// `apg/layers/requirements/requirement/`, no project prefix; the legacy spec
/// JSONL is gone).
fn spec_has_requirement(apg_root: &Path, _project: &str, req_fqn: &str) -> anyhow::Result<bool> {
    Ok(crate::layers::read_existing_nodes(apg_root)?
        .iter()
        .any(|n| {
            n.layer == "requirements"
                && n.node_type == "requirement"
                && crate::layers::fqn(crate::layers::Layer::Requirements, &n.node_type, &n.name)
                    == req_fqn
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::load;
    use crate::testutil::{self, Repo};
    use lbug::{Connection, Database};

    /// A temp repo with a real project context for `foo` (R4 — non-git
    /// fixtures are gone): the worktree on branch `foo` carries a real DB
    /// (Module/File/Struct + an UnresolvedTarget) and a fresh scan_meta.
    /// Returns `(wt_apg_root, repo, wt_root)`. Tags are module-prefixed so
    /// parallel tests in other modules never collide on a temp dir.
    fn fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
        let repo = Repo::new(&format!("plan-{name}"));
        let wt = repo.start_project("foo");
        db_at(&wt);
        testutil::write_scan_meta(
            &wt.join(specs::LAYOUT),
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
        (wt.join(specs::LAYOUT), repo, wt)
    }

    /// Builds a real DB + load files under `dir/apg` (used by `fixture`).
    fn db_at(dir: &Path) {
        let mut g = Graph::default();
        g.nodes.insert(
            "github.com/x/y".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/abs/store.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: "/abs/store.go".into(),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 100,
                }),
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        g.nodes.insert(
            "github.com/x/y.Store".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: "/abs/store.go".into(),
                    start: 0,
                    end: 40,
                    start_line: 1,
                    end_line: 40,
                }),
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("github.com/x/y".to_string(), "/abs/store.go".to_string()));
        g.contains.insert((
            "/abs/store.go".to_string(),
            "github.com/x/y.Store".to_string(),
        ));

        // An unresolved reference — real-code checks must NOT treat it as
        // implementation code (the apply gate's realization check, REVIEW).
        g.nodes.insert(
            "github.com/x/y.Missing".to_string(),
            Node {
                kind: NodeKind::UnresolvedTarget,
                category: Some("external".to_string()),
                ..Node::default()
            },
        );

        let ldir = dir.join("apg").join(specs::TRANS).join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        load::build_load_files(&g, &ldir).unwrap();
        let db = Database::new(
            dir.join("apg").join(specs::TRANS).join("db.lbug"),
            Default::default(),
        )
        .unwrap();
        let conn = Connection::new(&db).unwrap();
        load::create_schema(&conn).unwrap();
        load::copy_from(&conn, &ldir).unwrap();
        drop(conn);
        drop(db);
    }

    /// Writes a plan JSONL for `foo` with one phase containing one task.
    fn write_plan(apg_root: &Path) -> PathBuf {
        let path = specs::plan_jsonl_path(apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ];
        specs::write_jsonl(&path, &records).unwrap();
        path
    }

    /// Writes one requirement node file (`requirements.requirement.<name>`)
    /// under the layers store so `--satisfies <name>` resolves in the update
    /// phase unit tests.
    fn write_requirement(apg_root: &Path, name: &str) {
        let path = crate::layers::node_file_path(
            apg_root,
            crate::layers::Layer::Requirements,
            "requirement",
            name,
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            r#"{{"name":"{name}","type":"requirement","layer":"requirements","body":"x","properties":{{}},"out":[],"in":[]}}"#
        );
        std::fs::write(&path, body).unwrap();
    }

    #[test]
    fn plan_done_is_assertion_only_and_undone_reverses() {
        let (apg_root, repo, _wt) = fixture("assertion-done");
        let _path = write_plan(&apg_root);

        // Mark the task done.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == "foo/plan.phase-01.task-1" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "done");

        // Assertion-only: no promotion side effects — the plan file still
        // carries only the task's status flip.

        // undone reverses.
        plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == "foo/plan.phase-01.task-1" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "pending");

        testutil::remove(&repo);
    }

    #[test]
    fn plan_complete_is_milestone_only_with_gate_and_no_retirement() {
        let (apg_root, repo, _wt) = fixture("milestone-complete");

        // Pending task → complete is rejected by the gate.
        let _path = write_plan(&apg_root);
        assert!(plan_complete_at(&apg_root, "foo", 1).is_err());

        // Mark the task done, add resolved feedback to prove the gate accepts.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();

        // Milestone-only: the plan file survives (no retirement) — the phase's
        // durable `status` flipped to done, nothing materialized elsewhere.
        assert!(specs::plan_jsonl_path(&apg_root, "foo").exists());
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(r, Record::PlanPhase { .. })));

        testutil::remove(&repo);
    }

    #[test]
    fn plan_note_roundtrip_into_plan_jsonl() {
        let (apg_root, repo, _wt) = fixture("task-note");
        let _path = write_plan(&apg_root);

        plan_note_at(
            &apg_root,
            "foo",
            "foo/plan.phase-01.task-1",
            "deviation: the Store uses a byte slice, not a file handle",
            "note",
        )
        .unwrap();

        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Note { fqn, body, .. }
                if fqn == "foo/plan.note-1"
                    && body.contains("deviation")
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Details { from, to }
                if from == "foo/plan.note-1" && to == "foo/plan.phase-01.task-1"
        )));

        // The note + Details edge land in the live DB.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'foo/plan.note-1'})-[:Details]->(:Task) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "note→task Details edge: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    fn apply_gate_rejects_unrealized_planned_node() {
        let (apg_root, repo, _wt) = fixture("apply-gate");

        // A plan whose task Builds a planned node whose FQN has NO real code
        // in the graph (the code was claimed-done but is missing).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // The gate rejects: the planned node does not resolve to real code.
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("coherence gate blocked"), "{err}");
        assert!(err.to_string().contains("github.com/x/y.Gateway"), "{err}");

        testutil::remove(&repo);
    }

    #[test]
    fn apply_gate_passes_when_planned_node_realized_and_feedback_resolved() {
        let (apg_root, repo, _wt) = fixture("apply-gate-ok");

        // The planned node's FQN resolves to the fixture's real Struct.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "resolved".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // Green: every planned node is realized, all feedback resolved.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        testutil::remove(&repo);
    }

    #[test]
    fn apply_gate_checks_every_planned_node_not_just_builds_targets() {
        let (apg_root, repo, _wt) = fixture("apply-gate-all-planned");

        // Two planned nodes, only one built: the gate must block on the
        // unrealized one even though every Builds edge's target resolves (the
        // per-plan realization check, not a Builds-target check).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("github.com/x/y.Gateway"), "{err}");

        testutil::remove(&repo);
    }

    #[test]
    fn apply_gate_rejects_unresolved_feedback() {
        let (apg_root, repo, _wt) = fixture("apply-feedback");

        // Planned node realizes, but an open Feedback reviews the phase.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(
            err.to_string().contains("unresolved review feedback"),
            "{err}"
        );

        testutil::remove(&repo);
    }

    #[test]
    fn link_phase_edges_keeps_all_satisfies_and_preserves_other_phases() {
        // Regression: the removal ran inside the loop (only the last edge
        // survived) and removed edges incident to the phase (incoming Gates
        // from a later phase were severed).
        let mut records = vec![
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
            // A later phase gating this one: incoming edge, must survive.
            Record::Gates {
                from: "foo/plan.phase-02".into(),
                to: "foo/plan.phase-01".into(),
            },
        ];
        link_phase_edges(
            "foo/plan.phase-01",
            &["R1".into(), "R2".into(), "R3".into()],
            &["3".into()],
            &mut records,
        )
        .unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            satisfies,
            vec![
                "requirements.requirement.R1",
                "requirements.requirement.R2",
                "requirements.requirement.R3"
            ]
        );
        // Outgoing Gates set; the incoming phase-02 → phase-01 gate survives.
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-03"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-02" && to == "foo/plan.phase-01"
        )));
        // Linking phase-01 to gate phase-02 would close the incoming
        // phase-02 → phase-01 gate into a cycle — rejected.
        assert!(link_phase_edges("foo/plan.phase-01", &[], &["2".into()], &mut records,).is_err());
        // Re-linking replaces, never duplicates.
        link_phase_edges("foo/plan.phase-01", &["R9".into()], &[], &mut records).unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(satisfies, vec!["requirements.requirement.R9"]);
    }

    /// Unit: `plan_update_phase_at` updates a phase in place — its task
    /// Contains edges survive — while `--satisfies`/`--prereq` fold `link`'s
    /// set-semantics (a passed set replaces the phase's own outgoing bridge
    /// edges, an omitted dimension is preserved). A bogus `--satisfies` is
    /// refused before any mutation, and an absent target is refused for all
    /// three update cores with the record set left byte-identical.
    #[test]
    fn plan_update_phase_at_preserves_tasks_validates_satisfies_and_sets_bridge_edges() {
        let (apg_root, repo, _wt) = fixture("update-phase");
        write_requirement(&apg_root, "R1");
        write_requirement(&apg_root, "R2");
        write_requirement(&apg_root, "R3");

        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D1".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D2".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
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
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "requirements.requirement.R1".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
            // An unrelated phase's own Satisfies must never be touched.
            Record::Satisfies {
                from: "foo/plan.phase-02".into(),
                to: "requirements.requirement.R3".into(),
            },
        ];
        let snapshot = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };

        // Title/deliverable only: the task Contains edge and both bridge edges
        // survive; phase-02's Satisfies is untouched.
        plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            Some("P1b"),
            Some("D1b"),
            None,
            None,
        )
        .unwrap();
        let phase = records
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase {
                    fqn,
                    title,
                    deliverable,
                    ..
                } if fqn == "foo/plan.phase-01" => Some((title.as_str(), deliverable.as_str())),
                _ => None,
            })
            .unwrap();
        assert_eq!(phase, ("P1b", "D1b"));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-01" && to == "requirements.requirement.R1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-02"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-02" && to == "requirements.requirement.R3"
        )));

        // A passed set replaces only the phase's own outgoing bridge edges;
        // the task Contains edge and the unrelated phase's edge survive.
        let satisfies = vec!["R2".to_string()];
        let prereqs = vec!["02".to_string()];
        plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            None,
            None,
            Some(&prereqs),
            Some(&satisfies),
        )
        .unwrap();
        let s: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(s, vec!["requirements.requirement.R2"]);
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-02" && to == "requirements.requirement.R3"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));

        // A bogus --satisfies is refused before any mutation (the passed title
        // must not land).
        let before = snapshot(&records);
        let err = plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            Some("NOPE"),
            None,
            None,
            Some(&["ghost".to_string()]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a requirement"), "{err}");
        assert_eq!(
            snapshot(&records),
            before,
            "a refused satisfies must not mutate the records"
        );

        // Refuse-absent for all three update cores: no bytes change.
        let before = snapshot(&records);
        assert!(
            plan_update_phase_at(
                &apg_root,
                "foo",
                &mut records,
                9,
                Some("X"),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            plan_update_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                9,
                Some("X"),
                None,
                None,
                None,
                None,
                None,
            )
            .is_err()
        );
        assert!(
            plan_update_planned_at(&apg_root, &mut records, "/nope.ts", None, Some("X"), None)
                .is_err()
        );
        assert_eq!(
            snapshot(&records),
            before,
            "every absent-target refusal must leave the store byte-identical"
        );

        testutil::remove(&repo);
    }

    /// Unit: `plan_update_task_at` preserves `status` + incident `Reviews` and
    /// re-validates kind/tier (`validate_task_kind_tier`) and verb/target
    /// (`validate_task_verb`). Every invalid classification, verb, or
    /// creates-over-real-code is refused before any write.
    #[test]
    fn plan_update_task_at_preserves_status_reviews_and_revalidates() {
        let (apg_root, repo, _wt) = fixture("update-task");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
                verb: "modifies".into(),
                target: "github.com/x/y.Store".into(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "b".into(),
                status: "open".into(),
                disposition: String::new(),
            },
        ];
        let snapshot = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };

        // A title-only update preserves `status` + the incident Reviews edge.
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            Some("T2"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let task = records
            .iter()
            .find_map(|r| match r {
                Record::Task {
                    fqn, title, status, ..
                } if fqn == "foo/plan.phase-01.task-1" => Some((title.as_str(), status.as_str())),
                _ => None,
            })
            .unwrap();
        assert_eq!(task, ("T2", "done"));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Reviews { from, to }
                if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
        )));

        // A re-validated classification change (source -> test/unit) lands.
        plan_update_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            None,
            Some("test"),
            Some("unit"),
            None,
            None,
            None,
        )
        .unwrap();

        // Negatives — each refused before any write.
        let before = snapshot(&records);
        let mut try_case = |label: &str,
                            kind: Option<&str>,
                            tier: Option<&str>,
                            verb: Option<&str>,
                            target: Option<&str>| {
            let err = plan_update_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                None,
                kind,
                tier,
                verb,
                target,
                None,
            )
            .unwrap_err();
            assert!(!err.to_string().is_empty(), "{label}");
            assert_eq!(snapshot(&records), before, "{label}: no partial write");
        };
        try_case("invalid kind", Some("qa"), None, None, None);
        try_case("test needs a tier", Some("test"), Some(""), None, None);
        try_case(
            "tier only for test",
            Some("source"),
            Some("unit"),
            None,
            None,
        );
        try_case("invalid verb", None, None, Some("explodes"), None);
        try_case(
            "creates over real code",
            None,
            None,
            Some("creates"),
            Some("github.com/x/y.Store"),
        );

        // The valid classification landed and status/Reviews still survive.
        let status = records
            .iter()
            .find_map(|r| match r {
                Record::Task { fqn, status, .. } if fqn == "foo/plan.phase-01.task-1" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "done");

        testutil::remove(&repo);
    }

    /// Unit: `plan_update_planned_at` repoints the parent `Contains` edge while
    /// preserving every other incident edge.
    #[test]
    fn plan_update_planned_at_repoints_parent_and_preserves_other_edges() {
        let (apg_root, repo, _wt) = fixture("update-planned");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
            },
            Record::PlannedNode {
                fqn: "/todo/app.ts".into(),
                kind: "file".into(),
                name: "app.ts".into(),
                parent: "github.com/x/y.Missing".into(),
            },
            Record::Contains {
                from: "github.com/x/y.Missing".into(),
                to: "/todo/app.ts".into(),
            },
            // A non-Contains incident edge must survive the repoint.
            Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "/todo/app.ts".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
        ];

        plan_update_planned_at(
            &apg_root,
            &mut records,
            "/todo/app.ts",
            Some("function"),
            Some("app2.ts"),
            Some("github.com/x/y"),
        )
        .unwrap();

        let planned = records
            .iter()
            .find_map(|r| match r {
                Record::PlannedNode {
                    fqn,
                    kind,
                    name,
                    parent,
                } if fqn == "/todo/app.ts" => Some((kind.as_str(), name.as_str(), parent.as_str())),
                _ => None,
            })
            .unwrap();
        assert_eq!(planned, ("function", "app2.ts", "github.com/x/y"));
        let contains: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Contains { from, to } if to == "/todo/app.ts" => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            contains,
            vec!["github.com/x/y"],
            "exactly the repointed parent Contains edge"
        );
        assert!(
            records.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "/todo/app.ts"
            )),
            "the non-Contains incident edge survives"
        );
        assert!(
            records.iter().any(|r| matches!(
                r,
                Record::Gates { from, to }
                    if from == "foo/plan.phase-01" && to == "foo/plan.phase-02"
            )),
            "unrelated edges are untouched"
        );

        // An absent planned node is refused (no record count change).
        let before = records.len();
        assert!(
            plan_update_planned_at(&apg_root, &mut records, "/nope.ts", None, Some("x"), None)
                .is_err()
        );
        assert_eq!(records.len(), before);

        testutil::remove(&repo);
    }

    #[test]
    fn task_kind_tier_validation() {
        // Default kind is source.
        assert!(validate_task_kind_tier("source", "").is_ok());
        // All four implementer-workable kinds accepted.
        for k in ["source", "test", "gate", "docs"] {
            let tier = if k == "test" { "unit" } else { "" };
            assert!(validate_task_kind_tier(k, tier).is_ok(), "kind {k}");
        }
        // Unknown kinds rejected — the retired `human` kind included.
        assert!(validate_task_kind_tier("qa", "").is_err());
        assert!(validate_task_kind_tier("human", "").is_err());
        // tier required for test, rejected for non-test.
        assert!(validate_task_kind_tier("test", "").is_err());
        assert!(validate_task_kind_tier("source", "unit").is_err());
        // Unknown tier rejected.
        assert!(validate_task_kind_tier("test", "smoke").is_err());
        // All three tiers accepted for test.
        for t in ["unit", "int", "e2e"] {
            assert!(validate_task_kind_tier("test", t).is_ok(), "tier {t}");
        }
    }

    #[test]
    fn task_verb_creates_accepts_absent_and_planned_targets() {
        let (apg_root, repo, _wt) = fixture("verb-creates");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ];

        // A creates against an FQN absent from the scanned graph — and not yet
        // declared anywhere — is accepted (the plan may declare the planned
        // node later; the verb's rule is only "not existing real code").
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap();
        match records
            .iter()
            .find(|r| matches!(r, Record::Task { fqn, .. } if fqn == "foo/plan.phase-01.task-1"))
        {
            Some(Record::Task {
                verb,
                target,
                new_fqn,
                ..
            }) => {
                assert_eq!(verb, "creates");
                assert_eq!(target, "github.com/x/y.Gateway");
                assert!(new_fqn.is_empty());
            }
            other => panic!("expected the creates task, got {other:?}"),
        }

        // A creates against a DB `status: planned` node is accepted: the
        // planned-node universe is the DB's planned nodes UNION the
        // `Record::PlannedNode` records — here the planned FQN was written
        // through (re-ingested `status: planned`) while the records handed to
        // the add carry no PlannedNode record, so the DB half must count.
        let mut with_planned = records.clone();
        with_planned.push(Record::PlannedNode {
            fqn: "github.com/x/y.Gateway".to_string(),
            kind: "struct".to_string(),
            name: "Gateway".to_string(),
            parent: String::new(),
        });
        write_through(&apg_root, "foo", &with_planned).unwrap();
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            2,
            "T2",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-2"
                    && verb == "creates"
                    && target == "github.com/x/y.Gateway"
        )));

        testutil::remove(&repo);
    }

    #[test]
    fn task_verb_creates_refuses_real_scanned_code() {
        let (apg_root, repo, _wt) = fixture("verb-creates-real");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ];

        // `github.com/x/y.Store` is a real Struct in the fixture DB — a
        // creates against it is refused before any Task record lands.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Store",
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("already resolves to scanned code"),
            "{err}"
        );
        assert!(
            records.iter().all(
                |r| !matches!(r, Record::Task { fqn, .. } if fqn == "foo/plan.phase-01.task-1")
            ),
            "a refused creates must not leave a partial Task record"
        );

        testutil::remove(&repo);
    }

    /// Strict-surface repro: the plan's own `status: planned` placeholder must
    /// NOT be silently re-declared. Re-declaring the SAME FQN is refused
    /// (naming `apg plan update`/`apg plan rm`); the parent correction the old
    /// upsert performed now goes through `plan_update_planned_at`, which
    /// repoints the parent `Contains` edge in place.
    #[test]
    fn plan_add_planned_redeclares_own_placeholder_with_corrected_parent() {
        let (apg_root, repo, _wt) = fixture("planned-redeclare");
        let plan_path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();

        // First declaration: a planned File whose parent is an UnresolvedTarget
        // (not code — and not a valid Contains pair, so no DB edge is ever
        // merged for it). The write-through re-ingests the placeholder as a
        // `status: planned` File row.
        plan_add_planned_at(
            &apg_root,
            &mut records,
            "file",
            "/todo/app.ts",
            "app.ts",
            Some("github.com/x/y.Missing"),
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // Re-declaring the SAME FQN is refused (no implicit upsert), naming the
        // update/rm follow-ups, and the on-disk store is byte-identical.
        let before = std::fs::read_to_string(&plan_path).unwrap();
        let err = plan_add_planned_at(
            &apg_root,
            &mut records,
            "file",
            "/todo/app.ts",
            "app.ts",
            Some("github.com/x/y"),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(
            msg.contains("apg plan update foo planned /todo/app.ts"),
            "{msg}"
        );
        assert!(
            msg.contains("apg plan rm foo planned /todo/app.ts"),
            "{msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before,
            "a refused planned re-declaration must not touch the plan store"
        );

        // The parent correction goes through the update core, which repoints
        // the parent Contains edge in place (set-semantics for parent).
        plan_update_planned_at(
            &apg_root,
            &mut records,
            "/todo/app.ts",
            None,
            None,
            Some("github.com/x/y"),
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // Plan records: one PlannedNode at the FQN with the corrected parent,
        // and exactly one Contains record to it (the old parent edge was
        // repointed, not duplicated).
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let parents: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::PlannedNode { fqn, parent, .. } if fqn == "/todo/app.ts" => {
                    Some(parent.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            parents,
            vec!["github.com/x/y"],
            "the planned record carries the corrected parent"
        );
        let contains: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::Contains { from, to } if to == "/todo/app.ts" => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            contains,
            vec!["github.com/x/y"],
            "exactly the corrected parent Contains edge"
        );

        // DB: the placeholder is still a `status: planned` File (not real
        // code), and the corrected parent's Contains edge is the only one.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .q("MATCH (n:File {fqn: '/todo/app.ts'}) RETURN n.status")
            .unwrap();
        assert!(out.contains("planned"), "DB placeholder row: {out}");
        let out = db
            .q("MATCH (p)-[:Contains]->(f:File {fqn: '/todo/app.ts'}) RETURN p.fqn")
            .unwrap();
        assert!(
            out.contains("github.com/x/y"),
            "DB corrected parent edge: {out}"
        );
        assert!(
            !out.contains("github.com/x/y.Missing"),
            "no stale parent edge: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    /// Positive control: a FQN that IS real scanned code is still refused,
    /// with the existing message wording.
    #[test]
    fn plan_add_planned_refuses_real_scanned_code() {
        let (apg_root, repo, _wt) = fixture("planned-real");
        let _path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();

        // `github.com/x/y.Store` is a real Struct in the fixture DB — planning
        // over it is refused before any record lands.
        let err = plan_add_planned_at(
            &apg_root,
            &mut records,
            "struct",
            "github.com/x/y.Store",
            "Store",
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("already resolves to a `Struct` code node"),
            "{err}"
        );
        assert!(
            records.iter().all(
                |r| !matches!(r, Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Store")
            ),
            "a refused planned node must not leave a record"
        );

        testutil::remove(&repo);
    }

    /// An FQN that matches only an `UnresolvedTarget` is not code: the planned
    /// declaration lands (the old `code_label` probe counted the unresolved
    /// row and refused).
    #[test]
    fn plan_add_planned_ignores_unresolved_targets() {
        let (apg_root, repo, _wt) = fixture("planned-unresolved");
        let _path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();

        plan_add_planned_at(
            &apg_root,
            &mut records,
            "function",
            "github.com/x/y.Missing",
            "Missing",
            None,
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();
        assert!(records.iter().any(
            |r| matches!(r, Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Missing")
        ));

        testutil::remove(&repo);
    }

    #[test]
    fn task_verb_modifies_deletes_require_real_scanned_code() {
        let (apg_root, repo, _wt) = fixture("verb-modifies");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ];

        // modifies/deletes with an unresolvable FQN are refused.
        for verb in ["modifies", "deletes"] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "github.com/x/y.Nope",
                "",
            )
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("does not resolve in the scanned graph"),
                "{verb}: {err}"
            );
        }
        // modifies/deletes against a still-planned FQN are refused too — only
        // a creates builds a planned node.
        let mut with_planned = records.clone();
        with_planned.push(Record::PlannedNode {
            fqn: "github.com/x/y.Gateway".to_string(),
            kind: "struct".to_string(),
            name: "Gateway".to_string(),
            parent: String::new(),
        });
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut with_planned,
            1,
            1,
            "T",
            "source",
            "",
            "modifies",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("is a planned node, not scanned code"),
            "{err}"
        );

        // modifies/deletes with a real scanned FQN are accepted; the verb and
        // target land on the task record.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "modifies",
            "github.com/x/y.Store",
            "",
        )
        .unwrap();
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            2,
            "T2",
            "source",
            "",
            "deletes",
            "github.com/x/y.Store",
            "",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && verb == "modifies"
                    && target == "github.com/x/y.Store"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-2"
                    && verb == "deletes"
                    && target == "github.com/x/y.Store"
        )));

        testutil::remove(&repo);
    }

    #[test]
    fn task_verb_renames_moves_validate_source_and_new_fqn() {
        let (apg_root, repo, _wt) = fixture("verb-rename");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ];

        // renames/moves with an unresolvable source are refused.
        for verb in ["renames", "moves"] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "github.com/x/y.Nope",
                "github.com/x/y.Gateway",
            )
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("does not resolve in the scanned graph"),
                "{verb}: {err}"
            );
        }
        // A rename without the destination is refused.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "renames",
            "github.com/x/y.Store",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("requires --to"), "{err}");

        // A colliding destination is refused — both against another real node
        // and against the source itself.
        for (verb, to) in [
            ("renames", "/abs/store.go"),
            ("moves", "github.com/x/y.Store"),
        ] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "github.com/x/y.Store",
                to,
            )
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("must not collide with existing code"),
                "{verb}: {err}"
            );
        }

        // renames/moves with a resolving source and a free destination are
        // accepted; the pair is recorded on the task.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "renames",
            "github.com/x/y.Store",
            "github.com/x/y.Store2",
        )
        .unwrap();
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            2,
            "T2",
            "source",
            "",
            "moves",
            "github.com/x/y.Store",
            "github.com/x/y.Store2",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, new_fqn, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && verb == "renames"
                    && target == "github.com/x/y.Store"
                    && new_fqn == "github.com/x/y.Store2"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, new_fqn, .. }
                if fqn == "foo/plan.phase-01.task-2"
                    && verb == "moves"
                    && target == "github.com/x/y.Store"
                    && new_fqn == "github.com/x/y.Store2"
        )));

        testutil::remove(&repo);
    }

    #[test]
    fn task_verb_invalid_verbs_and_flag_combinations_refused() {
        let (apg_root, repo, _wt) = fixture("verb-flags");
        let mut records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ];

        // Unknown verb refused.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "explodes",
            "github.com/x/y.Store",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid task verb"), "{err}");

        // A non-creates verb without a target FQN is meaningless — refused.
        for verb in ["modifies", "deletes", "renames", "moves"] {
            let err = plan_add_task_at(
                &apg_root,
                "foo",
                &mut records,
                1,
                1,
                "T",
                "source",
                "",
                verb,
                "",
                "",
            )
            .unwrap_err();
            assert!(err.to_string().contains("requires --fqn"), "{verb}: {err}");
        }

        // --to is only valid for renames/moves.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "github.com/x/y.Gateway2",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("only valid for renames/moves"),
            "{err}"
        );

        // An omitted verb defaults to creates-without-target — the pre-verb
        // task shape still authors.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "",
            "",
            "",
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Task { fqn, verb, target, new_fqn, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && verb == "creates"
                    && target.is_empty()
                    && new_fqn.is_empty()
        )));

        testutil::remove(&repo);
    }

    #[test]
    fn task_verb_old_format_records_default_to_creates() {
        // A task record authored before the verb model (no verb/target keys)
        // parses with the default verb `creates` and no target — the transient
        // plan store is forward-compatible with old-format tasks.
        let dir = std::env::temp_dir().join(format!("apg-plan-verb-parse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"task","fqn":"foo/plan.phase-01.task-1","title":"T","kind":"source","tier":"","status":"pending"}"#,
        )
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        match &recs[0] {
            Record::Task {
                verb,
                target,
                new_fqn,
                ..
            } => {
                assert_eq!(verb, "creates");
                assert!(target.is_empty(), "old tasks carry no target");
                assert!(new_fqn.is_empty(), "old tasks carry no destination");
            }
            other => panic!("expected a task record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_groups_tasks_by_kind_and_shows_test_tier() {
        let records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "Implement".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-2".into(),
                title: "Unit tests".into(),
                kind: "test".into(),
                tier: "unit".into(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-3".into(),
                title: "Doc".into(),
                kind: "docs".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-2".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-3".into(),
            },
        ];
        let out = render_phase_tasks(&records, "foo/plan.phase-01");
        // Grouped by kind in canonical order (source before test before docs).
        let source_pos = out.find("**source**").unwrap();
        let test_pos = out.find("**test**").unwrap();
        let docs_pos = out.find("**docs**").unwrap();
        assert!(source_pos < test_pos && test_pos < docs_pos, "order: {out}");
        // Test depth renders as test/unit.
        assert!(
            out.contains("- [ ] `foo/plan.phase-01.task-2` — Unit tests (test/unit)"),
            "{out}"
        );
        // Done checkbox preserved.
        assert!(
            out.contains("- [x] `foo/plan.phase-01.task-1` — Implement (source)"),
            "{out}"
        );
    }

    #[test]
    fn plan_add_phase_rejects_transitive_gate_cycle() {
        // Phase-1 gates on phase-2, phase-2 gates on phase-3. Adding
        // phase-1 → phase-3 closes 1 → 3 → 2 → 1 — the same
        // `cycle_closing_path` machinery `apg plan link` uses now guards the
        // `plan add phase --prereq` path too (REVIEW: the add arm used to push
        // Gates with no cycle check). Mirrors the spec-side phase_gate_edge
        // test.
        let records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: String::new(),
                status: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: String::new(),
                status: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-03".into(),
                number: 3,
                title: "P3".into(),
                deliverable: String::new(),
                status: String::new(),
            },
            Record::Gates {
                from: "foo/plan.phase-02".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-03".into(),
                to: "foo/plan.phase-02".into(),
            },
        ];
        let err = push_gate("foo/plan.phase-01", "foo/plan.phase-03", &records).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("would create a cycle"), "got: {msg}");
        assert!(msg.contains("01 → 03"), "got: {msg}");
        // A self-gate is rejected (dedicated cycle path).
        let err = push_gate("foo/plan.phase-01", "foo/plan.phase-01", &records).unwrap_err();
        assert!(
            format!("{err:#}").contains("would create a cycle"),
            "got: {err:#}"
        );
        // A benign gate (phase-01 → phase-04) closes nothing.
        push_gate("foo/plan.phase-01", "foo/plan.phase-04", &records).unwrap();
    }

    #[test]
    fn plan_add_task_rejects_nonexistent_phase() {
        let (apg_root, repo, _wt) = fixture("task-no-phase");
        let _path = write_plan(&apg_root); // plan has phase-01 only

        let mut records = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        // A task under phase 02 (not authored) is rejected before any write.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            2,
            1,
            "T",
            "source",
            "",
            "creates",
            "",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("no such phase"), "{err}");
        assert!(
            records.iter().all(
                |r| !matches!(r, Record::Task { fqn, .. } if fqn.starts_with("foo/plan.phase-02"))
            ),
            "a rejected task must not leave a partial Task record"
        );

        testutil::remove(&repo);
    }

    /// `apg plan link` is retired along with `plan_link_at`; the set-semantics
    /// now live in `plan_update_phase_at`, which refuses an absent phase before
    /// any write — the plan store stays untouched.
    #[test]
    fn plan_link_rejects_nonexistent_phase() {
        let (apg_root, repo, _wt) = fixture("link-no-phase");
        let plan_path = write_plan(&apg_root); // plan has phase-01 only
        let before = std::fs::read_to_string(&plan_path).unwrap();

        let err = plan_update_phase_at(
            &apg_root,
            "foo",
            &mut load_plan(&apg_root, "foo").unwrap(),
            2,
            Some("ghost"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a phase of `foo`"), "{err}");
        // The plan JSONL is untouched (no partial write).
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before,
            "an absent-phase update must not touch the plan store"
        );
        let recs = specs::read_jsonl(&plan_path).unwrap();
        assert!(
            recs.iter().all(|r| !matches!(r, Record::Satisfies { .. })),
            "a rejected update must not write Satisfies"
        );

        testutil::remove(&repo);
    }

    /// Int (fixture repo/branch DB): `plan add phase|task|planned` refuses an
    /// existing entity (naming `update`/`rm`), leaving the existing
    /// Contains/Reviews edges intact and the plan store untouched.
    #[test]
    fn plan_add_subentities_refuse_existing_and_preserve_edges() {
        let (apg_root, repo, _wt) = fixture("add-refuse-existing");
        let plan_path = write_plan(&apg_root);
        let mut records = load_plan(&apg_root, "foo").unwrap();
        records.push(Record::PlannedNode {
            fqn: "/todo/app.ts".into(),
            kind: "file".into(),
            name: "app.ts".into(),
            parent: "github.com/x/y".into(),
        });
        records.push(Record::Contains {
            from: "github.com/x/y".into(),
            to: "/todo/app.ts".into(),
        });
        records.push(Record::Reviews {
            from: "foo/feedback-1".into(),
            to: "foo/plan.phase-01.task-1".into(),
        });
        specs::write_jsonl(&plan_path, &records).unwrap();
        let before = std::fs::read_to_string(&plan_path).unwrap();

        // Re-adding the existing phase is refused, naming update/rm.
        let err = plan_add_phase_at(
            &apg_root,
            "foo",
            &mut records,
            "foo/plan",
            1,
            "P1b",
            "D",
            &[],
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            err.to_string().contains("apg plan update foo phase 1"),
            "{err}"
        );
        assert!(err.to_string().contains("apg plan rm foo phase 1"), "{err}");
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), before);

        // Re-adding the existing task is refused; its phase Contains edge and
        // incident Reviews edge survive.
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T2",
            "source",
            "",
            "creates",
            "",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            err.to_string().contains("apg plan update foo task 1 1"),
            "{err}"
        );
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Reviews { from, to }
                if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
        )));

        // Re-adding the existing planned node is refused; its parent Contains
        // edge survives.
        let err = plan_add_planned_at(
            &apg_root,
            &mut records,
            "file",
            "/todo/app.ts",
            "app.ts",
            Some("github.com/x/y"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            err.to_string()
                .contains("apg plan update foo planned /todo/app.ts"),
            "{err}"
        );
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "github.com/x/y" && to == "/todo/app.ts"
        )));
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before,
            "every refused add leaves the plan store untouched"
        );

        testutil::remove(&repo);
    }

    /// Int: `apg plan link` is retired (unknown subcommand); `apg plan update
    /// <project> phase <n> --satisfies/--prereq` replaces only that phase's
    /// outgoing Satisfies/Gates edges (set-semantics), leaving other phases and
    /// incoming edges intact.
    #[test]
    fn plan_link_retired_and_update_phase_sets_bridge_edges() {
        let (apg_root, repo, wt) = fixture("link-retired");
        // A `--satisfies` target must resolve in the layers store; author one
        // and re-anchor the recorded scan_meta dirty (the untracked node file
        // makes the tree dirty, which the write-through's staleness gate sees).
        let req = "phase-update-preserves-tasks";
        let req_fqn = format!("requirements.requirement.{req}");
        write_requirement(&apg_root, req);
        testutil::write_scan_meta(
            &apg_root,
            Some(&repo.head_sha()),
            false,
            "2026-09-07T00:00:00Z",
        );
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "P".into(),
                strategy: String::new(),
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
            Record::PlanPhase {
                fqn: "foo/plan.phase-03".into(),
                number: 3,
                title: "P3".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
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
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "requirements.requirement.R1".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-02".into(),
            },
            // A later phase gating phase-01: an incoming edge, must survive.
            Record::Gates {
                from: "foo/plan.phase-03".into(),
                to: "foo/plan.phase-01".into(),
            },
            // An unrelated phase's own Satisfies must survive.
            Record::Satisfies {
                from: "foo/plan.phase-02".into(),
                to: "requirements.requirement.R3".into(),
            },
        ];
        specs::write_jsonl(&plan_path, &records).unwrap();

        // `apg plan link` is retired at dispatch: an unknown subcommand.
        let err = cmd_plan(&["link".to_string(), "foo".to_string(), "1".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown apg plan subcommand: link"),
            "{err}"
        );

        // The update-phase arm folds link's set-semantics through cmd_plan.
        with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "1".to_string(),
                "--satisfies".to_string(),
                req.to_string(),
                "--prereq".to_string(),
                "2".to_string(),
            ])
        })
        .unwrap();

        let recs = specs::read_jsonl(&plan_path).unwrap();
        let satisfies: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            satisfies,
            vec![req_fqn.as_str()],
            "the passed --satisfies replaces phase-01's outgoing set"
        );
        let gates: Vec<&str> = recs
            .iter()
            .filter_map(|r| match r {
                Record::Gates { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            gates,
            vec!["foo/plan.phase-02"],
            "the passed --prereq replaces phase-01's outgoing gate set"
        );
        // Incoming gate + unrelated phase Satisfies + the task Contains edge
        // survive the set-semantics rewrite.
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-03" && to == "foo/plan.phase-01"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Satisfies { from, to }
                if from == "foo/plan.phase-02" && to == "requirements.requirement.R3"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-01.task-1"
        )));

        testutil::remove(&repo);
    }

    #[test]
    fn plan_complete_writes_durable_phase_status() {
        let (apg_root, repo, _wt) = fixture("durable-phase");

        // Pending task → the phase-complete gate rejects (nothing written).
        let _path = write_plan(&apg_root);
        assert!(plan_complete_at(&apg_root, "foo", 1).is_err());
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase { fqn, status, .. } if fqn == "foo/plan.phase-01" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            status, "pending",
            "a rejected complete must not flip status"
        );

        // Gate green → the milestone is durably recorded: status flips to done
        // in the JSONL (distinguishable from tasks-done + feedback-resolved).
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase { fqn, status, .. } if fqn == "foo/plan.phase-01" => {
                    Some(status.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "done", "complete must write the durable milestone");

        // The DB carries it too (the plan write-through re-ingests).
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .q(&format!(
                "MATCH (p:PlanPhase {{fqn: {}}}) RETURN p.status",
                artifacts::lit("foo/plan.phase-01")
            ))
            .unwrap()
            .to_string();
        assert!(out.contains("done"), "phase status in DB: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    fn apply_gate_rejects_unresolved_target_as_unrealized() {
        let (apg_root, repo, _wt) = fixture("apply-unresolved");

        // The planned node's FQN matches an UnresolvedTarget node in the graph
        // (an unresolved reference, NOT real code) — the gate must block.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Missing".to_string(),
                kind: "struct".to_string(),
                name: "Missing".to_string(),
                parent: String::new(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "resolved".to_string(),
                disposition: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("not realized"), "{err}");
        assert!(err.to_string().contains("github.com/x/y.Missing"), "{err}");

        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // coverage_check: derived solution coverage in plan verify — every
    // in-scope solution node's implemented-by FQN (reached from a satisfied
    // requirement, or added on this branch) must be touched by at least one
    // plan task (SPEC §5); the coherence gate refuses when it does not.
    // ------------------------------------------------------------------

    /// Writes one solution-layer node file under `apg/layers/solution/` with
    /// the given `implemented-by` code-FQN targets (raw file write — the
    /// fixture commits it; the production node-file path is exercised by
    /// layers' own tests).
    fn write_solution_node(apg_root: &Path, node_type: &str, name: &str, refs: &[&str]) {
        let nf = crate::layers::NodeFile {
            layer: "solution".to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: String::new(),
            properties: std::collections::BTreeMap::new(),
            out: refs
                .iter()
                .map(|t| crate::layers::OutEdge {
                    kind: "implemented-by".to_string(),
                    target: t.to_string(),
                    properties: std::collections::BTreeMap::new(),
                })
                .collect(),
            in_edges: Vec::new(),
        };
        let path = crate::layers::node_file_path(
            apg_root,
            crate::layers::Layer::Solution,
            node_type,
            name,
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&nf).unwrap()).unwrap();
    }

    /// A node file with the given identity and `(kind, target)` out-edges — the
    /// building block for the pure `coverage_check` tests and the spine
    /// fixtures.
    fn nf(
        layer: &str,
        node_type: &str,
        name: &str,
        edges: &[(&str, &str)],
    ) -> crate::layers::NodeFile {
        crate::layers::NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: String::new(),
            properties: std::collections::BTreeMap::new(),
            out: edges
                .iter()
                .map(|(k, t)| crate::layers::OutEdge {
                    kind: k.to_string(),
                    target: t.to_string(),
                    properties: std::collections::BTreeMap::new(),
                })
                .collect(),
            in_edges: Vec::new(),
        }
    }

    /// A single-task record (phase-01/task-1) with the given verb and
    /// target(s) — the coverage touch source.
    fn task_rec(verb: &str, target: &str, new_fqn: &str) -> Record {
        Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: verb.to_string(),
            target: target.to_string(),
            new_fqn: new_fqn.to_string(),
        }
    }

    /// Writes an arbitrary layers node file with the given `(kind, target)`
    /// out-edges (raw file write; the fixture commits it).
    fn write_node_file(
        apg_root: &Path,
        layer: &str,
        node_type: &str,
        name: &str,
        edges: &[(&str, &str)],
    ) {
        let l = match layer {
            "requirements" => crate::layers::Layer::Requirements,
            "domain" => crate::layers::Layer::Domain,
            "solution" => crate::layers::Layer::Solution,
            "implementation" => crate::layers::Layer::Implementation,
            "global" => crate::layers::Layer::Global,
            other => panic!("bad layer `{other}`"),
        };
        let node = nf(layer, node_type, name, edges);
        let path = crate::layers::node_file_path(apg_root, l, node_type, name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&node).unwrap()).unwrap();
    }

    /// Commits the given repo-relative paths on the worktree's branch (git2
    /// — the same mechanics auto_commit uses), returning the new branch HEAD
    /// sha (re-anchor scan_meta against it so the branch graph stays fresh).
    fn wt_commit_paths(wt: &Path, rels: &[&str], msg: &str) -> String {
        let repo = git2::Repository::open(wt).unwrap();
        let mut index = repo.index().unwrap();
        for rel in rels {
            index.add_path(Path::new(rel)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
            .unwrap();
        oid.to_string()
    }

    /// A minimal green plan: Plan + phase-01 (no tasks, no planned nodes, no
    /// feedback) — the gates other than coverage are vacuous.
    fn bare_plan() -> Vec<Record> {
        vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
        ]
    }

    #[test]
    fn coverage_holds_when_every_implemented_by_fqn_is_touched() {
        // One solution node whose implemented-by FQNs are a real scanned
        // Struct (covered by a modifies task) and an absent FQN (covered by a
        // creates task — the planned-node case: a creates over a still-absent
        // FQN counts exactly like a modifies over real code). The node is
        // ADDED ON THIS BRANCH, so it is in scope even though no satisfied
        // requirement reaches it; there is no cumulative-store assumption —
        // a pre-existing unreachable node would be exempt (task-7's int test).
        let (apg_root, repo, wt) = fixture("coverage-ok");
        write_solution_node(
            &apg_root,
            "system",
            "payments",
            &["github.com/x/y.Store", "github.com/x/y.Gateway"],
        );
        let sha = wt_commit_paths(
            &wt,
            &["apg/layers/solution/system/payments.json"],
            "author solution node",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "done".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-2".to_string(),
            title: "T2".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "creates".to_string(),
            target: "github.com/x/y.Gateway".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-2".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        // Every implemented-by FQN is touched -> the bridge is complete.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_refuses_when_an_implemented_by_fqn_is_untouched() {
        // Two branch-added solution nodes; the plan touches only the real
        // Struct — the container's absent Gateway FQN is uncovered, so verify
        // refuses, naming the FQN, its solution node, and the matching creates
        // suggestion.
        let (apg_root, repo, wt) = fixture("coverage-gap");
        write_solution_node(&apg_root, "container", "api", &["github.com/x/y.Gateway"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/container/api.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("coverage incomplete"), "{msg}");
        assert!(msg.contains("solution.container.api"), "{msg}");
        assert!(msg.contains("github.com/x/y.Gateway"), "{msg}");
        assert!(
            msg.contains("--verb creates --fqn github.com/x/y.Gateway"),
            "{msg}"
        );

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_renames_moves_destination_counts() {
        // A branch-added node whose renames task destination equals the
        // implemented-by FQN touches it (the new_fqn half of the pair) — the
        // bridge holds under branch-delta scoping.
        let (apg_root, repo, wt) = fixture("coverage-rename");
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store2"],
        );
        let sha = wt_commit_paths(
            &wt,
            &["apg/layers/solution/component/checkout.json"],
            "author solution node",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "renames".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: "github.com/x/y.Store2".to_string(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_trivially_holds_with_no_solution_nodes() {
        // No solution-layer node files at all — nothing is spine-reached and
        // the branch delta is empty, so coverage is a no-op and verify passes
        // on the other gates alone.
        let (apg_root, repo, _wt) = fixture("coverage-empty");
        let _path = write_plan(&apg_root);
        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        testutil::remove(&repo);
    }

    #[test]
    fn coverage_exempts_solution_node_without_implemented_by_edge() {
        // A BRANCH-ADDED solution node with NO implemented-by edge stays
        // exempt — the rule is over the node's implemented-by FQNs, and with
        // none there is nothing to touch (nothing blocks). The gap is surfaced
        // as a warning (no code claims the node), never a blocker: the
        // no-claims list is part of the report.
        let (apg_root, repo, wt) = fixture("coverage-no-claim");
        write_solution_node(&apg_root, "system", "payments", &[]);
        let sha = wt_commit_paths(
            &wt,
            &["apg/layers/solution/system/payments.json"],
            "author solution node",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let records = bare_plan();
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        let nodes = crate::layers::read_existing_nodes(&apg_root).unwrap();
        let branch_added: BTreeSet<String> = ["solution.system.payments".to_string()]
            .into_iter()
            .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert_eq!(report.no_claims, vec!["solution.system.payments"]);
        assert!(report.gaps.is_empty());

        // The same node, neither branch-added nor spine-reached, is ignored
        // entirely (the pre-existing exemption).
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.no_claims.is_empty(), "{report:?}");
        assert!(report.gaps.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_check_reports_gaps_no_claims_and_verb_agnostic_touches() {
        // Pure semantics: exact-FQN touch matching, verb-agnostic (a deletes
        // task touches what it deletes), a renames/moves new_fqn covering the
        // destination, a creates covering a planned FQN, empty-target tasks
        // touching nothing, and non-solution nodes (person, solution notes)
        // ignored. The solution nodes are branch-added; the delta is supplied
        // directly here (the git compare is exercised by the int test).
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T1".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "modifies".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-2".to_string(),
                title: "T2".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "deletes".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-3".to_string(),
                title: "T3".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "renames".to_string(),
                target: "github.com/x/y.Old".to_string(),
                new_fqn: "github.com/x/y.Store2".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-4".to_string(),
                title: "T4".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: "github.com/x/y.Gateway".to_string(),
                new_fqn: String::new(),
            },
            // A target-less creates touches nothing.
            Record::Task {
                fqn: "foo/plan.phase-01.task-5".to_string(),
                title: "T5".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            // The planned-node declaration is irrelevant to coverage — the
            // creates task's touch is what counts (edge case: an
            // implemented-by FQN already `status: planned` in the DB,
            // awaiting its creates task).
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        let node = |t: &str, n: &str, refs: &[&str]| crate::layers::NodeFile {
            layer: "solution".to_string(),
            node_type: t.to_string(),
            name: n.to_string(),
            body: String::new(),
            properties: std::collections::BTreeMap::new(),
            out: refs
                .iter()
                .map(|r| crate::layers::OutEdge {
                    kind: "implemented-by".to_string(),
                    target: r.to_string(),
                    properties: std::collections::BTreeMap::new(),
                })
                .collect(),
            in_edges: Vec::new(),
        };
        let nodes = vec![
            node(
                "system",
                "payments",
                &["github.com/x/y.Store", "github.com/x/y.Gateway"],
            ),
            node("container", "api", &["github.com/x/y.Store2"]),
            // An untouched implemented-by FQN -> the gap.
            node("component", "reporting", &["github.com/x/y.Reporting"]),
            // No implemented-by edge -> exempt, reported as a warning.
            node("component", "checkout", &[]),
            // Not a solution kind -> ignored entirely.
            node("person", "ops", &["github.com/x/y.Store"]),
            crate::layers::NodeFile {
                layer: "solution".to_string(),
                node_type: "note".to_string(),
                name: "design-note".to_string(),
                body: String::new(),
                properties: std::collections::BTreeMap::new(),
                out: vec![crate::layers::OutEdge {
                    kind: "implemented-by".to_string(),
                    target: "github.com/x/y.Store".to_string(),
                    properties: std::collections::BTreeMap::new(),
                }],
                in_edges: Vec::new(),
            },
        ];
        // The nodes are branch-added (the pure test supplies the delta
        // directly — the git half is exercised by the int test).
        let branch_added: BTreeSet<String> = [
            "solution.system.payments",
            "solution.container.api",
            "solution.component.reporting",
            "solution.component.checkout",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        // Only the reporting component's FQN is untouched: Store is touched
        // (modifies AND deletes — verb-agnostic), Gateway by the creates,
        // Store2 by the rename's new_fqn.
        assert_eq!(
            report.gaps,
            vec![CoverageGap {
                solution: "solution.component.reporting".to_string(),
                fqn: "github.com/x/y.Reporting".to_string(),
            }]
        );
        assert_eq!(report.no_claims, vec!["solution.component.checkout"]);
    }

    // ------------------------------------------------------------------
    // phase-06 (spine-scoped coverage): the pure coverage_check semantics —
    // spine reachability, the branch delta, the pre-existing exemption, and
    // the no-claims exemption — plus the plan_verify_at int path that supplies
    // the real satisfied set and default-branch delta.
    // ------------------------------------------------------------------

    #[test]
    fn coverage_spine_reachability_scopes_required_solution_nodes() {
        // Only solution nodes reached from a SATISFIED requirement through
        // Requirement Drives Domain RealisedBy Solution are required; an
        // unrelated (non-branch-added) solution node is ignored — even an
        // untouched implemented-by FQN on it yields no gap.
        let nodes = vec![
            nf(
                "requirements",
                "requirement",
                "cr",
                &[("drives", "domain.entity.plan-record")],
            ),
            nf(
                "domain",
                "entity",
                "plan-record",
                &[("realised-by", "solution.component.reached")],
            ),
            nf(
                "solution",
                "component",
                "reached",
                &[("implemented-by", "github.com/x/y.Store")],
            ),
            nf(
                "solution",
                "component",
                "unrelated",
                &[("implemented-by", "github.com/x/y.Untouched")],
            ),
        ];
        let satisfied: BTreeSet<String> = ["requirements.requirement.cr".to_string()]
            .into_iter()
            .collect();

        // The reached node's FQN is touched -> no gap; the unrelated node's
        // untouched FQN is never considered.
        let records = vec![task_rec("modifies", "github.com/x/y.Store", "")];
        let report = coverage_check(&records, &nodes, &satisfied, &BTreeSet::new());
        assert!(report.gaps.is_empty(), "{report:?}");

        // Leave the reached node's FQN untouched -> exactly that gap (the
        // unrelated node stays out of scope).
        let records = vec![task_rec("creates", "github.com/x/y.Other", "")];
        let report = coverage_check(&records, &nodes, &satisfied, &BTreeSet::new());
        assert_eq!(
            report.gaps,
            vec![CoverageGap {
                solution: "solution.component.reached".to_string(),
                fqn: "github.com/x/y.Store".to_string(),
            }]
        );
    }

    #[test]
    fn coverage_branch_added_solution_nodes_are_required_without_a_spine() {
        // A branch-added solution node's implemented-by FQN is required even
        // when no satisfied requirement reaches it; a branch-added node with
        // NO implemented-by edge stays exempt via CoverageReport::no_claims
        // (the branch-added plan-store-atomic-rewrite forces no fake task).
        let nodes = vec![
            nf(
                "solution",
                "component",
                "added",
                &[("implemented-by", "github.com/x/y.Store")],
            ),
            nf("solution", "component", "plan-store-atomic-rewrite", &[]),
        ];
        let branch_added: BTreeSet<String> = [
            "solution.component.added".to_string(),
            "solution.component.plan-store-atomic-rewrite".to_string(),
        ]
        .into_iter()
        .collect();

        let records = vec![task_rec("modifies", "github.com/x/y.Store", "")];
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert!(report.gaps.is_empty(), "{report:?}");
        assert_eq!(
            report.no_claims,
            vec!["solution.component.plan-store-atomic-rewrite"]
        );

        // The branch-added node's FQN untouched -> a gap even with no spine.
        let records = vec![task_rec("creates", "github.com/x/y.Other", "")];
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert_eq!(
            report.gaps,
            vec![CoverageGap {
                solution: "solution.component.added".to_string(),
                fqn: "github.com/x/y.Store".to_string(),
            }]
        );
    }

    #[test]
    fn coverage_exempts_unreachable_pre_existing_solution_nodes() {
        // The merged worktree-cleanup nodes are pre-existing (present on the
        // default branch) and unreachable from any satisfied requirement, so
        // they yield no gaps and force no fake modifies tasks.
        let nodes = vec![
            nf(
                "solution",
                "component",
                "project-delete",
                &[("implemented-by", "apg.project_cmd.delete_project")],
            ),
            nf(
                "solution",
                "component",
                "project-dispatch",
                &[("implemented-by", "apg.project_cmd.cmd_project")],
            ),
            nf(
                "solution",
                "component",
                "project-merge",
                &[("implemented-by", "apg.project_cmd.project_merge_at")],
            ),
            nf(
                "solution",
                "container",
                "project-command",
                &[("implemented-by", "apg.project_cmd.cmd_project")],
            ),
        ];
        let records = vec![task_rec("creates", "github.com/x/y.Gateway", "")];
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.gaps.is_empty(), "{report:?}");
        assert!(report.no_claims.is_empty(), "{report:?}");
    }

    #[test]
    fn plan_verify_at_supplies_spine_and_branch_delta() {
        // plan_verify_at computes the satisfied-requirement set from the
        // plan's Satisfies edges and the branch delta against the repo's
        // default branch (the public repo_identity default_branch). A
        // pre-existing unreachable node (committed on `main` BEFORE the
        // project branch) forces no gap; the reached + branch-added nodes'
        // implemented-by FQNs must be touched.
        let repo = Repo::new("verify-spine");
        // The pre-existing worktree-cleanup nodes: present on `main` before the
        // branch is cut, so they are not part of the branch delta and — being
        // unreachable from the plan's satisfied requirement — must force no
        // gap and no fake `modifies` task.
        for (node_type, name, code) in [
            (
                "component",
                "project-delete",
                "apg.project_cmd.delete_project",
            ),
            (
                "component",
                "project-dispatch",
                "apg.project_cmd.cmd_project",
            ),
            (
                "component",
                "project-merge",
                "apg.project_cmd.project_merge_at",
            ),
            (
                "container",
                "project-command",
                "apg.project_cmd.cmd_project",
            ),
        ] {
            repo.write(
                &format!("apg/layers/solution/{node_type}/{name}.json"),
                &serde_json::to_string_pretty(&nf(
                    "solution",
                    node_type,
                    name,
                    &[("implemented-by", code)],
                ))
                .unwrap(),
            );
        }
        repo.commit_all("author pre-existing worktree-cleanup nodes");
        let wt = repo.start_project("foo");
        db_at(&wt);
        let apg_root = wt.join(specs::LAYOUT);

        // The branch's spine + branch-added solution nodes.
        write_node_file(
            &apg_root,
            "requirements",
            "requirement",
            "cr",
            &[("drives", "domain.entity.plan-record")],
        );
        write_node_file(
            &apg_root,
            "domain",
            "entity",
            "plan-record",
            &[(
                "realised-by",
                "solution.component.coverage-spine-validation",
            )],
        );
        write_solution_node(
            &apg_root,
            "component",
            "coverage-spine-validation",
            &["github.com/x/y.Store"],
        );
        write_solution_node(&apg_root, "component", "plan-store-atomic-rewrite", &[]);
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/requirements/requirement/cr.json",
                "apg/layers/domain/entity/plan-record.json",
                "apg/layers/solution/component/coverage-spine-validation.json",
                "apg/layers/solution/component/plan-store-atomic-rewrite.json",
            ],
            "author the branch spine and solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        // Green: the reached/branch-added FQN is touched; the pre-existing
        // unreachable node and the no-claim node force no gap.
        let mut records = bare_plan();
        records.push(Record::Satisfies {
            from: "foo/plan.phase-01".to_string(),
            to: "requirements.requirement.cr".to_string(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        records.push(task_rec("modifies", "github.com/x/y.Store", ""));
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();
        assert!(plan_verify_at(&apg_root, "foo").is_ok());

        // Refusal: drop the touching task -> the reached node's FQN is a gap,
        // named with its solution node; the pre-existing node is still not
        // named (no false gap).
        let mut records = bare_plan();
        records.push(Record::Satisfies {
            from: "foo/plan.phase-01".to_string(),
            to: "requirements.requirement.cr".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("coverage incomplete"), "{msg}");
        assert!(
            msg.contains("solution.component.coverage-spine-validation"),
            "{msg}"
        );
        assert!(msg.contains("github.com/x/y.Store"), "{msg}");
        // None of the pre-existing unreachable worktree-cleanup nodes is named.
        for name in [
            "project-delete",
            "project-dispatch",
            "project-merge",
            "project-command",
        ] {
            assert!(!msg.contains(name), "{name} named in: {msg}");
        }

        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // task-5 (coverage tests): the task-text audit gaps — the green path
    // across MULTIPLE solution nodes, both halves of a renames/moves pair as
    // touches, and a refusal that names ONLY the uncovered node.
    // ------------------------------------------------------------------

    #[test]
    fn coverage_holds_across_multiple_solution_nodes() {
        // The bridge is complete only when EVERY in-scope solution node's
        // implemented-by FQNs are touched — this is the green end-to-end path
        // through plan_verify_at across MULTIPLE branch-added solution nodes
        // (two containers; one real FQN, one still-absent FQN), not just
        // several FQNs on a single node. Verify returns its green verdict and
        // the derived report agrees: no gaps, no no-claims warnings.
        let (apg_root, repo, wt) = fixture("coverage-multi-ok");
        write_solution_node(&apg_root, "container", "api", &["github.com/x/y.Gateway"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/container/api.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "creates".to_string(),
            target: "github.com/x/y.Gateway".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-2".to_string(),
            title: "T2".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-2".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        // Every in-scope solution node's every implemented-by FQN is touched
        // -> green.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        let nodes = crate::layers::read_existing_nodes(&apg_root).unwrap();
        let branch_added: BTreeSet<String> =
            ["solution.container.api", "solution.component.checkout"]
                .into_iter()
                .map(str::to_string)
                .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert!(report.gaps.is_empty(), "{report:?}");
        assert!(report.no_claims.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_refusal_names_only_the_uncovered_solution() {
        // Mixed coverage across two branch-added nodes: the component's real
        // FQN is covered by a modifies task, the container's absent FQN is
        // not. The refusal names the uncovered node/FQN only — the covered
        // node and its claim appear nowhere in the message (no false positives
        // for a partially-covered bridge).
        let (apg_root, repo, wt) = fixture("coverage-mixed-names");
        write_solution_node(&apg_root, "container", "api", &["github.com/x/y.Gateway"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/container/api.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "modifies".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("coverage incomplete"), "{msg}");
        assert!(msg.contains("solution.container.api"), "{msg}");
        assert!(msg.contains("github.com/x/y.Gateway"), "{msg}");
        // The covered component and its claim are NOT named.
        assert!(!msg.contains("solution.component.checkout"), "{msg}");
        assert!(!msg.contains("github.com/x/y.Store"), "{msg}");

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_moves_touches_source_and_destination_halves() {
        // A renames/moves task claims the code at BOTH FQNs: `target` (the
        // source, where the code was) and `new_fqn` (the destination, where it
        // lands). Here the two halves are different branch-added solution
        // nodes' implemented-by targets, so the single `moves` covers both and
        // the bridge holds under branch-delta scoping. (coverage_check counts
        // `target` for every verb plus `new_fqn` for renames/moves — the pair
        // is one claim across two locations; the existing renames test covers
        // the destination half.)
        let (apg_root, repo, wt) = fixture("coverage-rename-both");
        write_solution_node(&apg_root, "system", "payments", &["github.com/x/y.Store"]);
        write_solution_node(
            &apg_root,
            "component",
            "checkout",
            &["github.com/x/y.Store2"],
        );
        let sha = wt_commit_paths(
            &wt,
            &[
                "apg/layers/solution/system/payments.json",
                "apg/layers/solution/component/checkout.json",
            ],
            "author solution nodes",
        );
        testutil::write_scan_meta(&apg_root, Some(&sha), true, "2026-09-07T00:00:00Z");

        let mut records = bare_plan();
        records.push(Record::Task {
            fqn: "foo/plan.phase-01.task-1".to_string(),
            title: "T1".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: "moves".to_string(),
            target: "github.com/x/y.Store".to_string(),
            new_fqn: "github.com/x/y.Store2".to_string(),
        });
        records.push(Record::Contains {
            from: "foo/plan.phase-01".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &records).unwrap();

        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        let nodes = crate::layers::read_existing_nodes(&apg_root).unwrap();
        let branch_added: BTreeSet<String> =
            ["solution.system.payments", "solution.component.checkout"]
                .into_iter()
                .map(str::to_string)
                .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert!(report.gaps.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    fn scoped_review_feedback_routes_and_gates_by_scope() {
        // Scoped-review routing (structural vs phase): feedback on the Plan
        // node is the structural scope, feedback on a PlanPhase is the phase
        // scope. The phase-complete gate only checks its phase's scope; the
        // apply gate checks every scope — so structural feedback does not
        // block a phase's completion milestone but blocks apply.
        let (apg_root, repo, _wt) = fixture("scoped-review");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            // Structural scope: an open review on the Plan node itself.
            Record::Feedback {
                fqn: "foo/feedback-structural".to_string(),
                body: "breakdown issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-structural".to_string(),
                to: "foo/plan".to_string(),
            },
            // Phase scope: an open review on the phase.
            Record::Feedback {
                fqn: "foo/feedback-phase".to_string(),
                body: "phase issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-phase".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // Phase completion is blocked by the PHASE-scope feedback but NOT the
        // structural (Plan-scope) feedback — the milestone routes by scope.
        // So: resolve the phase feedback, leave the structural open.
        let mut recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        for r in &mut recs {
            if let Record::Feedback { fqn, status, .. } = r
                && fqn == "foo/feedback-phase"
            {
                *status = "resolved".to_string();
            }
        }
        specs::write_jsonl(&path, &recs).unwrap();
        assert!(
            plan_complete_at(&apg_root, "foo", 1).is_ok(),
            "structural (Plan-scope) feedback must not block the phase milestone"
        );

        // Apply is blocked by the STRUCTURAL feedback — every scope must be
        // green at the coherence gate.
        let err = plan_verify_at(&apg_root, "foo").unwrap_err();
        assert!(
            err.to_string().contains("unresolved review feedback"),
            "{err}"
        );
        assert!(err.to_string().contains("foo/feedback-structural"), "{err}");

        testutil::remove(&repo);
    }

    #[test]
    fn plan_init_works_and_warns_without_spec_jsonl() {
        // The legacy spec-exists gate is gone: no `apg/specs/<project>.jsonl`
        // (and no requirement node files yet) still allows init — the empty
        // spec is surfaced by the caller, never a blocker. With a requirement
        // in the layers store the gate resolves green.
        let (apg_root, repo, _wt) = fixture("init-no-spec");

        // No requirements yet: init works and reports the warning condition.
        let has = plan_init_at(&apg_root, "foo", "Plan Foo", "S").unwrap();
        assert!(!has, "no requirement node files -> the warning path");
        assert!(
            specs::plan_jsonl_path(&apg_root, "foo").exists(),
            "init must create the plan store even without requirements"
        );
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(
            recs.iter()
                .any(|r| matches!(r, Record::Plan { fqn, .. } if fqn == "foo/plan")),
            "the Plan record must land in .trans/plans"
        );
        // The legacy spec store is never created or referenced.
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );

        // A requirement node file in the layers store satisfies the gate —
        // under a second project context (init is a per-branch act).
        let wt2 = repo.start_project("bar");
        let bar_apg = wt2.join(specs::LAYOUT);
        let req_file = crate::layers::node_file_path(
            &bar_apg,
            crate::layers::Layer::Requirements,
            "requirement",
            "timer",
        );
        std::fs::create_dir_all(req_file.parent().unwrap()).unwrap();
        std::fs::write(
            &req_file,
            r#"{"name":"timer","type":"requirement","layer":"requirements","body":"x","properties":{},"out":[],"in":[]}"#,
        )
        .unwrap();
        let has = plan_init_at(&bar_apg, "bar", "Plan Bar", "S2").unwrap();
        assert!(
            has,
            "a requirement node file must satisfy the spec-exists gate"
        );
        assert!(specs::plan_jsonl_path(&bar_apg, "bar").exists());
        assert!(
            !bar_apg.join("specs").exists(),
            "apg/specs must never be written"
        );

        testutil::remove(&repo);
    }

    #[test]
    fn plan_mutations_never_commit() {
        // Transience (SPEC §4.2/§5): plan mutations write the gitignored
        // `.trans/plans/` store and NEVER auto-commit — the branch HEAD does
        // not move and the tree stays clean through add/done/note mutations.
        let (apg_root, repo, _wt) = fixture("never-commit");
        let _path = write_plan(&apg_root);
        let head_before = repo.head_sha();

        // plan add (a phase), plan done (assertion), plan note — three
        // mutations through the central funnel.
        let mut records = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let plan_fqn = "foo/plan".to_string();
        plan_add_phase_at(
            &apg_root,
            "foo",
            &mut records,
            &plan_fqn,
            2,
            "P2",
            "D2",
            &[],
            &[],
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_note_at(
            &apg_root,
            "foo",
            "foo/plan.phase-01.task-1",
            "concern noted",
            "note",
        )
        .unwrap();

        // The plan state landed in the transient store...
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::PlanPhase { fqn, .. } if fqn == "foo/plan.phase-02"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Note { fqn, .. } if fqn == "foo/plan.note-1"
        )));
        // ...and nothing was committed: `.trans` is gitignored and transient.
        assert_eq!(
            repo.head_sha(),
            head_before,
            "plan mutations must never auto-commit"
        );
        assert!(
            repo.is_clean(),
            "the tree must stay clean — plan files are gitignored"
        );

        testutil::remove(&repo);
    }

    /// The worktree branch's HEAD sha — the branch an auto-commit would land
    /// on. `repo.head_sha()` reads the main checkout's HEAD, which a worktree
    /// commit could never move, so transience assertions must check this one.
    fn wt_head(wt: &Path) -> String {
        git2::Repository::open(wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string()
    }

    #[test]
    fn plan_remaining_mutation_paths_never_commit() {
        // Transience surface completion (SPEC §4.2/§5):
        // `plan_mutations_never_commit` covers add-phase/done/note; this
        // closes the rest of the mutation surface: init, task add (an
        // accepted creates through the write-through funnel, and a refused
        // modifies leaving the on-disk store byte-identical), planned-node
        // declaration, phase update (the retired link's successor), complete
        // (the durable milestone), and undone.
        // Every mutation writes only the gitignored `.trans/plans/` store:
        // the project branch HEAD (where an auto-commit would land), the main
        // HEAD, and the tree all stay untouched.
        let (apg_root, repo, wt) = fixture("never-commit-rest");
        let head_before = repo.head_sha();
        let branch_head_before = wt_head(&wt);

        // init writes the Plan record into the transient store.
        let has = plan_init_at(&apg_root, "foo", "P", "S").unwrap();
        assert!(!has, "no requirement node files -> the warning path");
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");

        // A phase add through the write-through funnel.
        let mut records = specs::read_jsonl(&plan_path).unwrap();
        plan_add_phase_at(
            &apg_root,
            "foo",
            &mut records,
            "foo/plan",
            1,
            "P1",
            "D1",
            &[],
            &[],
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // A refused modifies (unresolvable FQN) leaves the on-disk store
        // byte-identical — no partial Task record, no write, no commit.
        let before_file = std::fs::read_to_string(&plan_path).unwrap();
        let err = plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "modifies",
            "github.com/x/y.Nope",
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not resolve in the scanned graph"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            before_file,
            "a refused task add must not touch the plan store"
        );

        // An accepted creates against an unplanned FQN lands through the
        // funnel, verb + target on the task record.
        plan_add_task_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            1,
            "T",
            "source",
            "",
            "creates",
            "github.com/x/y.Gateway",
            "",
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();

        // The planned-node declaration (the `plan add planned` write).
        records.push(Record::PlannedNode {
            fqn: "github.com/x/y.Gateway".to_string(),
            kind: "struct".to_string(),
            name: "Gateway".to_string(),
            parent: String::new(),
        });
        write_through(&apg_root, "foo", &records).unwrap();

        // update phase (an in-place write-through — the retired link's
        // successor), then done → complete (the durable milestone) → undone.
        plan_update_phase_at(
            &apg_root,
            "foo",
            &mut records,
            1,
            Some("P1b"),
            None,
            None,
            None,
        )
        .unwrap();
        write_through(&apg_root, "foo", &records).unwrap();
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();
        plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();

        // The state landed in the transient store: the milestone, the task
        // (undone) with its verb + target, and the planned node.
        let recs = specs::read_jsonl(&plan_path).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::PlanPhase { fqn, status, .. }
                if fqn == "foo/plan.phase-01" && status == "done"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Task { fqn, status, verb, target, .. }
                if fqn == "foo/plan.phase-01.task-1"
                    && status == "pending"
                    && verb == "creates"
                    && target == "github.com/x/y.Gateway"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Gateway"
        )));

        // ...and nothing was committed: `.trans` is gitignored and transient.
        assert_eq!(
            wt_head(&wt),
            branch_head_before,
            "plan mutations must never auto-commit on the project branch"
        );
        assert_eq!(
            repo.head_sha(),
            head_before,
            "plan mutations must never move the main HEAD either"
        );
        assert!(
            repo.is_clean(),
            "the tree must stay clean — plan files are gitignored"
        );

        testutil::remove(&repo);
    }

    /// Runs `f` with the process cwd temporarily set to `dir` — the CLI
    /// wrappers resolve `apg/` by walking up from cwd. Serialized behind the
    /// shared cwd lock so it never interleaves with `scan_checkout`.
    fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = testutil::CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let out = f();
        std::env::set_current_dir(old).unwrap();
        out
    }

    /// The `foo` plan record's `(title, strategy)` pair, read back from the
    /// transient store.
    fn plan_fields(apg_root: &Path) -> (String, String) {
        specs::read_jsonl(&specs::plan_jsonl_path(apg_root, "foo"))
            .unwrap()
            .iter()
            .find_map(|r| match r {
                Record::Plan {
                    fqn,
                    title,
                    strategy,
                } if fqn == "foo/plan" => Some((title.clone(), strategy.clone())),
                _ => None,
            })
            .unwrap()
    }

    /// Unit: `plan_update_at` MERGEs title/strategy independently and preserves
    /// every phase/task/planned record and every plan edge
    /// (`Contains`/`Gates`/`Satisfies`/`Reviews`); an absent plan is refused.
    #[test]
    fn plan_update_at_merges_and_preserves_every_record_and_edge() {
        let (apg_root, repo, _wt) = fixture("plan-update-merge");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: "S0".to_string(),
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
                target: "github.com/x/y.Gateway".to_string(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".to_string(),
                to: "requirements.requirement.r1".to_string(),
            },
            Record::Gates {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-00".to_string(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "b".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // Every non-Plan record, serialized — must be byte-identical before
        // and after each update (phase/task/planned records + all plan edges).
        let others = |recs: &[Record]| -> Vec<String> {
            recs.iter()
                .filter(|r| !matches!(r, Record::Plan { .. }))
                .map(|r| serde_json::to_string(r).unwrap())
                .collect()
        };
        let others_before = others(&records);

        // `--title` alone: strategy unchanged.
        plan_update_at(&apg_root, "foo", Some("New"), None).unwrap();
        assert_eq!(
            plan_fields(&apg_root),
            ("New".to_string(), "S0".to_string())
        );
        assert_eq!(
            others(&specs::read_jsonl(&path).unwrap()),
            others_before,
            "a title-only update must preserve every record and edge"
        );

        // `--strategy` alone: title unchanged.
        plan_update_at(&apg_root, "foo", None, Some("S2")).unwrap();
        assert_eq!(
            plan_fields(&apg_root),
            ("New".to_string(), "S2".to_string())
        );
        assert_eq!(
            others(&specs::read_jsonl(&path).unwrap()),
            others_before,
            "a strategy-only update must preserve every record and edge"
        );

        // Both flags merge together.
        plan_update_at(&apg_root, "foo", Some("T3"), Some("S3")).unwrap();
        assert_eq!(plan_fields(&apg_root), ("T3".to_string(), "S3".to_string()));

        // An absent plan is refused, naming `apg plan add`.
        let err = plan_update_at(&apg_root, "ghost", Some("X"), None).unwrap_err();
        assert!(err.to_string().contains("apg plan add ghost"), "{err}");

        testutil::remove(&repo);
    }

    /// Int (fixture repo/branch DB): the plan-record CLI surface — `apg plan
    /// add <project>` creates and refuses an existing plan; `apg plan update`
    /// MERGEs title/strategy independently and refuses an absent plan; the
    /// retired `apg plan init` is an unknown subcommand.
    #[test]
    fn plan_add_update_cli_and_init_retirement() {
        let (apg_root, repo, wt) = fixture("plan-add-update-cli");

        // `apg plan init` is retired at dispatch: an unknown subcommand. The
        // match fails before any root resolution, so no cwd is needed.
        let err = cmd_plan(&["init".to_string(), "foo".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown apg plan subcommand: init"),
            "{err}"
        );

        // `apg plan add <project>` (no second positional) creates the plan.
        with_cwd(&wt, || {
            cmd_plan(&[
                "add".to_string(),
                "foo".to_string(),
                "--title".to_string(),
                "T1".to_string(),
                "--strategy".to_string(),
                "S1".to_string(),
            ])
        })
        .unwrap();
        assert_eq!(plan_fields(&apg_root), ("T1".to_string(), "S1".to_string()));

        // `apg plan add <project>` refuses when the plan already exists.
        let err = with_cwd(&wt, || {
            cmd_plan(&["add".to_string(), "foo".to_string()]).unwrap_err()
        });
        assert!(err.to_string().contains("already exists"), "{err}");

        // `apg plan update`: `--title` alone preserves strategy.
        with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "foo".to_string(),
                "--title".to_string(),
                "T2".to_string(),
            ])
        })
        .unwrap();
        assert_eq!(plan_fields(&apg_root), ("T2".to_string(), "S1".to_string()));

        // `--strategy` alone preserves title.
        with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "foo".to_string(),
                "--strategy".to_string(),
                "S2".to_string(),
            ])
        })
        .unwrap();
        assert_eq!(plan_fields(&apg_root), ("T2".to_string(), "S2".to_string()));

        // Updating an absent plan is refused, naming `apg plan add`.
        let err = with_cwd(&wt, || {
            cmd_plan(&[
                "update".to_string(),
                "ghost".to_string(),
                "--title".to_string(),
                "X".to_string(),
            ])
            .unwrap_err()
        });
        assert!(err.to_string().contains("apg plan add ghost"), "{err}");

        testutil::remove(&repo);
    }

    /// No `Feedback`/`Note` record is orphaned (each still has its
    /// `Reviews`/`Details` edge) and no plan-family edge points at a record
    /// that no longer exists.
    fn assert_no_orphans(records: &[Record]) {
        let node_fqns: BTreeSet<&str> = records.iter().filter_map(artifacts::node_fqn).collect();
        for r in records {
            if let Record::Feedback { fqn, .. } = r {
                assert!(
                    records
                        .iter()
                        .any(|e| matches!(e, Record::Reviews { from, .. } if from == fqn)),
                    "orphan Feedback `{fqn}` (no Reviews edge)"
                );
            }
            if let Record::Note { fqn, .. } = r {
                assert!(
                    records
                        .iter()
                        .any(|e| matches!(e, Record::Details { from, .. } if from == fqn)),
                    "orphan Note `{fqn}` (no Details edge)"
                );
            }
            if let Some((from, to)) = artifacts::edge_endpoints(r) {
                for endpoint in [from, to] {
                    // Durable requirement FQNs are not in the transient plan
                    // store; every plan-family endpoint must resolve.
                    if endpoint.starts_with("foo/plan") || endpoint.starts_with("foo/feedback-") {
                        assert!(
                            node_fqns.contains(endpoint),
                            "edge endpoint `{endpoint}` points at a removed record"
                        );
                    }
                }
            }
        }
    }

    /// Unit: every plan/phase/task/planned rm refuses while the entity still
    /// has a dependent (plan-with-phases, phase-with-tasks, `done` or
    /// feedback-bearing task, creates-targeted planned node), naming the
    /// dependent plus the `--force` escape; a non-existent entity is a hard
    /// error; every refusal leaves the on-disk JSONL byte-identical.
    #[test]
    fn plan_rm_refusals_name_dependents_and_leave_the_store_intact() {
        let (apg_root, repo, _wt) = fixture("rm-refusal");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
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
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: "github.com/x/y".to_string(),
            },
            Record::Contains {
                from: "github.com/x/y".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        // A plan with a phase refuses, naming the phase + --force.
        let err = plan_rm_at(&apg_root, "foo", false).unwrap_err().to_string();
        assert!(err.contains("foo/plan.phase-01"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A phase with a task refuses, naming the task + --force.
        let err = plan_rm_phase_at(&apg_root, "foo", 1, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo/plan.phase-01.task-1"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A done task refuses, naming the status + --force.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let before_done = std::fs::read_to_string(&path).unwrap();
        let err = plan_rm_task_at(&apg_root, "foo", 1, 1, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("done"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_done);

        // A pending task with incident feedback refuses, naming the feedback.
        plan_undone_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let mut recs = specs::read_jsonl(&path).unwrap();
        recs.push(Record::Feedback {
            fqn: "foo/feedback-1".to_string(),
            body: "b".to_string(),
            status: "open".to_string(),
            disposition: String::new(),
        });
        recs.push(Record::Reviews {
            from: "foo/feedback-1".to_string(),
            to: "foo/plan.phase-01.task-1".to_string(),
        });
        specs::write_jsonl(&path, &recs).unwrap();
        let before_fb = std::fs::read_to_string(&path).unwrap();
        let err = plan_rm_task_at(&apg_root, "foo", 1, 1, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo/feedback-1"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_fb);

        // A creates-targeted planned node refuses, naming the task + --force.
        let err = plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Store", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo/plan.phase-01.task-1"), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_fb);

        // Non-existent entities are hard errors, never a silent no-op.
        assert!(plan_rm_at(&apg_root, "ghost", true).is_err());
        assert!(plan_rm_phase_at(&apg_root, "foo", 9, false).is_err());
        assert!(plan_rm_task_at(&apg_root, "foo", 1, 9, false).is_err());
        assert!(plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Nope", false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before_fb);

        testutil::remove(&repo);
    }

    /// Unit: `--force` cascades leave no orphan records — no task without its
    /// phase, no edge to a removed plan/phase/task/planned/Feedback/Note; a
    /// phase whose only dependents are Feedback/Note is removable WITHOUT
    /// `--force` (its Reviews/Details edges and the dependent Feedback/Note
    /// records go with it); a plan `--force` cascade removes the plan-level
    /// Feedback/Note records and deletes the emptied store.
    #[test]
    fn plan_rm_cascades_leave_no_orphan_records() {
        let (apg_root, repo, _wt) = fixture("rm-cascade");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            // Plan-level (structural) feedback: removed only by the plan rm.
            Record::Feedback {
                fqn: "foo/feedback-plan".to_string(),
                body: "structural".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-plan".to_string(),
                to: "foo/plan".to_string(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".to_string(),
                number: 1,
                title: "P1".to_string(),
                deliverable: "D".to_string(),
                status: "pending".to_string(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".to_string(),
                number: 2,
                title: "P2".to_string(),
                deliverable: "D2".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-02".to_string(),
            },
            Record::Gates {
                from: "foo/plan.phase-02".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".to_string(),
                to: "requirements.requirement.r1".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: "github.com/x/y".to_string(),
            },
            Record::Contains {
                from: "github.com/x/y".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
            // Task-level feedback: removed by the task cascade.
            Record::Feedback {
                fqn: "foo/feedback-task".to_string(),
                body: "task issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-task".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            // Phase-level feedback + a task note: both removed by a phase rm.
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "phase issue".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Note {
                fqn: "foo/plan.note-1".to_string(),
                body: "note".to_string(),
                kind: "note".to_string(),
            },
            Record::Details {
                from: "foo/plan.note-1".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // The creates-targeted planned node refuses without --force; --force
        // removes it plus its parent Contains edge, leaving no dangling edge.
        assert!(plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Store", false).is_err());
        plan_rm_planned_at(&apg_root, "foo", "github.com/x/y.Store", true).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(!recs.iter().any(
            |r| matches!(r, Record::PlannedNode { fqn, .. } if fqn == "github.com/x/y.Store")
        ));
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Contains { to, .. } if to == "github.com/x/y.Store"))
        );
        assert_no_orphans(&recs);

        // The feedback-bearing task refuses without --force; --force removes
        // the task, its phase Contains edge and the now-orphaned task feedback.
        assert!(plan_rm_task_at(&apg_root, "foo", 1, 1, false).is_err());
        plan_rm_task_at(&apg_root, "foo", 1, 1, true).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(
            !recs.iter().any(|r| matches!(r, Record::Task { .. })),
            "no task may survive its phase's task cascade"
        );
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-task")),
            "the task's feedback must not survive as an orphan"
        );
        assert_no_orphans(&recs);

        // A phase whose only dependents are Feedback/Note is removable WITHOUT
        // --force: its incident edges and the dependent records go with it.
        plan_rm_phase_at(&apg_root, "foo", 1, false).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::PlanPhase { fqn, .. } if fqn == "foo/plan.phase-01"))
        );
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-1"))
        );
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Note { fqn, .. } if fqn == "foo/plan.note-1"))
        );
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, Record::Gates { from, .. } if from == "foo/plan.phase-02")),
            "the phase-02 gate on the removed phase must go too"
        );
        assert_no_orphans(&recs);

        // The plan still carries phase-02, so a plain plan rm refuses...
        assert!(plan_rm_at(&apg_root, "foo", false).is_err());

        // ...and a plan --force cascade removes the plan-level Feedback too and
        // deletes the emptied store (an empty file would block `plan add`).
        plan_rm_at(&apg_root, "foo", true).unwrap();
        assert!(!path.exists(), "an emptied plan store must be deleted");
        assert!(
            plan_rm_at(&apg_root, "foo", true).is_err(),
            "rm of the now-absent plan is a hard error"
        );

        testutil::remove(&repo);
    }

    /// Unit: an error mid-cascade leaves the original plan file exactly as it
    /// was — the whole-record rewrite is atomic, never a half-deleted plan.
    #[test]
    fn plan_rm_error_mid_cascade_leaves_the_store_untouched() {
        let (apg_root, repo, _wt) = fixture("rm-atomic");
        let path = write_plan(&apg_root);
        let before = std::fs::read(&path).unwrap();

        // Force the single write-through to fail: record a scan_meta that does
        // not match the live git state, so the stale gate refuses the re-ingest
        // after the whole cascade has been computed in memory.
        testutil::write_scan_meta(
            &apg_root,
            Some("0000000000000000000000000000000000000000"),
            true,
            "2026-09-07T00:00:00Z",
        );

        let err = plan_rm_at(&apg_root, "foo", true).unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a mid-cascade failure must leave the plan store byte-identical"
        );

        testutil::remove(&repo);
    }

    /// Int (fixture repo/branch DB): the `apg plan rm` CLI surface — a
    /// dependent-bearing plan/phase refuses (non-zero, naming the dependent
    /// plus the `--force` escape), the same rm with `--force` cascades, an
    /// absent entity is a hard error, and `rm plan --force` deletes the JSONL
    /// so a following `apg plan add` recreates it (rm→add round-trip).
    #[test]
    fn plan_rm_cli_refusal_force_and_rm_add_roundtrip() {
        let (apg_root, repo, wt) = fixture("rm-cli");
        let path = write_plan(&apg_root);

        // A plan with a phase refuses, naming the phase + --force.
        let err = with_cwd(&wt, || {
            cmd_plan(&["rm".to_string(), "foo".to_string()]).unwrap_err()
        });
        assert!(err.to_string().contains("foo/plan.phase-01"), "{err}");
        assert!(err.to_string().contains("--force"), "{err}");
        assert!(path.exists());

        // A phase with a task refuses, naming the task + --force.
        let err = with_cwd(&wt, || {
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "1".to_string(),
            ])
            .unwrap_err()
        });
        assert!(
            err.to_string().contains("foo/plan.phase-01.task-1"),
            "{err}"
        );
        assert!(err.to_string().contains("--force"), "{err}");

        // A done task refuses without --force, cascades with it.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        let err = with_cwd(&wt, || {
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "task".to_string(),
                "1".to_string(),
                "1".to_string(),
            ])
            .unwrap_err()
        });
        assert!(err.to_string().contains("done"), "{err}");
        with_cwd(&wt, || {
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "task".to_string(),
                "1".to_string(),
                "1".to_string(),
                "--force".to_string(),
            ])
        })
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(!recs.iter().any(|r| matches!(r, Record::Task { .. })));

        // A phase with no tasks removes without --force.
        with_cwd(&wt, || {
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "1".to_string(),
            ])
        })
        .unwrap();
        assert!(
            !specs::read_jsonl(&path)
                .unwrap()
                .iter()
                .any(|r| matches!(r, Record::PlanPhase { .. }))
        );

        // `rm foo --force` deletes the whole plan JSONL...
        with_cwd(&wt, || {
            cmd_plan(&["rm".to_string(), "foo".to_string(), "--force".to_string()])
        })
        .unwrap();
        assert!(
            !path.exists(),
            "a plan --force rm must delete the plan JSONL"
        );

        // ...so the following `apg plan add foo` recreates it (rm→add).
        with_cwd(&wt, || cmd_plan(&["add".to_string(), "foo".to_string()])).unwrap();
        assert!(path.exists(), "apg plan add must recreate the removed plan");

        // A non-existent entity is a hard error.
        let err = with_cwd(&wt, || {
            cmd_plan(&[
                "rm".to_string(),
                "foo".to_string(),
                "phase".to_string(),
                "9".to_string(),
            ])
            .unwrap_err()
        });
        assert!(err.to_string().contains("no phase 9"), "{err}");
        let err = with_cwd(&wt, || {
            cmd_plan(&["rm".to_string(), "ghost".to_string()]).unwrap_err()
        });
        assert!(
            err.to_string().contains("no plan for project `ghost`"),
            "{err}"
        );

        testutil::remove(&repo);
    }
}
