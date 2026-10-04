//! The `apg plan` status surface: the assertion/milestone commands
//! (`done`/`undone`/`note`/`complete`) over the transient plan JSONL.

use std::path::Path;

use crate::artifacts::{self, parse_args};
use crate::layers::NodeProperties;
use crate::schema::Record;

use super::{load_plan, require_apg_root, write_through};

/// `apg plan done <project> <task-fqn>` (PHASE_03) — the implementer's
/// assertion. Marks a task done with NO promotion and NO code-graph
/// verification: the plan's planned nodes stay declared until the
/// apply act (PlanCompletion-SPEC.md), whose coherence gate verifies every
/// planned node is realized against the merged graph before applying.
/// `apg plan undone` remains the reversal.
pub(crate) fn plan_done(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan done <project> <task-fqn>");
    };
    let apg_root = require_apg_root()?;
    plan_done_at(&apg_root, project, task_fqn)
}

/// Core of `plan_done`: flips a task's status to `done` in the transient plan
/// JSONL (assertion only — no promotion, no graph verification).
pub fn plan_done_at(apg_root: &Path, project: &str, task_fqn: &str) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
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
pub(crate) fn plan_undone(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan undone <project> <task-fqn>");
    };
    let apg_root = require_apg_root()?;
    plan_undone_at(&apg_root, project, task_fqn)
}

/// Core of `plan_undone`: flips a task's status back to `pending`.
pub fn plan_undone_at(apg_root: &Path, project: &str, task_fqn: &str) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
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
pub(crate) fn plan_note(args: &[String]) -> anyhow::Result<()> {
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
pub fn plan_note_at(
    apg_root: &Path,
    project: &str,
    task_fqn: &str,
    body: &str,
    kind: &str,
) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
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
        properties: NodeProperties::default(),
    });
    records.push(Record::Details {
        from: fqn.clone(),
        to: task_fqn.to_string(),
        properties: NodeProperties::default(),
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
pub(crate) fn plan_complete(args: &[String]) -> anyhow::Result<()> {
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
pub fn plan_complete_at(apg_root: &Path, project: &str, phase: u32) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;
    let mut records = load_plan(apg_root, project)?;
    let phase_fqn = format!("{project}/plan.phase-{phase:02}");

    let tasks: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Contains { from, to, .. } if from == &phase_fqn => Some(to.clone()),
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
