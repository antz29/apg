//! `apg plan` — the phased execution plan that bridges the spec to present
//! code (PHASE_03 execution + apply model). The plan is transient by design
//! (R22): it lives in the gitignored `apg/.trans/plans/<project>.jsonl` and is
//! branch-local. Nothing is promoted by `plan done`/`plan complete` — those are
//! assertion + milestone only. The plan survives until the apply act, whose
//! coherence gate (Builds targets resolve, all feedback resolved) precedes the
//! merge + rebuild of `main`'s graph (PlanCompletion-SPEC.md).

use std::path::{Path, PathBuf};

use crate::artifacts::{self, parse_args, remove_node};
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
        anyhow::bail!("no plan for project `{project}` — run `apg plan init {project}` first");
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
        anyhow::bail!("usage: apg plan <init|add|link|done|undone|note|complete|render|apply> …");
    };
    match sub {
        "init" => plan_init(&args[1..]),
        "add" => plan_add(&args[1..]),
        "link" => plan_link(&args[1..]),
        "done" => plan_done(&args[1..]),
        "undone" => plan_undone(&args[1..]),
        "note" => plan_note(&args[1..]),
        "complete" => plan_complete(&args[1..]),
        "render" => plan_render(&args[1..]),
        "apply" => plan_apply(&args[1..]),
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
        anyhow::bail!("no spec for `{project}` — author a spec first (`apg spec init {project}`)");
    }
    let path = specs::plan_jsonl_path(&apg_root, project);
    if path.exists() {
        anyhow::bail!("plan for `{project}` already exists at {}", path.display());
    }
    let records = vec![Record::Plan {
        fqn: format!("{project}/plan"),
        title: p
            .get("title")
            .unwrap_or_else(|| format!("Plan for {project}")),
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
    let plan_fqn = format!("{project}/plan");
    match kind {
        "phase" => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!(
                    "usage: apg plan add <project> phase <n> --title … [--deliverable …] [--prereq <n>]* [--satisfies <req-id>]*"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("phase requires --title");
            };
            let fqn = format!("{project}/plan.phase-{n:02}");
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
                let g = g
                    .parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
                recs.push(Record::Gates {
                    from: fqn.clone(),
                    to: format!("{project}/plan.phase-{g:02}"),
                });
            }
            for req in p.all("satisfies") {
                let req_fqn = format!("{project}/spec.{req}");
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
                anyhow::bail!(
                    "usage: apg plan add <project> task <phase> <k> --title … [--kind <source|test|gate|docs>] [--tier <unit|int|e2e>] [--builds <future-name>] [--anchor <fqn>]*"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("task requires --title");
            };
            let fqn = format!("{project}/plan.phase-{phase:02}.task-{k}");
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
                from: format!("{project}/plan.phase-{phase:02}"),
                to: fqn.clone(),
            });
            if let Some(future_name) = p.get("builds") {
                let future_fqn = format!("{project}/{future_name}");
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
                    recs.push(Record::Anchors {
                        from: fqn.clone(),
                        to: a,
                    });
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

/// `apg plan link <project> <phase-n> [--satisfies <req-id>]* [--prereq <n>]*`
/// — add/refresh the phase's bridge edges.
fn plan_link(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(phase)) = (
        p.positional.first(),
        p.positional.get(1).and_then(|s| s.parse::<u32>().ok()),
    ) else {
        anyhow::bail!(
            "usage: apg plan link <project> <phase-n> [--satisfies <req-id>]* [--prereq <n>]*"
        );
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_plan(&apg_root, project)?;
    let phase_fqn = format!("{project}/plan.phase-{phase:02}");
    for req in p.all("satisfies") {
        let req_fqn = format!("{project}/spec.{req}");
        if !spec_has_requirement(&apg_root, project, &req_fqn)? {
            anyhow::bail!("satisfies target `{req}` is not a requirement of `{project}`");
        }
    }
    link_phase_edges(
        &phase_fqn,
        &p.all("satisfies"),
        &p.all("prereq"),
        &mut records,
    )?;
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
        .split('/')
        .next()
        .ok_or_else(|| anyhow::anyhow!("bad phase fqn `{phase_fqn}`"))?;
    records.retain(|r| !matches!(r, Record::Satisfies { from, .. } if from.as_str() == phase_fqn));
    for req in reqs {
        let req_fqn = format!("{project}/spec.{req}");
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
        let phase_n = phase_fqn
            .rsplit("phase-")
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if let Some(path) =
            artifacts::cycle_closing_path(records, phase_fqn, &target, |r| match r {
                Record::Gates { from, to } => Some((from.as_str(), to.as_str())),
                _ => None,
            })
        {
            let short: Vec<String> = path
                .iter()
                .map(|f| {
                    f.strip_prefix(&format!("{project}/plan.phase-"))
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

/// `apg plan done <project> <task-fqn>` (PHASE_03) — the implementer's
/// assertion. Marks a task done with NO promotion and NO code-graph
/// verification: the plan's `Builds` futures stay declared until the apply act
/// (PlanCompletion-SPEC.md), whose coherence gate verifies every `Builds`
/// target resolves in the merged graph before applying. `apg plan undone`
/// remains the reversal.
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
/// recreate retired Futures (the code is already in the present).
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
            Record::Note { fqn, .. } if fqn.starts_with(&format!("{project}/plan.note-")) => {
                fqn.rsplit_once("-")
                    .and_then(|(_, s)| s.parse::<u64>().ok())
            }
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
/// unresolved feedback) is enforced; NO `Implements` materialization and NO
/// plan retirement.
fn plan_complete_at(apg_root: &Path, project: &str, phase: u32) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let records = load_plan(apg_root, project)?;
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
    // the apply act.
    println!("Completed phase {phase} of plan {project} (milestone — plan survives until apply)");
    Ok(())
}

/// `apg plan apply <project>` (PHASE_03) — the single delivery moment
/// (PlanCompletion-SPEC.md). Runs the change-set coherence gate against the
/// branch's graph:
///
/// - every `Builds` future's target resolves in the code graph (a claimed-done
///   task whose code is missing blocks apply);
/// - every phase and the whole-plan review are green (all `Feedback` resolved);
/// - the human gate has passed (the navigator's summary; outside the CLI).
///
/// The gate is all this command checks — it performs NO merge and NO graph
/// mutation. On green it prints the merge + rebuild handoff; the navigator
/// operates `git merge <project-branch>` into `main` and rebuilds `main`'s
/// graph with a fresh scan on human approval (push/tag remain human).
fn plan_apply(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan apply <project>");
    };
    let apg_root = require_apg_root()?;
    plan_apply_at(&apg_root, project)
}

/// Core of `plan_apply` — the coherence gate. Returns the apply handoff message
/// on green, or errors listing every blocker (unresolved Builds target,
/// unresolved feedback).
fn plan_apply_at(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let records = load_plan(apg_root, project)?;
    // Futures are declared in the spec JSONL (`apg spec add future`), while the
    // plan's Builds edges reference them by fqn — the gate needs the spec
    // records too to resolve each Builds future's target.
    let spec_records = {
        let p = specs::spec_jsonl_path(apg_root, project);
        if p.exists() {
            specs::read_jsonl(&p)?
        } else {
            Vec::new()
        }
    };
    let all = records.iter().chain(spec_records.iter()).collect::<Vec<_>>();
    let db = artifacts::ArtifactDb::open(apg_root)?;

    // 1. Every Builds future's target resolves in the branch's graph.
    let mut blocked: Vec<String> = Vec::new();
    let builds: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Builds { to, .. } => Some(to.clone()),
            _ => None,
        })
        .collect();
    for future_fqn in &builds {
        let target = all.iter().find_map(|r| match r {
            Record::Future { fqn, target, .. } if fqn == future_fqn => Some(target.clone()),
            _ => None,
        });
        let target = target.filter(|t| !t.is_empty());
        match target {
            Some(t) if db.has_node(&t) => {}
            _ => {
                blocked.push(format!(
                    "`{future_fqn}` is claimed-built by the plan but its target does not resolve in the branch's graph (missing code blocks apply)"
                ));
            }
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

    if !blocked.is_empty() {
        anyhow::bail!(
            "apply coherence gate blocked: {}",
            blocked.join("; ")
        );
    }
    if !unresolved.is_empty() {
        anyhow::bail!(
            "apply coherence gate blocked: unresolved review feedback: {} — resolve every `Feedback` before apply",
            unresolved.join(", ")
        );
    }
    println!("Apply gate passed for {project}: every Builds target resolves, all feedback resolved.");
    println!(
        "Merge: git merge {project} into main, then rebuild main's graph with `apg scan` (push/tag remain human)."
    );
    Ok(())
}

/// The built code nodes of a phase: the unique set of its tasks' `Anchors`
/// (files/functions the tasks touched). Retained for the apply-coherence view
/// and the unit test; the milestone-only `plan complete` does not materialize
/// them (PHASE_03).
#[cfg(test)]
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

/// The requirements a plan phase `Satisfies` (unit-test helper).
#[cfg(test)]
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
    out.push_str("## Phases\n\n| Phase | Deliverable | Satisfies | Prereq |\n|---|---|---|---|\n");
    for n in &phases {
        let pfqn = format!("{project}/plan.phase-{n:02}");
        let deliverable = records
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase {
                    fqn, deliverable, ..
                } if fqn == &pfqn => Some(deliverable.clone()),
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
        let pfqn = format!("{project}/plan.phase-{n:02}");
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

/// Whether the spec project has a requirement with this fqn.
fn spec_has_requirement(apg_root: &Path, project: &str, req_fqn: &str) -> anyhow::Result<bool> {
    let path = specs::spec_jsonl_path(apg_root, project);
    if !path.exists() {
        return Ok(false);
    }
    Ok(specs::read_jsonl(&path)?
        .iter()
        .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == req_fqn)))
}

/// Whether the spec project has a future with this fqn.
fn spec_has_future(apg_root: &Path, project: &str, future_fqn: &str) -> anyhow::Result<bool> {
    let path = specs::spec_jsonl_path(apg_root, project);
    if !path.exists() {
        return Ok(false);
    }
    Ok(specs::read_jsonl(&path)?
        .iter()
        .any(|r| matches!(r, Record::Future { fqn, .. } if fqn == future_fqn)))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::load;
    use lbug::{Connection, Database};

    /// A temp `apg/` layout with a real DB carrying a code graph (Module, File,
    /// Struct) plus committed spec JSONL for project `foo`.
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("apg-plan-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        std::fs::create_dir_all(dir.join("apg").join("specs")).unwrap();

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
        (dir.join("apg"), dir)
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
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();
        path
    }

    #[test]
    fn plan_done_is_assertion_only_and_undone_reverses() {
        let (apg_root, dir) = fixture("assertion-done");
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

        // Assertion-only: no Futures were retired, no Implements appeared — the
        // plan carries no promotion side effects.
        assert!(
            recs.iter()
                .all(|r| !matches!(r, Record::Implements { .. })),
            "assertion-only done must not materialize Implements"
        );

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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_complete_is_milestone_only_with_gate_and_no_retirement() {
        let (apg_root, dir) = fixture("milestone-complete");

        // Pending task → complete is rejected by the gate.
        let _path = write_plan(&apg_root);
        assert!(plan_complete_at(&apg_root, "foo", 1).is_err());

        // Mark the task done, add resolved feedback to prove the gate accepts.
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();

        // Milestone-only: the plan file survives (no retirement), no Implements
        // materialized in the spec JSONL.
        assert!(specs::plan_jsonl_path(&apg_root, "foo").exists());
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(r, Record::PlanPhase { .. })));
        let spec_path = specs::spec_jsonl_path(&apg_root, "foo");
        let spec_recs = if spec_path.exists() {
            specs::read_jsonl(&spec_path).unwrap()
        } else {
            vec![]
        };
        assert!(
            spec_recs
                .iter()
                .all(|r| !matches!(r, Record::Implements { .. })),
            "milestone-only complete must not materialize Implements"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_note_roundtrip_into_plan_jsonl() {
        let (apg_root, dir) = fixture("task-note");
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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_rejects_unresolved_builds_target() {
        let (apg_root, dir) = fixture("apply-gate");

        // A plan whose task Builds a Future whose target is NOT in the code
        // graph (the code was claimed-done but is missing).
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "foo/gateway".to_string(),
            },
            Record::Future {
                fqn: "foo/gateway".to_string(),
                kind: "function".to_string(),
                target: "github.com/x/y.Gateway".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // The gate rejects: the Builds target does not resolve in the graph.
        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("coherence gate blocked"), "{err}");
        assert!(err.to_string().contains("foo/gateway"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_passes_when_builds_target_resolves_and_feedback_resolved() {
        let (apg_root, dir) = fixture("apply-gate-ok");

        // The Builds future's target resolves to the fixture Struct.
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "foo/store".to_string(),
            },
            Record::Future {
                fqn: "foo/store".to_string(),
                kind: "struct".to_string(),
                target: "github.com/x/y.Store".to_string(),
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

        // Green: every Builds target resolves, all feedback resolved.
        assert!(plan_apply_at(&apg_root, "foo").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_resolves_builds_future_declared_in_spec_jsonl() {
        let (apg_root, dir) = fixture("apply-gate-spec-future");

        // Real-world shape: `apg spec add future` declares the Future in the
        // SPEC JSONL, and the plan carries only the Builds edge referencing it.
        // The gate must merge spec records to resolve the target (regression:
        // the dogfood apply once failed because it searched only the plan).
        let plan = specs::plan_jsonl_path(&apg_root, "foo");
        let plan_records = vec![
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "foo/store".to_string(),
            },
        ];
        specs::write_jsonl(&plan, &plan_records).unwrap();

        let spec = specs::spec_jsonl_path(&apg_root, "foo");
        let spec_records = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "S".to_string(),
                goal: String::new(),
            },
            Record::Future {
                fqn: "foo/store".to_string(),
                kind: "struct".to_string(),
                target: "github.com/x/y.Store".to_string(),
            },
        ];
        specs::write_jsonl(&spec, &spec_records).unwrap();

        // Green: the spec-declared Future's target resolves in the graph.
        assert!(plan_apply_at(&apg_root, "foo").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_rejects_unresolved_spec_declared_builds_target() {
        let (apg_root, dir) = fixture("apply-gate-spec-future-bad");

        // Same shape, but the spec-declared Future targets code that does not
        // resolve — the gate must still reject it.
        let plan = specs::plan_jsonl_path(&apg_root, "foo");
        let plan_records = vec![
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "foo/store".to_string(),
            },
        ];
        specs::write_jsonl(&plan, &plan_records).unwrap();

        let spec = specs::spec_jsonl_path(&apg_root, "foo");
        let spec_records = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "S".to_string(),
                goal: String::new(),
            },
            Record::Future {
                fqn: "foo/store".to_string(),
                kind: "struct".to_string(),
                target: "github.com/x/y.Missing".to_string(),
            },
        ];
        specs::write_jsonl(&spec, &spec_records).unwrap();

        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("coherence gate blocked"), "{err}");
        assert!(err.to_string().contains("foo/store"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_rejects_unresolved_feedback() {
        let (apg_root, dir) = fixture("apply-feedback");

        // Builds target resolves, but an open Feedback reviews the phase.
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "done".to_string(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "foo/store".to_string(),
            },
            Record::Future {
                fqn: "foo/store".to_string(),
                kind: "struct".to_string(),
                target: "github.com/x/y.Store".to_string(),
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

        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("unresolved review feedback"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
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
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D".into(),
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
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => {
                    Some(to.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            satisfies,
            vec![
                "foo/spec.R1",
                "foo/spec.R2",
                "foo/spec.R3"
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
        assert!(
            link_phase_edges("foo/plan.phase-01", &[], &["2".into()], &mut records,)
                .is_err()
        );
        // Re-linking replaces, never duplicates.
        link_phase_edges(
            "foo/plan.phase-01",
            &["R9".into()],
            &[],
            &mut records,
        )
        .unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => {
                    Some(to.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(satisfies, vec!["foo/spec.R9"]);
    }

    #[test]
    fn plan_complete_implements_are_idempotent_and_preserve_spec_edges() {
        // Plan records: one done task anchored to two code nodes, phase
        // Satisfying R1.
        let records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P".into(),
                deliverable: "D".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".into(),
                to: "foo/spec.R1".into(),
            },
            Record::Anchors {
                from: "foo/plan.phase-01.task-1".into(),
                to: "code/one".into(),
            },
            Record::Anchors {
                from: "foo/plan.phase-01.task-1".into(),
                to: "code/two".into(),
            },
        ];
        let tasks = vec!["foo/plan.phase-01.task-1".to_string()];
        let built = built_codes(&records, &tasks);
        assert_eq!(built, vec!["code/one", "code/two"]);
        let satisfied = satisfied_reqs(&records, "foo/plan.phase-01");
        assert_eq!(satisfied, vec!["foo/spec.R1"]);

        // Spec records carry the requirement's own edges — must survive.
        let mut spec_records = vec![
            Record::Requirement {
                fqn: "foo/spec.R1".into(),
                id: "R1".into(),
                title: "A".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::DependsOn {
                from: "foo/spec.R1".into(),
                to: "foo/spec.R2".into(),
            },
            Record::Anchors {
                from: "foo/spec.R1".into(),
                to: "code/one".into(),
            },
        ];
        for req in &satisfied {
            spec_records.retain(|r| !matches!(r, Record::Implements { to, .. } if to == req));
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
                Record::Implements { from, to } if to == "foo/spec.R1" => {
                    Some(from.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(impls, vec!["code/one", "code/two"]);

        // The requirement's DependsOn + Anchors survive in the spec records.
        assert!(
            spec_records
                .iter()
                .any(|r| matches!(r, Record::DependsOn { from, to }
            if from == "foo/spec.R1" && to == "foo/spec.R2"))
        );
        assert!(
            spec_records
                .iter()
                .any(|r| matches!(r, Record::Anchors { from, to }
            if from == "foo/spec.R1" && to == "code/one"))
        );
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
    fn render_groups_tasks_by_kind_and_shows_test_tier() {
        let records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P".into(),
                deliverable: "D".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "Implement".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-2".into(),
                title: "Unit tests".into(),
                kind: "test".into(),
                tier: "unit".into(),
                status: "pending".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-3".into(),
                title: "Doc".into(),
                kind: "docs".into(),
                tier: String::new(),
                status: "pending".into(),
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
}
