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
//! holds — SPEC §5: every solution node's `implemented-by` FQN touched by a
//! plan task) precedes the merge + rebuild of `main`'s graph
//! (PlanCompletion-SPEC.md).

use std::collections::BTreeSet;
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
        anyhow::bail!("usage: apg plan <init|add|link|done|undone|note|complete|render|verify> …");
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
        "verify" => plan_verify(&args[1..]),
        // R5: `apg plan apply` was renamed `verify` — the binary applies
        // nothing; verify is the pre-merge coherence gate.
        "apply" => anyhow::bail!(
            "`apg plan apply` was renamed `verify` (R5) — the binary applies nothing; run `apg plan verify <project>` (the merge act is `apg project merge <project>`)"
        ),
        other => anyhow::bail!("unknown apg plan subcommand: {other}"),
    }
}

/// `apg plan init <project> [--title T] [--strategy S]` — the plan is the
/// tier-4 bridge for a project whose requirements live in the layers store
/// (`apg/layers/requirements/`, SPEC §5). The legacy spec-exists gate (the
/// old `apg/specs/<project>.jsonl` and `apg spec init`) is gone: a plan
/// without any requirement node files yet is allowed, but the empty spec is
/// surfaced as a warning — phases added with `--satisfies` validate against
/// the layers store and will refuse until the requirement tier exists.
fn plan_init(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg plan init <project> [--title T] [--strategy S]");
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
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
    println!("Created plan {project} at {}", path.display());
    Ok(())
}

/// Core of `plan_init`: writes the Plan record into the transient plan store
/// (`.trans/plans/<project>.jsonl`). Returns whether the layers store holds
/// any requirement node file — the spec-exists gate, resolved against the
/// layers store (never the legacy spec JSONL); the CLI wrapper surfaces a
/// warning when no requirements exist yet (a warning, never a blocker).
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
            let (Some(node_kind), Some(fqn)) =
                (p.positional.get(2).map(|s| s.as_str()), p.positional.get(3))
            else {
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
/// before any write), validates kind/tier, validates the Task→Implementation
/// verb + target FQN(s) against the scanned graph and the planned-node
/// universe, and appends the Task + Contains records.
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
        // Requirements live in the layers store: FQN `requirements.requirement.<name>`.
        let req_fqn = format!("requirements.requirement.{req}");
        if !spec_has_requirement(apg_root, project, &req_fqn)? {
            anyhow::bail!(
                "satisfies target `{req}` is not a requirement of `{project}` — requirements live in apg/layers/requirements/"
            );
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
/// `cycle_closing_path` machinery `apg spec add phase` / `apg plan link` use)
/// before the edge is ever accumulated into the records — so the JSONL/DB
/// write-through never runs on a cycle. `records` must already have the
/// phase's stale incident edges removed (the add/link callers do this).
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
/// - derived solution coverage holds (SPEC §5): every solution node's
///   `implemented-by` FQN is touched by at least one plan task ([`coverage_check`]);
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

    // 3. Derived solution coverage (SPEC §5): every solution node's
    // `implemented-by` FQN must be touched by at least one plan task — the
    // plan is the HOW for the whole solution, and the bridge is complete iff
    // coverage holds. Task targets come from the transient plan records
    // (note-26: the DB's Task table is static — the plan JSONL is the source
    // of truth), the solution nodes from the durable layers store. The
    // suggestion names the verb the uncovered FQN's status calls for: a
    // `modifies` over code that already resolves, a `creates` over a planned
    // or still-absent FQN.
    let coverage = coverage_check(&records, &crate::layers::read_existing_nodes(apg_root)?);
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

/// The derived-coverage verdict (SPEC §5): whether every solution node's
/// `implemented-by` FQN is touched by at least one plan task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageReport {
    /// The uncovered `implemented-by` FQNs with their owning solution node —
    /// non-empty iff coverage does NOT hold (the coherence gate refuses).
    pub gaps: Vec<CoverageGap>,
    /// Solution nodes declaring NO `implemented-by` edge — exempt from the
    /// coverage rule (with no FQN there is nothing to touch), surfaced as a
    /// warning so the bridge gap ("no code claims this node") stays visible.
    pub no_claims: Vec<String>,
}

/// Derived solution coverage (SPEC §5): every solution-layer node's
/// (System/Container/Component) `implemented-by` code FQN must be touched by
/// at least one plan task — the plan is the HOW for the whole solution, and
/// the bridge is complete iff coverage holds.
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
/// blocker): the bridge has a gap the spec-writer should close by authoring
/// the edge.
///
/// Pure — no I/O. The caller supplies the transient plan records (note-26:
/// the `.trans/plans/<project>.jsonl`, not the DB's static Task table, is the
/// task source of truth) and the durable node files
/// ([`crate::layers::read_existing_nodes`]).
pub(crate) fn coverage_check(
    records: &[Record],
    nodes: &[crate::layers::NodeFile],
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
    let mut gaps: Vec<CoverageGap> = Vec::new();
    let mut no_claims: Vec<String> = Vec::new();
    for n in nodes {
        if n.layer != "solution"
            || !["system", "container", "component"].contains(&n.node_type.as_str())
        {
            continue;
        }
        let solution = crate::layers::fqn(crate::layers::Layer::Solution, &n.node_type, &n.name);
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

    #[test]
    fn plan_link_rejects_nonexistent_phase() {
        let (apg_root, repo, _wt) = fixture("link-no-phase");
        let _path = write_plan(&apg_root); // plan has phase-01 only

        let err = plan_link_at(&apg_root, "foo", 2, &["R1".into()], &[]).unwrap_err();
        assert!(err.to_string().contains("not a phase of `foo`"), "{err}");
        // The plan JSONL is untouched (no partial write).
        let recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(
            recs.iter().all(|r| !matches!(r, Record::Satisfies { .. })),
            "a rejected link must not write Satisfies"
        );

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
    // task-4 (coverage_check): derived solution coverage in plan verify —
    // every solution node's implemented-by FQN must be touched by at least
    // one plan task (SPEC §5); the coherence gate refuses when it does not.
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
        // FQN counts exactly like a modifies over real code).
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
        // Two solution nodes; the plan touches only the real Struct — the
        // container's absent Gateway FQN is uncovered, so verify refuses,
        // naming the FQN, its solution node, and the matching creates
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
        // A renames task whose destination equals the implemented-by FQN
        // touches it (the new_fqn half of the pair) — the bridge holds.
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
        // No solution-layer node files at all — coverage is a no-op (there
        // is nothing to touch) and verify passes on the other gates alone.
        let (apg_root, repo, _wt) = fixture("coverage-empty");
        let _path = write_plan(&apg_root);
        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        testutil::remove(&repo);
    }

    #[test]
    fn coverage_exempts_solution_node_without_implemented_by_edge() {
        // A solution node with NO implemented-by edge is exempt from the
        // coverage rule — the SPEC's rule is over the node's implemented-by
        // FQNs, and with none there is nothing to touch (nothing blocks).
        // The gap is surfaced as a warning (no code claims the node), never a
        // blocker: the no-claims list is part of the report.
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
        let report = coverage_check(&records, &nodes);
        assert_eq!(report.no_claims, vec!["solution.system.payments"]);
        assert!(report.gaps.is_empty());

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_check_reports_gaps_no_claims_and_verb_agnostic_touches() {
        // Pure semantics: exact-FQN touch matching, verb-agnostic (a deletes
        // task touches what it deletes), a renames/moves new_fqn covering the
        // destination, a creates covering a planned FQN, empty-target tasks
        // touching nothing, and non-solution nodes (person, solution notes)
        // ignored.
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
        let report = coverage_check(&records, &nodes);
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
    // task-5 (coverage tests): the task-text audit gaps — the green path
    // across MULTIPLE solution nodes, both halves of a renames/moves pair as
    // touches, and a refusal that names ONLY the uncovered node.
    // ------------------------------------------------------------------

    #[test]
    fn coverage_holds_across_multiple_solution_nodes() {
        // The bridge is complete only when EVERY solution node's implemented-by
        // FQNs are touched — this is the green end-to-end path through
        // plan_verify_at across MULTIPLE solution nodes (two containers; one
        // real FQN, one still-absent FQN), not just several FQNs on a single
        // node. Verify returns its green verdict and the derived report agrees:
        // no gaps, no no-claims warnings.
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

        // Every solution node's every implemented-by FQN is touched -> green.
        assert!(plan_verify_at(&apg_root, "foo").is_ok());
        let nodes = crate::layers::read_existing_nodes(&apg_root).unwrap();
        let report = coverage_check(&records, &nodes);
        assert!(report.gaps.is_empty(), "{report:?}");
        assert!(report.no_claims.is_empty(), "{report:?}");

        testutil::remove(&repo);
    }

    #[test]
    fn coverage_refusal_names_only_the_uncovered_solution() {
        // Mixed coverage: the component's real FQN is covered by a modifies
        // task, the container's absent FQN is not. The refusal names the
        // uncovered node/FQN only — the covered node and its claim appear
        // nowhere in the message (no false positives for a partially-covered
        // bridge).
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
        // lands). Here the two halves are different solution nodes'
        // implemented-by targets, so the single `moves` covers both and the
        // bridge holds. (coverage_check counts `target` for every verb plus
        // `new_fqn` for renames/moves — the pair is one claim across two
        // locations; the existing renames test covers the destination half.)
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
        let report = coverage_check(&records, &nodes);
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
        // declaration, link, complete (the durable milestone), and undone.
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

        // link (an edge refresh — still a write-through), then done →
        // complete (the durable milestone) → undone.
        plan_link_at(&apg_root, "foo", 1, &[], &[]).unwrap();
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
}
