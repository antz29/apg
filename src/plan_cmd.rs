//! `apg plan` — the phased execution plan that bridges the spec (`future`) to
//! present code (SPEC R23). The plan is transient by design (R22): it lives in
//! the gitignored `apg/.trans/plans/<project>.jsonl` and is retired once the
//! final phase completes — the durable trace is the spec's `Implements` edges
//! and the retired `Future`s.

use std::path::{Path, PathBuf};

use crate::artifacts::{self, parse_args, reingest_project, remove_node};
use crate::schema::Record;
use crate::spec_cmd;
use crate::specs;

fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

fn load_plan(apg_root: &Path, project: &str) -> anyhow::Result<Vec<Record>> {
    let path = specs::plan_jsonl_path(apg_root, project);
    if !path.exists() {
        anyhow::bail!(
            "no plan for project `{project}` — run `apg plan init {project}` first"
        );
    }
    specs::read_jsonl(&path)
}

fn write_through(apg_root: &Path, project: &str, records: &[Record]) -> anyhow::Result<()> {
    specs::write_jsonl(&specs::plan_jsonl_path(apg_root, project), records)?;
    if let Err(e) = reingest_project(apg_root, project) {
        eprintln!("warning: write-through re-ingest failed: {e:#}");
    }
    Ok(())
}

pub fn cmd_plan(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg plan <init|add|link|done|undone|complete|render> …");
    };
    match sub {
        "init" => plan_init(&args[1..]),
        "add" => plan_add(&args[1..]),
        "link" => plan_link(&args[1..]),
        "done" => plan_done(&args[1..]),
        "undone" => plan_undone(&args[1..]),
        "complete" => plan_complete(&args[1..]),
        "render" => plan_render(&args[1..]),
        other => anyhow::bail!("unknown apg plan subcommand: {other}"),
    }
}

/// `apg plan init <project> [--title T] [--strategy S]` — the plan is only
/// ever for a project that has a spec.
fn plan_init(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan init <project> [--title T] [--strategy S]");
    };
    let apg_root = require_apg_root()?;
    let spec_path = specs::spec_jsonl_path(&apg_root, project);
    if !spec_path.exists() {
        anyhow::bail!(
            "no spec for `{project}` — author a spec first (`apg spec init {project}`)"
        );
    }
    let path = specs::plan_jsonl_path(&apg_root, project);
    if path.exists() {
        anyhow::bail!("plan for `{project}` already exists at {}", path.display());
    }
    let records = vec![Record::Plan {
        fqn: format!("future/{project}/plan"),
        title: p.get("title").unwrap_or_else(|| format!("Plan for {project}")),
        strategy: p.get("strategy").unwrap_or_default(),
    }];
    write_through(&apg_root, project, &records)?;
    println!("Created plan {project} at {}", path.display());
    Ok(())
}

/// `apg plan add <project> phase <n> …` / `task <phase> <k> …` (R23).
fn plan_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan add <project> <phase|task> …");
    };
    let Some(kind) = p.positional.get(1).map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg plan add <project> <phase|task> …");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_plan(&apg_root, project)?;
    let plan_fqn = format!("future/{project}/plan");
    match kind {
        "phase" => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!("usage: apg plan add <project> phase <n> --title … [--deliverable …] [--prereq <n>]* [--satisfies <req-id>]*");
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("phase requires --title");
            };
            let fqn = format!("future/{project}/plan.phase-{n:02}");
            let mut recs = vec![Record::PlanPhase {
                fqn: fqn.clone(),
                number: n,
                title,
                deliverable: p.get("deliverable").unwrap_or_default(),
            }];
            recs.push(Record::Contains {
                from: plan_fqn.clone(),
                to: fqn.clone(),
            });
            for g in p.all("prereq") {
                let g = g.parse::<u32>().map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
                recs.push(Record::Gates {
                    from: fqn.clone(),
                    to: format!("future/{project}/plan.phase-{g:02}"),
                });
            }
            for req in p.all("satisfies") {
                let req_fqn = format!("future/{project}/spec.{req}");
                if !spec_has_requirement(&apg_root, project, &req_fqn)? {
                    anyhow::bail!("satisfies target `{req}` is not a requirement of `{project}`");
                }
                recs.push(Record::Satisfies {
                    from: fqn.clone(),
                    to: req_fqn,
                });
            }
            remove_node(&mut records, &fqn);
            records.extend(recs);
            write_through(&apg_root, project, &records)?;
            println!("Added phase {n} to plan {project}");
        }
        "task" => {
            let (Some(phase), Some(k)) = (
                p.positional.get(2).and_then(|s| s.parse::<u32>().ok()),
                p.positional.get(3).and_then(|s| s.parse::<u32>().ok()),
            ) else {
                anyhow::bail!("usage: apg plan add <project> task <phase> <k> --title … [--tier …] [--builds <future-name>] [--anchor <fqn>]*");
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("task requires --title");
            };
            let fqn = format!("future/{project}/plan.phase-{phase:02}.task-{k}");
            let mut recs = vec![Record::Task {
                fqn: fqn.clone(),
                title,
                tier: p.get("tier").unwrap_or_else(|| "source".to_string()),
                status: "pending".to_string(),
            }];
            recs.push(Record::Contains {
                from: format!("future/{project}/plan.phase-{phase:02}"),
                to: fqn.clone(),
            });
            if let Some(future_name) = p.get("builds") {
                let future_fqn = format!("future/{project}/{future_name}");
                if !spec_has_future(&apg_root, project, &future_fqn)? {
                    anyhow::bail!(
                        "builds target `{future_name}` is not a declared future of `{project}` (declare it with `apg spec add future`)"
                    );
                }
                recs.push(Record::Builds {
                    from: fqn.clone(),
                    to: future_fqn,
                });
            }
            {
                let db = artifacts::ArtifactDb::open(&apg_root)?;
                for a in p.all("anchor") {
                    if db.code_label(&a).is_none() {
                        anyhow::bail!("task anchor `{a}` is not a resolved code node");
                    }
                    recs.push(Record::Anchors { from: fqn.clone(), to: a });
                }
            }
            remove_node(&mut records, &fqn);
            records.extend(recs);
            write_through(&apg_root, project, &records)?;
            println!("Added task {k} to plan.phase-{phase} of {project}");
        }
        other => anyhow::bail!("unknown plan add kind `{other}` — phase|task"),
    }
    Ok(())
}

/// `apg plan link <project> <phase-n> [--satisfies <req-id>]* [--prereq <n>]*`
/// — add/refresh the phase's bridge edges.
fn plan_link(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(phase)) = (
        p.positional.first(),
        p.positional.get(1).and_then(|s| s.parse::<u32>().ok()),
    ) else {
        anyhow::bail!("usage: apg plan link <project> <phase-n> [--satisfies <req-id>]* [--prereq <n>]*");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_plan(&apg_root, project)?;
    let phase_fqn = format!("future/{project}/plan.phase-{phase:02}");
    for req in p.all("satisfies") {
        let req_fqn = format!("future/{project}/spec.{req}");
        if !spec_has_requirement(&apg_root, project, &req_fqn)? {
            anyhow::bail!("satisfies target `{req}` is not a requirement of `{project}`");
        }
        artifacts::remove_incident_edges(&mut records, &phase_fqn);
        records.push(Record::Satisfies {
            from: phase_fqn.clone(),
            to: req_fqn,
        });
    }
    for g in p.all("prereq") {
        let g = g.parse::<u32>().map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
        artifacts::remove_incident_edges(&mut records, &phase_fqn);
        records.push(Record::Gates {
            from: phase_fqn.clone(),
            to: format!("future/{project}/plan.phase-{g:02}"),
        });
    }
    write_through(&apg_root, project, &records)?;
    println!("Linked plan.phase-{phase} of {project}");
    Ok(())
}

/// `apg plan done <project> <task-fqn>` (R23) — mark a task complete; that act
/// moves its portion of future work into the present: each `Builds` future is
/// verified against the code graph, promoted (re-anchor + `Implements` +
/// retire), then the task flips to `done`.
fn plan_done(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan done <project> <task-fqn>");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_plan(&apg_root, project)?;
    if !records.iter().any(|r| matches!(r, Record::Task { fqn, .. } if fqn == task_fqn)) {
        anyhow::bail!("task `{task_fqn}` not found in plan `{project}`");
    }
    let builds: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Builds { from, to } if from == task_fqn => Some(to.clone()),
            _ => None,
        })
        .collect();
    for future_fqn in builds {
        spec_cmd::promote_future(&apg_root, project, &future_fqn)?;
    }
    for r in &mut records {
        match r {
            Record::Task { fqn, status, .. } if fqn == task_fqn => {
                *status = "done".to_string();
            }
            _ => {}
        }
    }
    write_through(&apg_root, project, &records)?;
    println!("Task done: {task_fqn}");
    Ok(())
}

/// `apg plan undone <project> <task-fqn>` — a checklist correction; does not
/// recreate retired Futures (the code is already in the present).
fn plan_undone(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(task_fqn)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg plan undone <project> <task-fqn>");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_plan(&apg_root, project)?;
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
    write_through(&apg_root, project, &records)?;
    println!("Task undone: {task_fqn}");
    Ok(())
}

/// `apg plan complete <project> <phase-n>` (R23/R27). Requires every phase
/// task `done` and no unresolved feedback on the phase or its tasks; adds
/// `Implements` from the phase's built code (its done tasks' anchors) to each
/// `Satisfies`-ed requirement (idempotently). Completing the final phase
/// retires the plan and drops its JSONL (R22).
fn plan_complete(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(phase)) = (
        p.positional.first(),
        p.positional.get(1).and_then(|s| s.parse::<u32>().ok()),
    ) else {
        anyhow::bail!("usage: apg plan complete <project> <phase-n>");
    };
    let apg_root = require_apg_root()?;
    let records = load_plan(&apg_root, project)?;
    let phase_fqn = format!("future/{project}/plan.phase-{phase:02}");

    let tasks: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Contains { from, to } if from == &phase_fqn => Some(to.clone()),
            _ => None,
        })
        .filter(|t| records.iter().any(|r| matches!(r, Record::Task { fqn, .. } if fqn == t)))
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

    // Satisfies → Implements from the phase's built code (done tasks' anchors).
    // These are *spec* records: they serialize into apg/specs/<project>.jsonl
    // (the durable form), never the transient plan JSONL — the final-phase
    // complete retires the plan file, which would otherwise lose them (R18).
    let built = built_codes(&records, &tasks);
    let satisfied = satisfied_reqs(&records, &phase_fqn);
    if !built.is_empty() && !satisfied.is_empty() {
        let mut spec_records = spec_cmd::load_project(&apg_root, project)?;
        for req in &satisfied {
            // Idempotent Implements: replace only existing Implements edges on
            // the requirement (depends-on/anchors are untouched).
            spec_records.retain(|r| !matches!(r, Record::Implements { to, .. } if to == req));
            for code in &built {
                spec_records.push(Record::Implements {
                    from: code.clone(),
                    to: req.clone(),
                });
            }
        }
        spec_cmd::write_through(&apg_root, project, &spec_records)?;
    }
    write_through(&apg_root, project, &records)?;

    // Final phase complete → retire the plan (R22): drop the JSONL. The
    // durable record of the bridge is the spec's Implements edges + retired
    // Futures; the plan was the roadmap, not the record.
    let max_phase = records
        .iter()
        .filter_map(|r| match r {
            Record::PlanPhase { number, .. } => Some(*number),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    if phase >= max_phase {
        std::fs::remove_file(specs::plan_jsonl_path(&apg_root, project))?;
        if let Err(e) = reingest_project(&apg_root, project) {
            eprintln!("warning: write-through re-ingest failed: {e:#}");
        }
        println!("Completed final phase {phase} — plan {project} retired (JSONL dropped)");
    } else {
        println!("Completed phase {phase} of plan {project}");
    }
    Ok(())
}

/// The built code nodes of a phase: the unique set of its tasks' `Anchors`
/// (files/functions the tasks touched).
fn built_codes(records: &[Record], tasks: &[String]) -> Vec<String> {
    let mut built: Vec<String> = Vec::new();
    for t in tasks {
        for (a, _) in records.iter().filter_map(|e| match e {
            Record::Anchors { from, to } if from == t => Some((to.clone(), ())),
            _ => None,
        }) {
            if !built.contains(&a) {
                built.push(a);
            }
        }
    }
    built
}

/// The requirements a plan phase `Satisfies`.
fn satisfied_reqs(records: &[Record], phase_fqn: &str) -> Vec<String> {
    records
        .iter()
        .filter_map(|e| match e {
            Record::Satisfies { from, to } if from == phase_fqn => Some(to.clone()),
            _ => None,
        })
        .collect()
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
        Record::Plan { title, strategy, .. } => Some((title, strategy)),
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
    out.push_str("## Phases\n\n| Phase | Deliverable | Satisfies | Prereq |\n|---|---|---|---|\n");
    for n in &phases {
        let pfqn = format!("future/{project}/plan.phase-{n:02}");
        let deliverable = records
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase { fqn, deliverable, .. } if fqn == &pfqn => {
                    Some(deliverable.clone())
                }
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
            "| {n} | {deliverable} | {} | {} |\n",
            satisfies.join(", "),
            prereqs.join(", ")
        ));
    }
    out.push('\n');
    for n in &phases {
        let pfqn = format!("future/{project}/plan.phase-{n:02}");
        out.push_str(&format!("## Phase {n}\n\n"));
        let tasks: Vec<String> = records
            .iter()
            .filter_map(|e| match e {
                Record::Contains { from, to } if from == &pfqn => Some(to.clone()),
                _ => None,
            })
            .filter(|t| records.iter().any(|r| matches!(r, Record::Task { fqn, .. } if fqn == t)))
            .collect();
        for t in tasks {
            let (title, tier, status) = records
                .iter()
                .find_map(|r| match r {
                    Record::Task { fqn, title, tier, status } if fqn == &t => {
                        Some((title.clone(), tier.clone(), status.clone()))
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let check = if status == "done" { "[x]" } else { "[ ]" };
            out.push_str(&format!(
                "- {check} `{t}` — {title} (tier: {tier})\n"
            ));
        }
        out.push('\n');
    }
    match p.get("out").as_deref() {
        Some("-") => print!("{out}"),
        Some(path) => {
            std::fs::write(path, &out)?;
            println!("Rendered plan {project} to {path}");
        }
        None => {
            let out_path = apg_root.join(specs::TRANS).join("plans").join(format!("{project}.md"));
            std::fs::write(&out_path, &out)?;
            println!("Rendered plan {project} to {}", out_path.display());
        }
    }
    Ok(())
}

fn short_id(fqn: &str, project: &str) -> String {
    let prefix = format!("future/{project}/");
    fqn.strip_prefix(&prefix).unwrap_or(fqn).to_string()
}

/// Whether the spec project has a requirement with this fqn.
fn spec_has_requirement(apg_root: &Path, project: &str, req_fqn: &str) -> anyhow::Result<bool> {
    let path = specs::spec_jsonl_path(apg_root, project);
    if !path.exists() {
        return Ok(false);
    }
    Ok(specs::read_jsonl(&path)?.iter().any(|r| {
        matches!(r, Record::Requirement { fqn, .. } if fqn == req_fqn)
    }))
}

/// Whether the spec project has a future with this fqn.
fn spec_has_future(apg_root: &Path, project: &str, future_fqn: &str) -> anyhow::Result<bool> {
    let path = specs::spec_jsonl_path(apg_root, project);
    if !path.exists() {
        return Ok(false);
    }
    Ok(specs::read_jsonl(&path)?.iter().any(|r| {
        matches!(r, Record::Future { fqn, .. } if fqn == future_fqn)
    }))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_complete_implements_are_idempotent_and_preserve_spec_edges() {
        // Plan records: one done task anchored to two code nodes, phase
        // Satisfying R1.
        let records = vec![
            Record::PlanPhase {
                fqn: "future/foo/plan.phase-01".into(),
                number: 1,
                title: "P".into(),
                deliverable: "D".into(),
            },
            Record::Task {
                fqn: "future/foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                tier: "source".into(),
                status: "done".into(),
            },
            Record::Contains {
                from: "future/foo/plan.phase-01".into(),
                to: "future/foo/plan.phase-01.task-1".into(),
            },
            Record::Satisfies {
                from: "future/foo/plan.phase-01".into(),
                to: "future/foo/spec.R1".into(),
            },
            Record::Anchors {
                from: "future/foo/plan.phase-01.task-1".into(),
                to: "code/one".into(),
            },
            Record::Anchors {
                from: "future/foo/plan.phase-01.task-1".into(),
                to: "code/two".into(),
            },
        ];
        let tasks = vec!["future/foo/plan.phase-01.task-1".to_string()];
        let built = built_codes(&records, &tasks);
        assert_eq!(built, vec!["code/one", "code/two"]);
        let satisfied = satisfied_reqs(&records, "future/foo/plan.phase-01");
        assert_eq!(satisfied, vec!["future/foo/spec.R1"]);

        // Spec records carry the requirement's own edges — must survive.
        let mut spec_records = vec![
            Record::Requirement {
                fqn: "future/foo/spec.R1".into(),
                id: "R1".into(),
                title: "A".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::DependsOn {
                from: "future/foo/spec.R1".into(),
                to: "future/foo/spec.R2".into(),
            },
            Record::Anchors {
                from: "future/foo/spec.R1".into(),
                to: "code/one".into(),
            },
        ];
        for req in &satisfied {
            spec_records
                .retain(|r| !matches!(r, Record::Implements { to, .. } if to == req));
            for code in &built {
                spec_records.push(Record::Implements {
                    from: code.clone(),
                    to: req.clone(),
                });
            }
        }

        // Implements from BOTH built code nodes to R1.
        let impls: Vec<&str> = spec_records
            .iter()
            .filter_map(|r| match r {
                Record::Implements { from, to } if to == "future/foo/spec.R1" => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(impls, vec!["code/one", "code/two"]);

        // The requirement's DependsOn + Anchors survive in the spec records.
        assert!(spec_records.iter().any(|r| matches!(r, Record::DependsOn { from, to }
            if from == "future/foo/spec.R1" && to == "future/foo/spec.R2")));
        assert!(spec_records.iter().any(|r| matches!(r, Record::Anchors { from, to }
            if from == "future/foo/spec.R1" && to == "code/one")));
    }
}
