//! The `apg plan` authoring surface: the record/phase/task/planned
//! `add`/`update`/`rm` arms, the task verb/target validation, and the phase
//! `Gates`/`Satisfies` edge machinery.

use std::collections::BTreeSet;
use std::path::Path;

use crate::artifacts::{self, parse_args};
use crate::layers::NodeProperties;
use crate::schema::Record;
use crate::specs;

use super::{load_plan, plan_project, require_apg_root, spec_has_requirement, write_through};

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
pub fn plan_init_at(
    apg_root: &Path,
    project: &str,
    title: &str,
    strategy: &str,
) -> anyhow::Result<bool> {
    let path = specs::plan_jsonl_path(apg_root, project);
    if path.exists() {
        anyhow::bail!(
            "plan for `{project}` already exists at {} — use `apg plan update {project}` to change it or `apg plan rm {project}` to remove it",
            path.display()
        );
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
pub fn plan_update_at(
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
/// `apg plan update <project> task <phase> <k> [--title …]
/// [--assigned-to <agent-fqn>] [--implements <component-fqn>]
/// [--<kind>-<create|modify|delete|move|rename> <fqn>]… [--target …]` /
/// `apg plan update <project> planned <fqn> [--kind …] [--name …] [--parent …]`.
///
/// A missing flag leaves that dimension unchanged: the plan-record arm MERGEs
/// title/strategy, the phase/task arms update in place (every phase/task and
/// every edge survives — except the phase's replaced Satisfies/Gates sets and
/// the task's replaced assignment/operation edges), and the planned arm
/// repoints the parent `Contains` edge. Every arm refuses an absent target. On
/// the task arm a supplied `--assigned-to`/`--implements`/operation flag set
/// replaces that edge dimension (an omitted one leaves the existing edges
/// untouched); a renames/moves operation edge takes its destination from
/// `--target`. `apg plan link` is retired: its bridge set-semantics live here
/// (`phase` + `--satisfies`/`--prereq`).
pub(crate) fn plan_update(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!(
            "usage: apg plan update <project> [phase <n>|task <phase> <k>|planned <fqn>] [--title T] [--strategy S] …"
        );
    };
    let apg_root = require_apg_root()?;
    let _lock = artifacts::acquire_spec_lock(&apg_root)?;
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
                    "usage: apg plan update <project> task <phase> <k> [--title …] [--assigned-to <agent-fqn>] [--implements <component-fqn>] [--<kind>-<create|modify|delete|move|rename> <fqn>]… [--target <destination-fqn>]"
                );
            };
            // An omitted dimension leaves its edge set untouched; a supplied
            // one replaces it (an empty operation set is a target-less
            // activity).
            let assigned = p.has("assigned-to").then(|| p.all("assigned-to"));
            let implements = p.get("implements");
            let operations = task_operations_supplied(&p)
                .then(|| parse_task_operations(&p))
                .transpose()?;
            plan_update_task_at(
                &apg_root,
                project,
                &mut records,
                phase,
                k,
                p.get("title").as_deref(),
                assigned.as_deref(),
                implements.as_deref(),
                operations.as_deref(),
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
pub fn plan_update_phase_at(
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
/// its `status` and every incident `Reviews` edge. A supplied `assigned` or
/// `implements` dimension replaces that edge set; a supplied `operations`
/// dimension replaces the task's whole operation edge set (an empty slice is a
/// target-less activity, an omitted slice leaves the edges untouched). Every
/// effective dimension is re-validated exactly as `add` does — see
/// [`validate_task_assignment`] and [`validate_task_operations`] — so an update
/// can never introduce a dangling assignment or an invalid operation. An absent
/// task is refused before any mutation.
#[allow(clippy::too_many_arguments)]
pub fn plan_update_task_at(
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
    phase: u32,
    k: u32,
    title: Option<&str>,
    assigned: Option<&[String]>,
    implements: Option<&str>,
    operations: Option<&[TaskOperation]>,
) -> anyhow::Result<()> {
    let fqn = format!("{project}/plan.phase-{phase:02}.task-{k}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::Task { fqn: tf, .. } if tf == &fqn))
    {
        anyhow::bail!(
            "update task: `{fqn}` is not a task of `{project}` (author it first with `apg plan add {project} task {phase} {k} …`)"
        );
    }
    // Effective dimensions: each supplied value, else the stored edge set.
    let cur_assigned: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::AssignedTo { from, to } if from == &fqn => Some(to.clone()),
            _ => None,
        })
        .collect();
    let cur_implements = records.iter().find_map(|r| match r {
        Record::Implements { from, to } if from == &fqn => Some(to.clone()),
        _ => None,
    });
    let cur_operations = task_operations_from_records(records, &fqn);
    let eff_assigned: &[String] = assigned.unwrap_or(&cur_assigned);
    let eff_implements = match implements {
        Some("") => None,
        Some(component) => Some(component.to_string()),
        None => cur_implements,
    };
    let eff_operations: &[TaskOperation] = operations.unwrap_or(&cur_operations);
    let (agents, components) = task_assignment_universes(apg_root)?;
    validate_task_assignment(
        eff_assigned,
        eff_implements.as_deref(),
        &agents,
        &components,
    )?;
    let (scanned, planned) = task_verb_universes(apg_root, records)?;
    validate_task_operations(eff_operations, &scanned, &planned)?;
    // Rewrite only the dimensions the update supplies, leaving every other
    // incident edge — `status` and `Reviews` included — untouched.
    if let Some(assigned) = assigned {
        records.retain(|r| !matches!(r, Record::AssignedTo { from, .. } if from == &fqn));
        for agent in assigned {
            records.push(Record::AssignedTo {
                from: fqn.clone(),
                to: agent.clone(),
            });
        }
    }
    if let Some(component) = implements {
        records.retain(|r| !matches!(r, Record::Implements { from, .. } if from == &fqn));
        if !component.is_empty() {
            records.push(Record::Implements {
                from: fqn.clone(),
                to: component.to_string(),
            });
        }
    }
    if let Some(operations) = operations {
        records.retain(|r| {
            !(task_operation_of(r).is_some_and(|(_, _, from, _)| from == fqn)
                || matches!(r, Record::Target { from, .. } if from == &fqn))
        });
        push_task_operations(records, &fqn, operations);
    }
    for r in records.iter_mut() {
        if let Record::Task {
            fqn: tf, title: t, ..
        } = r
            && tf == &fqn
            && let Some(x) = title
        {
            *t = x.to_string();
        }
    }
    Ok(())
}

/// Core of the `update planned` arm: update a planned node's kind/name/parent
/// IN PLACE, repointing the parent `Contains` edge (set-semantics for parent)
/// while preserving every other incident edge. Refuses a real code FQN (a
/// realized placeholder is no longer a plan) and an absent planned node.
pub fn plan_update_planned_at(
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
            properties: NodeProperties::default(),
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
pub(crate) fn plan_rm(args: &[String]) -> anyhow::Result<()> {
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
/// edge ([`artifacts::remove_node`]), then garbage-collect any `Note` record
/// left with no remaining `Details` edge. `Feedback` is the deliberate
/// exception: its record — and the `Reviews` edge naming what it reviewed —
/// outlives the removed target, because a review item is closed by a reviewer,
/// never dropped by the removal of the node it reviews
/// (feedback-persists-across-target-loss). Pure: the caller rewrites the store
/// exactly once ([`persist_rm`]), so all the cascade work is in memory and any
/// failure before that single write leaves the on-disk plan untouched
/// (plan-rm-atomic).
pub(crate) fn cascade_remove(records: &mut Vec<Record>, fqns: &[String]) {
    // `artifacts::remove_node` strips EVERY edge incident to a removed node —
    // including the `Reviews` edge whose `to` is that node. Capture those
    // references first and re-install them after the strip, so an orphaned
    // Feedback still names the target it reviews.
    let removed: BTreeSet<&str> = fqns.iter().map(String::as_str).collect();
    let retained_reviews: Vec<Record> = records
        .iter()
        .filter(|r| matches!(r, Record::Reviews { to, .. } if removed.contains(to.as_str())))
        .cloned()
        .collect();
    for fqn in fqns {
        artifacts::remove_node(records, fqn);
    }
    records.extend(retained_reviews);
    // A Note whose only attachment was a removed node is an orphan: its
    // Details edge is gone, so the record must go too.
    let detailed: BTreeSet<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Details { from, .. } => Some(from.clone()),
            _ => None,
        })
        .collect();
    records.retain(|r| !matches!(r, Record::Note { fqn, .. } if !detailed.contains(fqn.as_str())));
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
/// every phase, task and planned node, every incident edge, and every Note
/// orphaned by the removal — in one in-memory pass. Feedback records (and their
/// `Reviews` edges) survive deliberately ([`cascade_remove`]), so
/// [`persist_rm`] deletes the store only when the cascade left it empty: a
/// store kept alive by surviving Feedback persists, and a later `apg plan add
/// <project>` refuses on the existing file. An absent plan is an error and the
/// stored file is left untouched (nothing is written before the whole cascade
/// is computed).
pub fn plan_rm_at(apg_root: &Path, project: &str, force: bool) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
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
/// every incident edge and every Note orphaned by the removal (its Feedback
/// records survive, [`cascade_remove`]); the `--force` path also removes the
/// phase's tasks. A phase whose only dependents are Feedback/Note is removable
/// WITHOUT `--force`. An absent phase is an error.
pub fn plan_rm_phase_at(apg_root: &Path, project: &str, n: u32, force: bool) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
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
/// incident Feedback (naming the status/Feedback and the `--force` escape);
/// `--force` cascades the task, its Contains edge, and any Note orphaned by the
/// removal, while its Feedback records (and their `Reviews` edges) survive
/// deliberately ([`cascade_remove`]). An absent task is an error.
pub fn plan_rm_task_at(
    apg_root: &Path,
    project: &str,
    phase: u32,
    k: u32,
    force: bool,
) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
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
///
/// A creating task is recognised by the `{kind}_create` operation edge it
/// carries (SCHEMA.md task edges).
pub fn plan_rm_planned_at(
    apg_root: &Path,
    project: &str,
    fqn: &str,
    force: bool,
) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    if !records
        .iter()
        .any(|r| matches!(r, Record::PlannedNode { fqn: pf, .. } if pf == fqn))
    {
        anyhow::bail!("no planned node `{fqn}` in plan `{project}` — nothing to remove");
    }
    let mut creators: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::ModuleCreate { from, to }
            | Record::FileCreate { from, to }
            | Record::StructCreate { from, to }
            | Record::FunctionCreate { from, to }
                if to == fqn =>
            {
                Some(from.clone())
            }
            _ => None,
        })
        .collect();
    creators.sort();
    creators.dedup();
    if !creators.is_empty() && !force {
        anyhow::bail!(
            "planned node `{fqn}` is targeted by a creates task: {} — pass --force to remove the planned node and its parent Contains edge",
            creators.join(", ")
        );
    }
    cascade_remove(&mut records, &[fqn.to_string()]);
    persist_rm(apg_root, project, &records)
}

/// The per-kind operation-edge flags: `--<kind>-<verb> <fqn>` builds the
/// SCHEMA.md `<kind>_<verb>` edge (`--module-create` → `module_create`). A
/// renames/moves flag takes the live source FQN; `--target` supplies its
/// planned destination.
const TASK_OPERATION_FLAGS: [(&str, &str, &str); 20] = [
    ("module-create", "module", "creates"),
    ("module-modify", "module", "modifies"),
    ("module-delete", "module", "deletes"),
    ("module-move", "module", "moves"),
    ("module-rename", "module", "renames"),
    ("file-create", "file", "creates"),
    ("file-modify", "file", "modifies"),
    ("file-delete", "file", "deletes"),
    ("file-move", "file", "moves"),
    ("file-rename", "file", "renames"),
    ("struct-create", "struct", "creates"),
    ("struct-modify", "struct", "modifies"),
    ("struct-delete", "struct", "deletes"),
    ("struct-move", "struct", "moves"),
    ("struct-rename", "struct", "renames"),
    ("function-create", "function", "creates"),
    ("function-modify", "function", "modifies"),
    ("function-delete", "function", "deletes"),
    ("function-move", "function", "moves"),
    ("function-rename", "function", "renames"),
];

/// Read a task's per-kind operation edges from the CLI arguments. Every
/// operation flag contributes one edge; `--target` is the renames/moves
/// destination and is refused without one.
fn parse_task_operations(p: &artifacts::ParsedArgs) -> anyhow::Result<Vec<TaskOperation>> {
    let destination = p.get("target").unwrap_or_default();
    let mut operations = Vec::new();
    for (flag, kind, verb) in TASK_OPERATION_FLAGS {
        for target in p.all(flag) {
            operations.push(TaskOperation {
                kind: kind.to_string(),
                verb: verb.to_string(),
                target,
                new_fqn: if matches!(verb, "renames" | "moves") {
                    destination.clone()
                } else {
                    String::new()
                },
            });
        }
    }
    if !destination.is_empty()
        && !operations
            .iter()
            .any(|op| matches!(op.verb.as_str(), "renames" | "moves"))
    {
        anyhow::bail!(
            "--target <destination-fqn> is only valid for a renames/moves operation edge"
        );
    }
    Ok(operations)
}

/// Whether any operation-edge argument was supplied (so an update replaces the
/// stored edge set rather than leaving it untouched).
fn task_operations_supplied(p: &artifacts::ParsedArgs) -> bool {
    p.has("target") || TASK_OPERATION_FLAGS.iter().any(|(flag, _, _)| p.has(flag))
}

/// `apg plan add <project>` (the plan itself — no second positional) /
/// `apg plan add <project> phase <n> …` / `task <phase> <k> …` /
/// `planned <kind> <fqn> …` (R23, PHASE_02). The create arm refuses when the
/// plan already exists and warns when the requirement tier is empty.
pub(crate) fn plan_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan add <project> [phase|task|planned] …");
    };
    let apg_root = require_apg_root()?;
    let _lock = artifacts::acquire_spec_lock(&apg_root)?;
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
                    "usage: apg plan add <project> task <phase> <k> --title … --assigned-to <agent-fqn> [--implements <component-fqn>] [--<kind>-<create|modify|delete|move|rename> <fqn>]… [--target <destination-fqn>]"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("task requires --title");
            };
            let assigned = p.all("assigned-to");
            let implements = p.get("implements");
            let operations = parse_task_operations(&p)?;
            plan_add_task_at(
                &apg_root,
                project,
                &mut records,
                phase,
                k,
                &title,
                &assigned,
                implements.as_deref(),
                &operations,
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
pub fn plan_add_phase_at(
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
        properties: NodeProperties::default(),
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
/// the task's assignment and its operation edge set, and appends the Task +
/// `assigned-to`/`implements` + per-kind operation records + Contains.
///
/// The Task record stores its title+status alone; the assignment and the
/// operation ride beside it as the SCHEMA.md task edges: exactly one
/// `assigned-to` Agent, at most one `implements` Component, and one per-kind
/// operation edge per operation — `creates` → `{kind}_create` to the planned
/// destination, `modifies`/`deletes` → `{kind}_{modify,delete}` to the live
/// target, `renames`/`moves` → `{kind}_{rename,move}` to the live source plus
/// a `target` edge to the planned destination. An empty operation set is a
/// target-less activity.
#[allow(clippy::too_many_arguments)]
pub fn plan_add_task_at(
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
    phase: u32,
    k: u32,
    title: &str,
    assigned: &[String],
    implements: Option<&str>,
    operations: &[TaskOperation],
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
    let (agents, components) = task_assignment_universes(apg_root)?;
    validate_task_assignment(assigned, implements, &agents, &components)?;
    let (scanned, planned) = task_verb_universes(apg_root, records)?;
    validate_task_operations(operations, &scanned, &planned)?;
    let mut recs = vec![Record::Task {
        fqn: fqn.clone(),
        title: title.to_string(),
        status: "pending".to_string(),
    }];
    for agent in assigned {
        recs.push(Record::AssignedTo {
            from: fqn.clone(),
            to: agent.clone(),
        });
    }
    if let Some(component) = implements.filter(|c| !c.is_empty()) {
        recs.push(Record::Implements {
            from: fqn.clone(),
            to: component.to_string(),
        });
    }
    push_task_operations(&mut recs, &fqn, operations);
    recs.push(Record::Contains {
        from: phase_fqn,
        to: fqn.clone(),
        properties: NodeProperties::default(),
    });
    records.extend(recs);
    Ok(())
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
pub fn plan_add_planned_at(
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
            properties: NodeProperties::default(),
        });
    }
    Ok(())
}

/// One per-kind operation edge a task carries: `<kind>_<verb>` from the task to
/// `target` — the planned destination of a `creates`, the live target of a
/// `modifies`/`deletes`, the live source of a `renames`/`moves` — plus the
/// renames/moves `target` edge destination in `new_fqn`. An empty operation set
/// is a target-less activity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskOperation {
    pub kind: String,
    pub verb: String,
    pub target: String,
    pub new_fqn: String,
}

/// Validate a task's assignment: exactly one `assigned-to` Agent (any Agent is
/// assignable to any task — the roster is not partitioned by role), that Agent
/// present in the branch graph's Agent roster (the assigned-to drift universe),
/// and an optional `implements` Component drawn from the solution layer.
pub(crate) fn validate_task_assignment(
    assigned: &[String],
    implements: Option<&str>,
    agents: &BTreeSet<String>,
    components: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if assigned.len() != 1 {
        anyhow::bail!(
            "a task assigns exactly one Agent (got {}) — pass --assigned-to <agent-fqn> once",
            assigned.len()
        );
    }
    let agent = &assigned[0];
    if agent.is_empty() {
        anyhow::bail!("assigned-to requires an Agent FQN (`agent.<file-stem>`)");
    }
    if !agents.contains(agent) {
        anyhow::bail!(
            "assigned Agent `{agent}` is drift — it is absent from the branch graph's Agent roster (route the task to the agent-builder)"
        );
    }
    if let Some(component) = implements
        && !components.contains(component)
    {
        anyhow::bail!("implements target `{component}` is not a Component of the solution layer");
    }
    Ok(())
}

/// The Task→Implementation verbs (SPEC §5): how a task's operation edge
/// touches the Implementation tier. Further verbs may reveal themselves
/// through dogfooding.
const TASK_VERBS: [&str; 5] = ["creates", "modifies", "deletes", "renames", "moves"];

/// Validate a task's per-kind operation edge set against the two code-reference
/// universes: `scanned` = every real code FQN in the live graph, `planned` = the
/// planned-node universe (a `status: planned` DB node or a
/// `Record::PlannedNode` in `.trans/plans`). An empty set is a valid target-less
/// activity. Each operation's rules:
///
/// - `creates` — the target must NOT resolve in the scanned graph (it may be a
///   planned node or still absent); creating over existing real code is refused.
/// - `modifies` / `deletes` — the target must resolve in the scanned graph; an
///   unresolvable (or still-planned) FQN is refused.
/// - `renames` / `moves` — the source must resolve in the scanned graph and the
///   destination FQN must not collide with existing real code.
///
/// An operation edge with an empty target is refused (a target-less edge is not
/// a valid operation). Pure — no I/O; the caller supplies both universes.
pub(crate) fn validate_task_operations(
    operations: &[TaskOperation],
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for op in operations {
        if !["module", "file", "struct", "function"].contains(&op.kind.as_str()) {
            anyhow::bail!(
                "invalid operation kind `{}` — one of module/file/struct/function",
                op.kind
            );
        }
        if !TASK_VERBS.contains(&op.verb.as_str()) {
            anyhow::bail!(
                "invalid task verb `{}` — one of creates/modifies/deletes/renames/moves",
                op.verb
            );
        }
        if op.target.is_empty() {
            anyhow::bail!(
                "{} {} operation edge requires a target FQN",
                op.kind,
                op.verb
            );
        }
        if !seen.insert((op.kind.as_str(), op.verb.as_str())) {
            anyhow::bail!(
                "duplicate {} {} operation edge — a task carries at most one per kind and verb",
                op.kind,
                op.verb
            );
        }
        if !op.new_fqn.is_empty() && !matches!(op.verb.as_str(), "renames" | "moves") {
            anyhow::bail!(
                "--target <destination-fqn> is only valid for renames/moves tasks (got `{}`)",
                op.verb
            );
        }
        let status = crate::layers::classify_code_ref(&op.target, scanned, planned);
        match op.verb.as_str() {
            "creates" => {
                if status == crate::layers::CodeRefStatus::Real {
                    anyhow::bail!(
                        "creates target `{}` already resolves to scanned code — a creates builds a planned node; plan the delta, not the present",
                        op.target
                    );
                }
            }
            "modifies" | "deletes" => match status {
                crate::layers::CodeRefStatus::Real => {}
                crate::layers::CodeRefStatus::Pending => anyhow::bail!(
                    "{} target `{}` is a planned node, not scanned code — only a creates builds a planned node",
                    op.verb,
                    op.target
                ),
                crate::layers::CodeRefStatus::Drift => anyhow::bail!(
                    "{} target `{}` does not resolve in the scanned graph — {} changes existing code (a creates would plan it)",
                    op.verb,
                    op.target,
                    op.verb
                ),
            },
            "renames" | "moves" => {
                if op.new_fqn.is_empty() {
                    anyhow::bail!(
                        "{} {} operation edge requires --target <destination-fqn>",
                        op.kind,
                        op.verb
                    );
                }
                if status != crate::layers::CodeRefStatus::Real {
                    anyhow::bail!(
                        "{} source `{}` does not resolve in the scanned graph — {} changes an existing FQN",
                        op.verb,
                        op.target,
                        op.verb
                    );
                }
                if crate::layers::classify_code_ref(&op.new_fqn, scanned, planned)
                    == crate::layers::CodeRefStatus::Real
                {
                    anyhow::bail!(
                        "{} target `{}` already resolves to scanned code — the new FQN must not collide with existing code",
                        op.verb,
                        op.new_fqn
                    );
                }
            }
            _ => unreachable!("validated against TASK_VERBS"),
        }
    }
    Ok(())
}

/// The SCHEMA.md per-kind operation edge record for `op`, sourced at `from`;
/// `None` when the kind/verb pair is not a declared task edge.
fn task_operation_record(from: &str, op: &TaskOperation) -> Option<Record> {
    use Record::*;
    let (from, to) = (from.to_string(), op.target.clone());
    Some(match (op.kind.as_str(), op.verb.as_str()) {
        ("module", "creates") => ModuleCreate { from, to },
        ("module", "modifies") => ModuleModify { from, to },
        ("module", "deletes") => ModuleDelete { from, to },
        ("module", "moves") => ModuleMove { from, to },
        ("module", "renames") => ModuleRename { from, to },
        ("file", "creates") => FileCreate { from, to },
        ("file", "modifies") => FileModify { from, to },
        ("file", "deletes") => FileDelete { from, to },
        ("file", "moves") => FileMove { from, to },
        ("file", "renames") => FileRename { from, to },
        ("struct", "creates") => StructCreate { from, to },
        ("struct", "modifies") => StructModify { from, to },
        ("struct", "deletes") => StructDelete { from, to },
        ("struct", "moves") => StructMove { from, to },
        ("struct", "renames") => StructRename { from, to },
        ("function", "creates") => FunctionCreate { from, to },
        ("function", "modifies") => FunctionModify { from, to },
        ("function", "deletes") => FunctionDelete { from, to },
        ("function", "moves") => FunctionMove { from, to },
        ("function", "renames") => FunctionRename { from, to },
        _ => return None,
    })
}

/// A task operation record as `(kind, verb, from, to)`; `None` for every other
/// record (the `target` destination edge included).
fn task_operation_of(r: &Record) -> Option<(&'static str, &'static str, &str, &str)> {
    use Record::*;
    Some(match r {
        ModuleCreate { from, to } => ("module", "creates", from, to),
        ModuleModify { from, to } => ("module", "modifies", from, to),
        ModuleDelete { from, to } => ("module", "deletes", from, to),
        ModuleMove { from, to } => ("module", "moves", from, to),
        ModuleRename { from, to } => ("module", "renames", from, to),
        FileCreate { from, to } => ("file", "creates", from, to),
        FileModify { from, to } => ("file", "modifies", from, to),
        FileDelete { from, to } => ("file", "deletes", from, to),
        FileMove { from, to } => ("file", "moves", from, to),
        FileRename { from, to } => ("file", "renames", from, to),
        StructCreate { from, to } => ("struct", "creates", from, to),
        StructModify { from, to } => ("struct", "modifies", from, to),
        StructDelete { from, to } => ("struct", "deletes", from, to),
        StructMove { from, to } => ("struct", "moves", from, to),
        StructRename { from, to } => ("struct", "renames", from, to),
        FunctionCreate { from, to } => ("function", "creates", from, to),
        FunctionModify { from, to } => ("function", "modifies", from, to),
        FunctionDelete { from, to } => ("function", "deletes", from, to),
        FunctionMove { from, to } => ("function", "moves", from, to),
        FunctionRename { from, to } => ("function", "renames", from, to),
        _ => return None,
    })
}

/// Append a task's operation edge records — plus one `target` destination edge
/// per distinct renames/moves destination — to `records`.
fn push_task_operations(records: &mut Vec<Record>, fqn: &str, operations: &[TaskOperation]) {
    let mut destinations: BTreeSet<&str> = BTreeSet::new();
    for op in operations {
        if let Some(rec) = task_operation_record(fqn, op) {
            records.push(rec);
        }
        if matches!(op.verb.as_str(), "renames" | "moves") && destinations.insert(&op.new_fqn) {
            records.push(Record::Target {
                from: fqn.to_string(),
                to: op.new_fqn.clone(),
            });
        }
    }
}

/// Read a task's stored operation edge set back into [`TaskOperation`]s; the
/// task's `target` destination edge fills a renames/moves `new_fqn`.
fn task_operations_from_records(records: &[Record], fqn: &str) -> Vec<TaskOperation> {
    let destination = records
        .iter()
        .find_map(|r| match r {
            Record::Target { from, to } if from == fqn => Some(to.clone()),
            _ => None,
        })
        .unwrap_or_default();
    records
        .iter()
        .filter_map(|r| {
            let (kind, verb, from, to) = task_operation_of(r)?;
            (from == fqn).then(|| TaskOperation {
                kind: kind.to_string(),
                verb: verb.to_string(),
                target: to.to_string(),
                new_fqn: if matches!(verb, "renames" | "moves") {
                    destination.clone()
                } else {
                    String::new()
                },
            })
        })
        .collect()
}

/// The two assignment universes: `agents` = the branch graph's Agent roster
/// (`agent.<file-stem>` nodes; a missing DB is an empty roster, so every
/// assignment is drift), `components` = the solution layer's Component FQNs.
fn task_assignment_universes(
    apg_root: &Path,
) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut agents = BTreeSet::new();
    if apg_root.join(specs::TRANS).join("db.lbug").exists() {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        let conn = db.conn()?;
        for row in conn.query("MATCH (n:Agent) RETURN n.fqn")? {
            let fqn = row.first().map(|v| v.to_string()).unwrap_or_default();
            if !fqn.is_empty() {
                agents.insert(fqn);
            }
        }
    }
    let components = crate::layers::read_existing_nodes(apg_root)?
        .into_iter()
        .filter(|n| n.layer == "solution" && n.node_type == "component")
        .map(|n| crate::layers::fqn(crate::layers::Layer::Solution, &n.node_type, &n.name))
        .collect();
    Ok((agents, components))
}

/// The two universes `validate_task_operations` classifies a task's target
/// against: `scanned` = every real code FQN the last scan produced (a missing DB
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
pub(crate) fn link_phase_edges(
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
pub(crate) fn push_gate(from: &str, to: &str, records: &[Record]) -> anyhow::Result<()> {
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
