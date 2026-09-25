//! `apg review` — the closed, coordinator-mediated writer↔reviewer feedback
//! cycle (SPEC R25/R26). A reviewer attaches a `Feedback` (`open`); the owning
//! writer works it and returns an ACTIONED/WONT-FIX claim; the coordinator
//! performs the shallow claim-vs-change consistency check and then actions the
//! item (`actioned`); the reviewer then resolves (terminal) or rejects
//! (reopens). The writer cannot resolve and the reviewer cannot action —
//! enforced by tool permissions (R28), never by convention; the coordinator is
//! the only actor that runs `apg review action`.
//!
//! Feedback is **transient** (apg-projects SPEC §5): branch-lifecycle data,
//! never committed. Both halves of the relationship — the `Feedback` record
//! AND its `Reviews` edge — live under the gitignored `apg/.trans/`, in the
//! tier dir of the attached node: `.trans/plans/<project>.jsonl` for
//! plan-phase/task targets (the plan store itself), `.trans/<tier>/<project>
//! .jsonl` for the five tier mirrors (requirements/domain/solution/
//! implementation/global). Review state dies with the branch; the reviewed
//! nodes persist.

use std::path::{Path, PathBuf};

use crate::artifacts::{self, ParsedArgs, node_fqn, parse_args};
use crate::layers::Layer;
use crate::schema::Record;
use crate::specs;

/// The project of a project-scoped plan/review fqn (`<project>/plan.phase-01`,
/// `<project>/feedback-1`). Callers must already know the fqn is plan-family
/// (a feedback fqn, a plan-family FQN) — for arbitrary fqns use the DB to
/// discriminate code nodes first. Durable layer FQNs
/// (`<layer>.<type>.<name>`) carry no project and never reach this helper.
fn project_of(fqn: &str) -> Option<String> {
    fqn.split('/').next().map(|s| s.to_string())
}

fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

pub fn cmd_review(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg review <add|action|resolve|reject|list> …");
    };
    match sub {
        "add" => review_add(&args[1..]),
        "action" => review_action(&args[1..]),
        "resolve" => review_set(&args[1..], "resolved", None),
        "reject" => review_set(&args[1..], "open", Some("rejected".to_string())),
        "list" => review_list(&args[1..]),
        other => anyhow::bail!("unknown apg review subcommand: {other}"),
    }
}

/// `apg review add <target-fqn> --body … [--kind …] [--project <p>]
/// [--checks <invariant-fqn>]*` — attach a `Feedback` (`open`) to any artifact
/// node (a durable layer node, a plan/phase/task, or code). Routing (SPEC
/// §5): both halves land in the `.trans` tier mirror of the attached node —
/// plan-phase/task targets in `.trans/plans/<project>.jsonl` (the plan
/// store), durable/code targets in `.trans/<tier>/<project>.jsonl`. Code and
/// durable-layer targets (whose FQNs carry no project prefix) need an
/// explicit `--project`; plan-family targets derive it from their FQN.
fn review_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    if p.positional.is_empty() {
        anyhow::bail!(
            "usage: apg review add <target-fqn> --body … [--kind …] [--project <p>] [--checks <invariant-fqn>]*"
        );
    }
    if p.get("body").is_none() {
        anyhow::bail!("review add requires --body");
    }
    let apg_root = require_apg_root()?;
    let _lock = artifacts::acquire_spec_lock(&apg_root)?;
    apply_review_add(&apg_root, &p)
}

/// Core of `review add`, split from the CLI wrapper so tests can drive it
/// against a fixture root (the `apply_invariant_add` pattern).
///
/// No `ArtifactDb` may stay live across the write-through: opening a second
/// `Database` on the same `db.lbug` while a first is still open corrupts the
/// file (lbug checkpoints from a stale buffer-manager view) — the merged
/// Feedback node vanishes and every later write-through SIGSEGVs in the
/// engine (`LocalNodeTable::isVisible`). Every DB handle here is scoped to a
/// block and dropped before `write_jsonl_and_reingest`.
///
/// Public so the relocated e2e crates reach it as
/// `apg::review_cmd::apply_review_add`.
pub fn apply_review_add(apg_root: &Path, p: &ParsedArgs) -> anyhow::Result<()> {
    let target = p.positional.first().expect("review add requires a target");
    let body = p.get("body").expect("review add requires --body");
    // R26 accepts `--kind`; the Feedback record has no kind column, so it is
    // accepted for CLI compatibility and ignored.
    let _ = p.get("kind");

    // Discriminate a code target from an authored (durable/plan) target by
    // its DB node label: code nodes (Module/Struct/Function/File/
    // UnresolvedTarget) need an explicit `--project`; plan-family targets
    // (`<project>/plan…`) derive the project from their FQN's first segment;
    // durable layer nodes (`<layer>.<type>.<name>` — no project prefix, the
    // old `<project>/spec.*` vocabulary is gone) also need an explicit
    // `--project` to route the feedback's `<project>/feedback-<n>` fqn.
    //
    // The routing DB is scoped so it is dropped before the write-through
    // re-ingest below: opening a second `Database` on the same `db.lbug` while
    // a first is still live corrupts the file (lbug checkpoint from a stale
    // buffer-manager view) — the Feedback node vanishes and later write-throughs
    // SIGSEGV in the engine.
    let (project, tier) = {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        match db.node_label(target) {
            Some(l) if crate::load::is_code_label(l) => {
                let proj = p
                    .get("project")
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "target `{target}` is a code node — pass --project <p> for code-target reviews"
                        )
                    })?;
                (proj, Layer::Implementation)
            }
            Some(_) => {
                // Plan-family FQNs (`<project>/plan…` — plan, phase, task,
                // plan note) carry the project as their first segment;
                // durable layer nodes carry none and need `--project`.
                let proj = if target.contains('/') {
                    project_of(target).ok_or_else(|| {
                        anyhow::anyhow!(
                            "target `{target}` is a spec/plan node without a project prefix"
                        )
                    })?
                } else {
                    p.get("project").ok_or_else(|| {
                        anyhow::anyhow!(
                            "target `{target}` is a durable layer node without a project prefix — pass --project <p> for durable-node reviews"
                        )
                    })?
                };
                let tier = feedback_tier(target, &proj);
                (proj, tier)
            }
            None => {
                anyhow::bail!("review target `{target}` does not exist in the graph");
            }
        }
    };

    let fqn = format!(
        "{project}/feedback-{}",
        feedback_number(apg_root, &project)?
    );
    let rec = Record::Feedback {
        fqn: fqn.clone(),
        body,
        status: "open".to_string(),
        disposition: String::new(),
    };
    let edge = Record::Reviews {
        from: fqn.clone(),
        to: target.to_string(),
    };

    // Both halves of the relationship land in `.trans` — the tier mirror of
    // the attached node (SPEC §5). Nothing here ever touches committed node
    // files, `apg/specs/`, or `apg/notes/`.
    let file = write_transient_feedback(apg_root, &project, tier, &[rec, edge])?;
    println!(
        "Attached {fqn} (open) → {target} (mirror: {})",
        file.display()
    );
    Ok(())
}

/// The tier a feedback target routes to (SPEC §5): a plan-family FQN
/// (`<project>/plan…` — plan, phase, task, plan note) → Plans, whose
/// `.trans` dir is the plan store itself; a durable node FQN with a layer
/// prefix (`requirements.`/`domain.`/`solution.`/`implementation.`/`global.`)
/// → that layer's tier mirror; anything else (a code-shaped FQN that
/// resolved to a non-code label, e.g. a Feedback) → Global.
fn feedback_tier(target: &str, project: &str) -> Layer {
    if target.starts_with(&format!("{project}/plan")) {
        return Layer::Plans;
    }
    for layer in [
        Layer::Requirements,
        Layer::Domain,
        Layer::Solution,
        Layer::Implementation,
        Layer::Global,
    ] {
        if target.starts_with(&format!("{}.", layer.layer_dir())) {
            return layer;
        }
    }
    Layer::Global
}

/// Route one feedback + Reviews pair into the transient mirror for the
/// attached node's tier (SPEC §5): `apg/.trans/plans/<project>.jsonl` for
/// plan-family targets, `apg/.trans/<tier>/<project>.jsonl` for the five
/// tier mirrors. Both halves of the relationship — the `Feedback` record AND
/// the `Reviews` edge — live in `.trans`; never in committed node files,
/// never in `apg/specs/` or `apg/notes/` (and `.trans` is gitignored, so the
/// write-through never auto-commits). Returns the mirror path written.
///
/// **Commit-then-project** (phase-05 task-15): the mirror write lands FIRST
/// (via `write_jsonl_and_reingest`) and the exact projection delta is applied
/// only after it, so the newly attached Feedback is immediately queryable by a
/// separate `apg query` process with no scan. The delta is computed inside the
/// funnel (task-3) — this caller never threads a delete set.
fn write_transient_feedback(
    apg_root: &Path,
    project: &str,
    tier: Layer,
    records: &[Record],
) -> anyhow::Result<PathBuf> {
    let file = specs::transient_feedback_path(apg_root, project, tier);
    let mut existing = if file.exists() {
        specs::read_jsonl(&file)?
    } else {
        Vec::new()
    };
    existing.extend_from_slice(records);
    artifacts::write_jsonl_and_reingest(apg_root, &file, project, &existing)?;
    Ok(file)
}

/// The next free `feedback-<n>` across the project's six transient files —
/// the plan store plus the five tier mirrors. Feedback FQNs share one
/// `<project>/feedback-<n>` namespace regardless of which mirror a review
/// routes to; numbering per-file would give two reviews the same FQN and the
/// re-ingest would collapse them into one Feedback node with both edges.
/// The namespace starts at `feedback-1` even when no transient file exists
/// yet (a plan-less project reviewing a durable/code node): the counter is
/// seeded at 1, matching `artifacts::next_free`, which returns 1 for an
/// empty record set.
fn feedback_number(apg_root: &Path, project: &str) -> anyhow::Result<u64> {
    let mut n: u64 = 1;
    for file in specs::project_transient_files(apg_root, project) {
        if file.exists() {
            let records = specs::read_jsonl(&file)?;
            n = n.max(artifacts::next_free(&records, "feedback"));
        }
    }
    Ok(n)
}

/// `apg review action <feedback-fqn> --fix|--wont-fix [--note …]` — the
/// coordinator actions the item (`actioned`, disposition set) after the owning
/// writer returns an ACTIONED/WONT-FIX claim and the shallow claim-vs-change
/// consistency check passes; reviewers never action.
fn review_action(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(fqn) = p.positional.first() else {
        anyhow::bail!("usage: apg review action <feedback-fqn> --fix|--wont-fix [--note …]");
    };
    let disposition = if p.has("fix") {
        "fixed"
    } else if p.has("wont-fix") {
        "wont-fix"
    } else {
        anyhow::bail!("action requires --fix or --wont-fix");
    };
    set_feedback(
        fqn,
        "actioned",
        Some(disposition.to_string()),
        p.get("note"),
    )
}

/// `apg review resolve <feedback-fqn>` / `reject <feedback-fqn>` — the
/// reviewer accepts (`resolved`, terminal) or rejects the action (back to
/// `open`, disposition `rejected`).
fn review_set(args: &[String], status: &str, disposition: Option<String>) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(fqn) = p.positional.first() else {
        anyhow::bail!(
            "usage: apg review {} <feedback-fqn>",
            if status == "resolved" {
                "resolve"
            } else {
                "reject"
            }
        );
    };
    set_feedback(fqn, status, disposition, None)
}

/// Locates the transient file a feedback fqn lives in (the plan store or one
/// of the five tier mirrors) and updates its status/disposition, write-through.
fn set_feedback(
    fqn: &str,
    status: &str,
    disposition: Option<String>,
    _note: Option<String>,
) -> anyhow::Result<()> {
    let project = project_of(fqn)
        .ok_or_else(|| anyhow::anyhow!("feedback fqn `{fqn}` must be `<project>/feedback-<n>`"))?;
    let apg_root = require_apg_root()?;
    set_feedback_at(&apg_root, fqn, &project, status, disposition)
}

/// Core of `set_feedback`: update a feedback node's status/disposition in the
/// transient file that carries it (the plan store or one of the five tier
/// mirrors), write-through.
///
/// **Commit-then-project** (phase-05 task-16): the mutated mirror lands FIRST
/// (via `write_jsonl_and_reingest`) and the exact projection delta — including
/// the changed Feedback FQN — is applied only after it, so an
/// actioned/resolved status is immediately queryable by a separate `apg query`
/// process with no scan. The delta is computed inside the funnel (task-3).
///
/// Public so the relocated e2e crates reach it as
/// `apg::review_cmd::set_feedback_at`.
pub fn set_feedback_at(
    apg_root: &Path,
    fqn: &str,
    project: &str,
    status: &str,
    disposition: Option<String>,
) -> anyhow::Result<()> {
    let _lock = artifacts::acquire_spec_lock(apg_root)?;

    let candidates = specs::project_transient_files(apg_root, project);
    let mut file: Option<PathBuf> = None;
    for c in &candidates {
        if c.exists()
            && specs::read_jsonl(c)?
                .iter()
                .any(|r| node_fqn(r) == Some(fqn))
        {
            file = Some(c.clone());
            break;
        }
    }
    let Some(file) = file else {
        anyhow::bail!(
            "feedback `{fqn}` not found in {project}'s transient plan store or feedback mirrors"
        );
    };
    let mut records = specs::read_jsonl(&file)?;
    let mut found = false;
    for r in &mut records {
        match r {
            Record::Feedback {
                fqn: f,
                status: s,
                disposition: d,
                ..
            } if f == fqn => {
                *s = status.to_string();
                if let Some(new_d) = &disposition {
                    *d = new_d.clone();
                }
                found = true;
            }
            _ => {}
        }
    }
    if !found {
        anyhow::bail!("feedback `{fqn}` not found");
    }
    artifacts::write_jsonl_and_reingest(apg_root, &file, project, &records)?;
    println!(
        "Feedback {fqn} → {status}{}",
        disposition.map(|d| format!(" ({d})")).unwrap_or_default()
    );
    Ok(())
}

/// `apg review list [<target-fqn>]` — list feedback with status.
fn review_list(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let apg_root = require_apg_root()?;
    let db = artifacts::ArtifactDb::open(&apg_root)?;
    let target = p.positional.first();
    let mut q = "MATCH (f:Feedback)-[:Reviews]->(n) RETURN f.fqn, f.status, f.disposition, n.fqn"
        .to_string();
    if let Some(t) = target {
        q = format!(
            "MATCH (f:Feedback)-[:Reviews]->(n) WHERE n.fqn = {} RETURN f.fqn, f.status, f.disposition, n.fqn",
            artifacts::lit(t)
        );
    }
    let conn = db.conn()?;
    let result = conn.query(&q)?;
    let names = result.get_column_names();
    println!("{}", names.join(","));
    for row in result {
        let cells: Vec<String> = row.iter().map(|v| v.to_string()).collect();
        println!("{}", cells.join(","));
    }
    Ok(())
}
