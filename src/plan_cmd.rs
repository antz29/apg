//! `apg plan` — the phased execution plan that bridges the spec to present
//! code (PHASE_03 execution + apply model). The plan is transient by design
//! (R22): it lives in the gitignored `apg/.trans/plans/<project>.jsonl` and is
//! branch-local. The plan-writer authors the tier-4 additions as **planned
//! Implementation nodes** (`apg plan add <project> planned <kind> <fqn>` —
//! Module/File/Struct/Function marked `status: planned` at the FQN where the
//! code will land, GraphModel-SPEC.md); a task `Builds` the planned node it
//! creates. Nothing is promoted by `plan done`/`plan complete` — those are
//! assertion + milestone only; a branch scan **replaces realized planned
//! nodes**. The plan survives until the apply act, whose coherence gate (every
//! planned node realized, all feedback resolved) precedes the merge + rebuild
//! of `main`'s graph (PlanCompletion-SPEC.md).

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

/// `apg plan add <project> phase <n> …` / `task <phase> <k> …` /
/// `planned <kind> <fqn> …` (R23, PHASE_02).
fn plan_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan add <project> <phase|task|planned> …");
    };
    let Some(kind) = p.positional.get(1).map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg plan add <project> <phase|task|planned> …");
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_plan(&apg_root, project)?;
    let plan_fqn = format!("{project}/plan");
    match kind {
        "planned" => {
            let (Some(node_kind), Some(fqn)) = (
                p.positional.get(2).map(|s| s.as_str()),
                p.positional.get(3),
            ) else {
                anyhow::bail!(
                    "usage: apg plan add <project> planned <kind> <fqn> [--name <name>] [--parent <parent-fqn>]"
                );
            };
            if !["module", "file", "struct", "function"].contains(&node_kind) {
                anyhow::bail!(
                    "planned node kind must be module/file/struct/function, got `{node_kind}`"
                );
            }
            // A plan never plans code that already exists: a present node at
            // the FQN would make the planned placeholder incoherent (the
            // scanner-replace only ever supersedes, never the reverse).
            if let Ok(db) = artifacts::ArtifactDb::open(&apg_root)
                && let Some(label) = db.code_label(fqn)
            {
                anyhow::bail!(
                    "planned node `{fqn}` already resolves to a `{label}` code node — a plan never plans existing code (plan the delta, not the present)"
                );
            }
            let rec = Record::PlannedNode {
                fqn: fqn.clone(),
                kind: node_kind.to_string(),
                name: p.get("name").unwrap_or_default(),
                parent: p.get("parent").unwrap_or_default(),
            };
            remove_node(&mut records, fqn);
            records.push(rec);
            if let Some(parent) = p.get("parent") {
                records.push(Record::Contains {
                    from: parent.clone(),
                    to: fqn.clone(),
                });
            }
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
                    "usage: apg plan add <project> task <phase> <k> --title … [--kind <source|test|gate|docs>] [--tier <unit|int|e2e>] [--builds <planned-fqn>] [--anchor <fqn>]*"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("task requires --title");
            };
            let kind = p.get("kind").unwrap_or_else(|| "source".to_string());
            let tier = p.get("tier").unwrap_or_default();
            let anchors: Vec<String> = p.all("anchor");
            plan_add_task_at(
                &apg_root,
                project,
                &mut records,
                phase,
                k,
                &title,
                &kind,
                &tier,
                p.get("builds").as_deref(),
                &anchors,
            )?;
            write_through(&apg_root, project, &records)?;
            println!("Added task {k} to plan.phase-{phase} of {project}");
        }
        other => anyhow::bail!("unknown plan add kind `{other}` — phase|task"),

    }
    Ok(())
}

/// Core of the `phase` add arm (extracted for tests): appends the PlanPhase +
/// Contains + Satisfies records and every prereq `Gates` edge — each gated
/// through the same cycle check `plan link` uses (a self-gate or transitive
/// Gates cycle is rejected before any write). Re-add is an upsert: the
/// phase's old incident edges are dropped BEFORE the cycle check, so a
/// retired edge can't resurrect as a false cycle (mirrors `apg spec add
/// phase`).
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
    remove_node(records, &fqn);
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
        let req_fqn = format!("{project}/spec.{req}");
        if !spec_has_requirement(apg_root, project, &req_fqn)? {
            anyhow::bail!("satisfies target `{req}` is not a requirement of `{project}`");
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
/// before any write), validates kind/tier, and appends the Task + Contains +
/// optional Builds/Anchors records.
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
    builds: Option<&str>,
    anchors: &[String],
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
    validate_task_kind_tier(kind, tier)?;
    let mut recs = vec![Record::Task {
        fqn: fqn.clone(),
        title: title.to_string(),
        kind: kind.to_string(),
        tier: tier.to_string(),
        status: "pending".to_string(),
    }];
    recs.push(Record::Contains {
        from: phase_fqn,
        to: fqn.clone(),
    });
    if let Some(builds_fqn) = builds {
        if !plan_has_planned_node(records, builds_fqn) {
            anyhow::bail!(
                "builds target `{builds_fqn}` is not a planned Implementation node of `{project}` (author it first with `apg plan add {project} planned <kind> <fqn>`)"
            );
        }
        recs.push(Record::Builds {
            from: fqn.clone(),
            to: builds_fqn.to_string(),
        });
    }
    {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        for a in anchors {
            if db.code_label(a).is_none() {
                anyhow::bail!("task anchor `{a}` is not a resolved code node");
            }
            recs.push(Record::Anchors {
                from: fqn.clone(),
                to: a.clone(),
            });
        }
    }
    remove_node(records, &fqn);
    records.extend(recs);
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
    plan_link_at(
        &apg_root,
        project,
        phase,
        &p.all("satisfies"),
        &p.all("prereq"),
    )?;
    println!("Linked plan.phase-{phase} of {project}");
    Ok(())
}

/// Core of `plan_link` (extracted for tests): verifies the target phase
/// exists (a link to a nonexistent `plan.phase-NN` is rejected before any
/// write — CLI-envelope, PlanCreation-SPEC "structure is valid"), validates
/// Satisfies targets, then sets the phase's bridge edges.
fn plan_link_at(
    apg_root: &Path,
    project: &str,
    phase: u32,
    satisfies: &[String],
    prereqs: &[String],
) -> anyhow::Result<()> {
    let mut records = load_plan(apg_root, project)?;
    let phase_fqn = format!("{project}/plan.phase-{phase:02}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::PlanPhase { fqn: pf, .. } if pf == &phase_fqn))
    {
        anyhow::bail!(
            "link target `{phase_fqn}` is not a phase of `{project}` (author the phase first with `apg plan add {project} phase {phase} …`)"
        );
    }
    for req in satisfies {
        let req_fqn = format!("{project}/spec.{req}");
        if !spec_has_requirement(apg_root, project, &req_fqn)? {
            anyhow::bail!("satisfies target `{req}` is not a requirement of `{project}`");
        }
    }
    link_phase_edges(&phase_fqn, satisfies, prereqs, &mut records)?;
    write_through(apg_root, project, &records)?;
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
/// `cycle_closing_path` machinery `apg spec add phase` / `apg plan link` use)
/// before the edge is ever accumulated into the records — so the JSONL/DB
/// write-through never runs on a cycle. `records` must already have the
/// phase's stale incident edges removed (the add/link callers do this).
fn push_gate(
    from: &str,
    to: &str,
    records: &[Record],
) -> anyhow::Result<()> {
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
/// verification: the plan's `Builds` planned nodes stay declared until the
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

/// `apg plan apply <project>` (PHASE_03) — the single delivery moment
/// (PlanCompletion-SPEC.md). Runs the change-set coherence gate against the
/// branch's graph:
///
/// - every planned Implementation node is realized in the code graph (a
///   planned node with no real code at its FQN blocks apply);
/// - every phase and the whole-plan review are green (all `Feedback` resolved);
/// - the human gate has passed (the navigator's summary; outside the CLI).
///
/// The gate is all this command checks — it performs NO merge and NO graph
/// mutation. On green it prints the merge + rebuild handoff; the navigator
/// operates `git merge <project-branch>` into `main` and rebuilds `main`'s
/// graph with a fresh scan on human approval (push/tag remain human).
///
/// Invariants are deliberately NOT evaluated here (wont-fix, REVIEW.md): an
/// invariant's body is free prose, so a mechanical pass could not check it,
/// and Invariants-SPEC's "Correctness never depends on them" makes a
/// gate-blocking invariant incoherent with the emergent model — the navigator
/// verifies the GuardedBy set (`apg invariants`) as part of the human gate.
fn plan_apply(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan apply <project>");
    };
    let apg_root = require_apg_root()?;
    plan_apply_at(&apg_root, project)
}

/// Core of `plan_apply` — the coherence gate. Returns the apply handoff message
/// on green, or errors listing every blocker (unrealized planned nodes,
/// unresolved feedback).
fn plan_apply_at(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;
    let records = load_plan(apg_root, project)?;
    let db = artifacts::ArtifactDb::open(apg_root)?;

    // 1. Every planned Implementation node in the branch is realized: a scan
    // found real (present) code at its FQN and replaced the placeholder. A
    // planned node still marked `planned`, or with no node at all, blocks
    // apply (PlanCompletion-SPEC.md — the planned-node realization gate).
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
                "planned {kind} node `{fqn}` is not realized — a scan must find real code at its FQN before apply (missing code blocks apply)"
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
    println!(
        "Apply gate passed for {project}: every planned node is realized, all feedback resolved."
    );
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

/// Whether the plan has a planned Implementation node with this fqn.
fn plan_has_planned_node(records: &[Record], fqn: &str) -> bool {
    records
        .iter()
        .any(|r| matches!(r, Record::PlannedNode { fqn: f, .. } if f == fqn))
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

        // Assertion-only: no planned nodes were retired, no Implements appeared — the
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
    fn apply_gate_rejects_unrealized_planned_node() {
        let (apg_root, dir) = fixture("apply-gate");

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
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Gateway".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        // The gate rejects: the planned node does not resolve to real code.
        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("coherence gate blocked"), "{err}");
        assert!(err.to_string().contains("github.com/x/y.Gateway"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_passes_when_planned_node_realized_and_feedback_resolved() {
        let (apg_root, dir) = fixture("apply-gate-ok");

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
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Store".to_string(),
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
        assert!(plan_apply_at(&apg_root, "foo").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_checks_every_planned_node_not_just_builds_targets() {
        let (apg_root, dir) = fixture("apply-gate-all-planned");

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
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("github.com/x/y.Gateway"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_rejects_unresolved_feedback() {
        let (apg_root, dir) = fixture("apply-feedback");

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
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Store".to_string(),
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
                status: "pending".into(),
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
                status: "pending".into(),
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
        assert!(format!("{err:#}").contains("would create a cycle"), "got: {err:#}");
        // A benign gate (phase-01 → phase-04) closes nothing.
        push_gate("foo/plan.phase-01", "foo/plan.phase-04", &records).unwrap();
    }

    #[test]
    fn plan_add_task_rejects_nonexistent_phase() {
        let (apg_root, dir) = fixture("task-no-phase");
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
            None,
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("no such phase"), "{err}");
        assert!(
            records
                .iter()
                .all(|r| !matches!(r, Record::Task { fqn, .. } if fqn.starts_with("foo/plan.phase-02"))),
            "a rejected task must not leave a partial Task record"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_link_rejects_nonexistent_phase() {
        let (apg_root, dir) = fixture("link-no-phase");
        let _path = write_plan(&apg_root); // plan has phase-01 only

        let err = plan_link_at(&apg_root, "foo", 2, &["R1".into()], &[]).unwrap_err();
        assert!(err.to_string().contains("not a phase of `foo`"), "{err}");
        // The plan JSONL is untouched (no partial write).
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(
            recs.iter()
                .all(|r| !matches!(r, Record::Satisfies { .. })),
            "a rejected link must not write Satisfies"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_complete_writes_durable_phase_status() {
        let (apg_root, dir) = fixture("durable-phase");

        // Pending task → the phase-complete gate rejects (nothing written).
        let _path = write_plan(&apg_root);
        assert!(plan_complete_at(&apg_root, "foo", 1).is_err());
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase {
                    fqn, status, ..
                } if fqn == "foo/plan.phase-01" => Some(status.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(status, "pending", "a rejected complete must not flip status");

        // Gate green → the milestone is durably recorded: status flips to done
        // in the JSONL (distinguishable from tasks-done + feedback-resolved).
        plan_done_at(&apg_root, "foo", "foo/plan.phase-01.task-1").unwrap();
        plan_complete_at(&apg_root, "foo", 1).unwrap();
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        let status = recs
            .iter()
            .find_map(|r| match r {
                Record::PlanPhase {
                    fqn, status, ..
                } if fqn == "foo/plan.phase-01" => Some(status.as_str()),
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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gate_rejects_unresolved_target_as_unrealized() {
        let (apg_root, dir) = fixture("apply-unresolved");

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

        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("not realized"), "{err}");
        assert!(err.to_string().contains("github.com/x/y.Missing"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rebuilds_graph_whose_delivered_descriptions_resolve() {
        // The apply→merge→rebuild path, unit level: the gate passes when every
        // planned node is realized; a rebuild of the (merged) graph then shows
        // the delivered descriptions — the requirement's Anchors + the
        // Implements terminal link — resolving to real code with a location.
        let (apg_root, dir) = fixture("apply-rebuild");

        // Spec: R1 anchored to the fixture's real Store, delivered by it.
        let spec = vec![
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Store".to_string(),
                body: "the store".to_string(),
                feature: String::new(),
            },
            Record::Anchors {
                from: "foo/spec.R1".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
            Record::Implements {
                from: "github.com/x/y.Store".to_string(),
                to: "foo/spec.R1".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &spec).unwrap();

        // Plan: phase Satisfies R1; the task Builds the planned node that the
        // fixture's scan has already realized (Store is present code).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let plan = vec![
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
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
            Record::Satisfies {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/spec.R1".to_string(),
            },
        ];
        specs::write_jsonl(&path, &plan).unwrap();

        // Gate green (planned node realized, no unresolved feedback).
        assert!(plan_apply_at(&apg_root, "foo").is_ok());

        // Rebuild the present graph the apply would produce: merge the branch's
        // committed spec/plan JSONLs into the code graph (the fixture's DB is
        // already the merged "main" code).
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        // The delivered requirement's anchor resolves to a real Struct with a
        // location (start_line present).
        let anchor = db
            .q("MATCH (:Requirement {fqn: 'foo/spec.R1'})-[:Anchors]->(s:Struct) RETURN s.fqn, s.start_line")
            .unwrap()
            .to_string();
        assert!(anchor.contains("github.com/x/y.Store"), "anchor: {anchor}");
        assert!(anchor.contains("1"), "realized struct has a location: {anchor}");
        // The Implements terminal link resolves against the rebuilt graph.
        let impls = db
            .q("MATCH (:Struct {fqn: 'github.com/x/y.Store'})-[:Implements]->(:Requirement {fqn: 'foo/spec.R1'}) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            impls.lines().last() == Some("1"),
            "delivered Implements resolves: {impls}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scoped_review_feedback_routes_and_gates_by_scope() {
        // Scoped-review routing (structural vs phase): feedback on the Plan
        // node is the structural scope, feedback on a PlanPhase is the phase
        // scope. The phase-complete gate only checks its phase's scope; the
        // apply gate checks every scope — so structural feedback does not
        // block a phase's completion milestone but blocks apply.
        let (apg_root, dir) = fixture("scoped-review");
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
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Store".to_string(),
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
        let err = plan_apply_at(&apg_root, "foo").unwrap_err();
        assert!(err.to_string().contains("unresolved review feedback"), "{err}");
        assert!(err.to_string().contains("foo/feedback-structural"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
