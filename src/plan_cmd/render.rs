//! The `apg plan render` arm: PLAN.md-style markdown with the strategy, the
//! phase table, and each phase's tasks as a checkable list.

use crate::artifacts::parse_args;
use crate::schema::Record;
use crate::specs;

use super::{load_plan, require_apg_root};

/// `apg plan render <project> [--out <path>|-]` — PLAN.md-style markdown with
/// the strategy, the phase table, and each phase's tasks as a checkable list.
pub(crate) fn plan_render(args: &[String]) -> anyhow::Result<()> {
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
pub(crate) fn render_phase_tasks(records: &[Record], pfqn: &str) -> String {
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

/// Render a plan FQN without its `{project}/` prefix.
fn short_id(fqn: &str, project: &str) -> String {
    let prefix = format!("{project}/");
    fqn.strip_prefix(&prefix).unwrap_or(fqn).to_string()
}
