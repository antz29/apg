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
    artifacts::write_jsonl_and_reingest(apg_root, &specs::plan_jsonl_path(apg_root, project), project, records)
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
    artifacts::acquire_spec_lock(&apg_root)?;
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
    artifacts::acquire_spec_lock(&apg_root)?;
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
                anyhow::bail!("usage: apg plan add <project> task <phase> <k> --title … [--kind <source|test|gate|docs|human>] [--tier <unit|int|e2e>] [--builds <future-name>] [--anchor <fqn>]*");
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("task requires --title");
            };
            let fqn = format!("future/{project}/plan.phase-{phase:02}.task-{k}");
            let kind = p.get("kind").unwrap_or_else(|| "source".to_string());
            let tier = p.get("tier").unwrap_or_default();
            validate_task_kind_tier(&kind, &tier)?;
            let mut recs = vec![Record::Task {
                fqn: fqn.clone(),
                title,
                kind: kind.clone(),
                tier,
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

/// Validate the two-axis task classification: `kind` is the owning role
/// (orthogonal), `tier` the verification depth (a hierarchy, meaningful only
/// for `kind = test`). Mirrors the `Future.kind` / `Note.kind` validation
/// pattern in `spec_cmd`.
fn validate_task_kind_tier(kind: &str, tier: &str) -> anyhow::Result<()> {
    if !["source", "test", "gate", "docs", "human"].contains(&kind) {
        anyhow::bail!("invalid task kind `{kind}` — one of source/test/gate/docs/human");
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
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_plan(&apg_root, project)?;
    let phase_fqn = format!("future/{project}/plan.phase-{phase:02}");
    for req in p.all("satisfies") {
        let req_fqn = format!("future/{project}/spec.{req}");
        if !spec_has_requirement(&apg_root, project, &req_fqn)? {
            anyhow::bail!("satisfies target `{req}` is not a requirement of `{project}`");
        }
    }
    link_phase_edges(&phase_fqn, &p.all("satisfies"), &p.all("prereq"), &mut records)?;
    write_through(&apg_root, project, &records)?;
    println!("Linked plan.phase-{phase} of {project}");
    Ok(())
}

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
        .strip_prefix("future/")
        .and_then(|s| s.split('/').next())
        .ok_or_else(|| anyhow::anyhow!("bad phase fqn `{phase_fqn}`"))?;
    records.retain(|r| !matches!(r, Record::Satisfies { from, .. } if from.as_str() == phase_fqn));
    for req in reqs {
        let req_fqn = format!("future/{project}/spec.{req}");
        records.push(Record::Satisfies {
            from: phase_fqn.to_string(),
            to: req_fqn,
        });
    }
    records.retain(|r| !matches!(r, Record::Gates { from, .. } if from.as_str() == phase_fqn));
    for g in prereqs {
        let g = g.parse::<u32>().map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
        let target = format!("future/{project}/plan.phase-{g:02}");
        let phase_n = phase_fqn
            .rsplit("phase-")
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if let Some(path) = artifacts::cycle_closing_path(records, phase_fqn, &target, |r| match r {
            Record::Gates { from, to } => Some((from.as_str(), to.as_str())),
            _ => None,
        }) {
            let short: Vec<String> = path
                .iter()
                .map(|f| {
                    f.strip_prefix(&format!("future/{project}/plan.phase-"))
                        .unwrap_or(f)
                        .to_string()
                })
                .collect();
            anyhow::bail!(
                "adding gate phase-{phase_n:02} → phase-{g:02} would create a cycle: {}",
                short.join(" → ")
            );
        }
        records.push(Record::Gates {
            from: phase_fqn.to_string(),
            to: target,
        });
    }
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
    artifacts::acquire_spec_lock(&apg_root)?;
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
    artifacts::acquire_spec_lock(&apg_root)?;
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
    artifacts::acquire_spec_lock(&apg_root)?;
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

    // Human-teeth (R…): a `human`-kind task is owned by the person — an agent
    // must not close the phase (or retire the final-phase plan) around an
    // unperformed human step. Refuse with a targeted message.
    let human_not_done = human_tasks_not_done(&records, &phase_fqn);
    if !human_not_done.is_empty() {
        anyhow::bail!(
            "phase {phase} has human tasks not done: {} — a human step cannot be closed by an agent; complete them first",
            human_not_done.join(", ")
        );
    }

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
        if apg_root.join(".trans").join("db.lbug").exists() {
            reingest_project(&apg_root, project)?;
        }
        println!("Completed final phase {phase} — plan {project} retired (JSONL dropped)");
    } else {
        println!("Completed phase {phase} of plan {project}");
    }
    Ok(())
}

/// The `human`-kind tasks directly contained in a phase that are not `done` —
/// the teeth that keep an agent from closing a phase around an unperformed
/// human step.
fn human_tasks_not_done(records: &[Record], phase_fqn: &str) -> Vec<String> {
    records
        .iter()
        .filter_map(|e| match e {
            Record::Contains { from, to } if from == phase_fqn => Some(to.clone()),
            _ => None,
        })
        .filter_map(|t| {
            records.iter().find_map(|r| match r {
                Record::Task {
                    fqn,
                    kind,
                    status,
                    ..
                } if fqn == &t => Some((t.clone(), kind, status)),
                _ => None,
            })
        })
        .filter(|(_, kind, status)| kind.as_str() == "human" && status.as_str() != "done")
        .map(|(t, _, _)| t)
        .collect()
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
            let out_path = apg_root.join(specs::TRANS).join("plans").join(format!("{project}.md"));
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
        .filter(|t| records.iter().any(|r| matches!(r, Record::Task { fqn, .. } if fqn == t)))
        .collect();
    const KIND_ORDER: [&str; 5] = ["source", "test", "gate", "docs", "human"];
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
    fn link_phase_edges_keeps_all_satisfies_and_preserves_other_phases() {
        // Regression: the removal ran inside the loop (only the last edge
        // survived) and removed edges incident to the phase (incoming Gates
        // from a later phase were severed).
        let mut records = vec![
            Record::PlanPhase {
                fqn: "future/foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D".into(),
            },
            Record::PlanPhase {
                fqn: "future/foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D".into(),
            },
            // A later phase gating this one: incoming edge, must survive.
            Record::Gates {
                from: "future/foo/plan.phase-02".into(),
                to: "future/foo/plan.phase-01".into(),
            },
        ];
        link_phase_edges(
            "future/foo/plan.phase-01",
            &["R1".into(), "R2".into(), "R3".into()],
            &["3".into()],
            &mut records,
        )
        .unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to }
                    if from == "future/foo/plan.phase-01" =>
                {
                    Some(to.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            satisfies,
            vec![
                "future/foo/spec.R1",
                "future/foo/spec.R2",
                "future/foo/spec.R3"
            ]
        );
        // Outgoing Gates set; the incoming phase-02 → phase-01 gate survives.
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "future/foo/plan.phase-01" && to == "future/foo/plan.phase-03"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "future/foo/plan.phase-02" && to == "future/foo/plan.phase-01"
        )));
        // Linking phase-01 to gate phase-02 would close the incoming
        // phase-02 → phase-01 gate into a cycle — rejected.
        assert!(link_phase_edges(
            "future/foo/plan.phase-01",
            &[],
            &["2".into()],
            &mut records,
        )
        .is_err());
        // Re-linking replaces, never duplicates.
        link_phase_edges("future/foo/plan.phase-01", &["R9".into()], &[], &mut records).unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to }
                    if from == "future/foo/plan.phase-01" =>
                {
                    Some(to.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(satisfies, vec!["future/foo/spec.R9"]);
    }

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
                kind: "source".into(),
                tier: String::new(),
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

    #[test]
    fn task_kind_tier_validation() {
        // Default kind is source.
        assert!(validate_task_kind_tier("source", "").is_ok());
        // All five kinds accepted.
        for k in ["source", "test", "gate", "docs", "human"] {
            let tier = if k == "test" { "unit" } else { "" };
            assert!(validate_task_kind_tier(k, tier).is_ok(), "kind {k}");
        }
        // Unknown kind rejected.
        assert!(validate_task_kind_tier("qa", "").is_err());
        // tier required for test, rejected for non-test.
        assert!(validate_task_kind_tier("test", "").is_err());
        assert!(validate_task_kind_tier("source", "unit").is_err());
        assert!(validate_task_kind_tier("human", "e2e").is_err());
        // Unknown tier rejected.
        assert!(validate_task_kind_tier("test", "smoke").is_err());
        // All three tiers accepted for test.
        for t in ["unit", "int", "e2e"] {
            assert!(validate_task_kind_tier("test", t).is_ok(), "tier {t}");
        }
    }

    #[test]
    fn plan_complete_refuses_undone_human_task() {
        let records = vec![
            Record::Task {
                fqn: "future/foo/plan.phase-01.task-1".into(),
                title: "sign off".into(),
                kind: "human".into(),
                tier: String::new(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "future/foo/plan.phase-01".into(),
                to: "future/foo/plan.phase-01.task-1".into(),
            },
        ];
        let undone = human_tasks_not_done(&records, "future/foo/plan.phase-01");
        assert_eq!(undone, vec!["future/foo/plan.phase-01.task-1"]);
        // A done human task passes the gate.
        let mut records = records;
        for r in &mut records {
            if let Record::Task { status, .. } = r {
                *status = "done".to_string();
            }
        }
        assert!(human_tasks_not_done(&records, "future/foo/plan.phase-01").is_empty());
    }

    #[test]
    fn render_groups_tasks_by_kind_and_shows_test_tier() {
        let records = vec![
            Record::PlanPhase {
                fqn: "future/foo/plan.phase-01".into(),
                number: 1,
                title: "P".into(),
                deliverable: "D".into(),
            },
            Record::Task {
                fqn: "future/foo/plan.phase-01.task-1".into(),
                title: "Implement".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
            },
            Record::Task {
                fqn: "future/foo/plan.phase-01.task-2".into(),
                title: "Unit tests".into(),
                kind: "test".into(),
                tier: "unit".into(),
                status: "pending".into(),
            },
            Record::Task {
                fqn: "future/foo/plan.phase-01.task-3".into(),
                title: "Doc".into(),
                kind: "docs".into(),
                tier: String::new(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "future/foo/plan.phase-01".into(),
                to: "future/foo/plan.phase-01.task-1".into(),
            },
            Record::Contains {
                from: "future/foo/plan.phase-01".into(),
                to: "future/foo/plan.phase-01.task-2".into(),
            },
            Record::Contains {
                from: "future/foo/plan.phase-01".into(),
                to: "future/foo/plan.phase-01.task-3".into(),
            },
        ];
        let out = render_phase_tasks(&records, "future/foo/plan.phase-01");
        // Grouped by kind in canonical order (source before test before docs).
        let source_pos = out.find("**source**").unwrap();
        let test_pos = out.find("**test**").unwrap();
        let docs_pos = out.find("**docs**").unwrap();
        assert!(source_pos < test_pos && test_pos < docs_pos, "order: {out}");
        // Test depth renders as test/unit.
        assert!(out.contains("- [ ] `future/foo/plan.phase-01.task-2` — Unit tests (test/unit)"), "{out}");
        // Done checkbox preserved.
        assert!(out.contains("- [x] `future/foo/plan.phase-01.task-1` — Implement (source)"), "{out}");
    }
}
