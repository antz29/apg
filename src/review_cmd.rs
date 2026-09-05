//! `apg review` — the closed writer↔reviewer feedback cycle (SPEC R25/R26).
//! A reviewer attaches a `Feedback` (`open`); a writer actions or wont-fixes it
//! (`actioned`); the reviewer then resolves (terminal) or rejects (reopens).
//! The writer cannot resolve and the reviewer cannot action — enforced by tool
//! permissions (R28), never by convention.

use std::path::{Path, PathBuf};

use crate::artifacts::{self, node_fqn, parse_args, reingest_project};
use crate::schema::Record;
use crate::spec_cmd::project_of;
use crate::specs;

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

/// `apg review add <target-fqn> --body … [--kind …] [--project <p>]` — attach
/// a `Feedback` (`open`) to any artifact node (spec, plan, task, or code).
/// Routing (R1): a spec/Future target serializes in `apg/specs/<project>.jsonl`;
/// a plan/task or code target in `apg/.trans/plans/<project>.jsonl`.
fn review_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(target) = p.positional.first() else {
        anyhow::bail!("usage: apg review add <target-fqn> --body … [--kind …] [--project <p>]");
    };
    let Some(body) = p.get("body") else {
        anyhow::bail!("review add requires --body");
    };
    // R26 accepts `--kind`; the Feedback record has no kind column, so it is
    // accepted for CLI compatibility and ignored.
    let _ = p.get("kind");
    let apg_root = require_apg_root()?;

    // Derive the project and the target file.
    let (project, target_is_spec) = if let Some(proj) = project_of(target) {
        let is_plan = target.starts_with(&format!("future/{proj}/plan"));
        (proj, !is_plan)
    } else {
        // A code (or other non-`future/`) target: the project is required and
        // code reviews live in the plan JSONL.
        let proj = p
            .get("project")
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "target `{target}` is not a `future/…` fqn — pass --project <p> for code-target reviews"
                )
            })?;
        (proj, false)
    };

    {
        let db = artifacts::ArtifactDb::open(&apg_root)?;
        if !db.has_node(target) {
            anyhow::bail!("review target `{target}` does not exist in the graph");
        }
    }

    let fqn = format!(
        "future/{project}/feedback-{}",
        feedback_number(&apg_root, &project, target_is_spec)?
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

    let file = if target_is_spec {
        specs::spec_jsonl_path(&apg_root, &project)
    } else {
        specs::plan_jsonl_path(&apg_root, &project)
    };
    let mut records = if file.exists() {
        specs::read_jsonl(&file)?
    } else {
        Vec::new()
    };
    records.push(rec);
    records.push(edge);
    specs::write_jsonl(&file, &records)?;
    if let Err(e) = reingest_project(&apg_root, &project) {
        eprintln!("warning: write-through re-ingest failed: {e:#}");
    }
    println!("Attached {fqn} (open) → {target}");
    Ok(())
}

/// The next free `feedback-<n>` in the target file.
fn feedback_number(apg_root: &Path, project: &str, target_is_spec: bool) -> anyhow::Result<u64> {
    let file = if target_is_spec {
        specs::spec_jsonl_path(apg_root, project)
    } else {
        specs::plan_jsonl_path(apg_root, project)
    };
    let records = if file.exists() {
        specs::read_jsonl(&file)?
    } else {
        Vec::new()
    };
    Ok(artifacts::next_free(&records, "feedback"))
}

/// `apg review action <feedback-fqn> --fix|--wont-fix [--note …]` — the writer
/// actions it (`actioned`, disposition set).
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
    set_feedback(fqn, "actioned", Some(disposition.to_string()), p.get("note"))
}

/// `apg review resolve <feedback-fqn>` / `reject <feedback-fqn>` — the
/// reviewer accepts (`resolved`, terminal) or rejects the action (back to
/// `open`, disposition `rejected`).
fn review_set(args: &[String], status: &str, disposition: Option<String>) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(fqn) = p.positional.first() else {
        anyhow::bail!("usage: apg review {} <feedback-fqn>", if status == "resolved" { "resolve" } else { "reject" });
    };
    set_feedback(fqn, status, disposition, None)
}

/// Locates the file a feedback fqn lives in (spec or plan JSONL) and updates
/// its status/disposition, write-through.
fn set_feedback(
    fqn: &str,
    status: &str,
    disposition: Option<String>,
    _note: Option<String>,
) -> anyhow::Result<()> {
    let project = project_of(fqn).ok_or_else(|| {
        anyhow::anyhow!("feedback fqn `{fqn}` must be `future/<project>/feedback-<n>`")
    })?;
    let apg_root = require_apg_root()?;

    let candidates = [
        specs::spec_jsonl_path(&apg_root, &project),
        specs::plan_jsonl_path(&apg_root, &project),
    ];
    let mut file: Option<PathBuf> = None;
    for c in &candidates {
        if c.exists() && specs::read_jsonl(c)?.iter().any(|r| node_fqn(r) == Some(fqn)) {
            file = Some(c.clone());
            break;
        }
    }
    let Some(file) = file else {
        anyhow::bail!("feedback `{fqn}` not found in {project}'s spec or plan JSONL");
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
    specs::write_jsonl(&file, &records)?;
    if let Err(e) = reingest_project(&apg_root, &project) {
        eprintln!("warning: write-through re-ingest failed: {e:#}");
    }
    println!("Feedback {fqn} → {status}{}", disposition.map(|d| format!(" ({d})")).unwrap_or_default());
    Ok(())
}

/// `apg review list [<target-fqn>]` — list feedback with status.
fn review_list(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let apg_root = require_apg_root()?;
    let db = artifacts::ArtifactDb::open(&apg_root)?;
    let target = p.positional.first();
    let mut q = "MATCH (f:Feedback)-[:Reviews]->(n) RETURN f.fqn, f.status, f.disposition, n.fqn".to_string();
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