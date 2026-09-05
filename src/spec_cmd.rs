//! `apg spec` — authoring, lifecycle, and rendering of graph-native specs
//! (SPEC R6-R9, R17-R19). Every mutation is write-through (R5): load the
//! project JSONL, apply the change, write it back, re-ingest into the live DB.

use std::path::{Path, PathBuf};

use crate::artifacts::{
    self, node_fqn, parse_args, reingest_project, remove_node, ParsedArgs,
};
use crate::schema::Record;
use crate::specs;

fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

/// Reads a project's spec JSONL (erroring when it does not exist).
pub(crate) fn load_project(apg_root: &Path, project: &str) -> anyhow::Result<Vec<Record>> {
    let path = specs::spec_jsonl_path(apg_root, project);
    if !path.exists() {
        anyhow::bail!(
            "no spec for project `{project}` — run `apg spec init {project}` first"
        );
    }
    specs::read_jsonl(&path)
}

/// Writes a project's spec JSONL and re-ingests it into the live DB (R5). A
/// missing DB (no scan yet) is a warning, not an error — the JSONL is the
/// durable form.
pub(crate) fn write_through(apg_root: &Path, project: &str, records: &[Record]) -> anyhow::Result<()> {
    specs::write_jsonl(&specs::spec_jsonl_path(apg_root, project), records)?;
    if let Err(e) = reingest_project(apg_root, project) {
        eprintln!("warning: write-through re-ingest failed: {e:#}");
    }
    Ok(())
}

/// The project of a `future/<project>/…` fqn.
pub fn project_of(fqn: &str) -> Option<String> {
    let rest = fqn.strip_prefix("future/")?;
    rest.split('/').next().map(|s| s.to_string())
}

/// The next free numbered `spec.<prefix>-<n>` counter (ng/ac/vi/…).
fn next_spec_counter(records: &[Record], prefix: &str) -> u64 {
    let needle = format!("spec.{prefix}-");
    records
        .iter()
        .filter_map(|r| node_fqn(r))
        .filter_map(|f| f.split(&needle).nth(1))
        .filter_map(|s| s.parse::<u64>().ok())
        .max()
        .map(|n| n + 1)
        .unwrap_or(1)
}

pub fn cmd_spec(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg spec <init|add|anchor|link|rm|render|promote|archive> …");
    };
    match sub {
        "init" => spec_init(&args[1..]),
        "add" => spec_add(&args[1..]),
        "anchor" => spec_anchor(&args[1..]),
        "link" => spec_link(&args[1..]),
        "rm" => spec_rm(&args[1..]),
        "render" => spec_render(&args[1..]),
        "promote" => spec_promote(&args[1..]),
        "archive" => spec_archive(&args[1..]),
        other => anyhow::bail!("unknown apg spec subcommand: {other}"),
    }
}

/// `apg spec init <project> [--title T] [--goal G]` (R6).
fn spec_init(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg spec init <project> [--title T] [--goal G]");
    };
    let apg_root = require_apg_root()?;
    let path = specs::spec_jsonl_path(&apg_root, project);
    if path.exists() {
        anyhow::bail!("spec for project `{project}` already exists at {}", path.display());
    }
    let title = p.get("title").unwrap_or_else(|| project.clone());
    let goal = p.get("goal").unwrap_or_default();
    let records = vec![Record::Spec {
        fqn: format!("future/{project}/spec"),
        title,
        goal,
    }];
    write_through(&apg_root, project, &records)?;
    println!("Created spec {project} at {}", path.display());
    Ok(())
}

/// `apg spec add <project> <kind> …` (R7).
fn spec_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg spec add <project> <requirement|future|phase|decision|non-goal|acceptance-criterion|verification|note> …");
    };
    let Some(kind) = p.positional.get(1).map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg spec add <project> <kind> …");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_project(&apg_root, project)?;

    let spec_fqn = format!("future/{project}/spec");
    match kind {
        "requirement" => {
            let Some(id) = p.positional.get(2) else {
                anyhow::bail!("usage: apg spec add <project> requirement <id> [--title …] [--body …] [--feature …] [--depends-on <id>]* [--anchor <fqn>]*");
            };
            let fqn = format!("future/{project}/spec.{id}");
            let mut recs = vec![Record::Requirement {
                fqn: fqn.clone(),
                id: id.clone(),
                title: p.get("title").unwrap_or_default(),
                body: p.get("body").unwrap_or_default(),
                feature: p.get("feature").unwrap_or_default(),
            }];
            recs.push(Record::Contains {
                from: spec_fqn.clone(),
                to: fqn.clone(),
            });
            for dep in p.all("depends-on") {
                let dep_fqn = format!("future/{project}/spec.{dep}");
                if dep == *id {
                    anyhow::bail!("requirement `{id}` cannot depend on itself");
                }
                if !records
                    .iter()
                    .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &dep_fqn))
                {
                    anyhow::bail!(
                        "depends-on target `{dep}` is not an existing requirement in `{project}`"
                    );
                }
                recs.push(Record::DependsOn {
                    from: fqn.clone(),
                    to: dep_fqn,
                });
            }
            {
                let db = artifacts::ArtifactDb::open(&apg_root)?;
                for a in p.all("anchor") {
                    db.resolve_anchor(&a)?;
                    recs.push(Record::Anchors {
                        from: fqn.clone(),
                        to: a,
                    });
                }
            }
            remove_node(&mut records, &fqn);
            records.extend(recs);
            write_through(&apg_root, project, &records)?;
            println!("Added requirement {id} to {project}");
        }
        "future" => {
            let Some(name) = p.positional.get(2) else {
                anyhow::bail!("usage: apg spec add <project> future <name> --kind <function|struct|service|rpc|endpoint|other> [--target <fqn>]");
            };
            let Some(kind) = p.get("kind") else {
                anyhow::bail!("future requires --kind");
            };
            if !["function", "struct", "service", "rpc", "endpoint", "other"]
                .contains(&kind.as_str())
            {
                anyhow::bail!(
                    "invalid future kind `{kind}` — one of function/struct/service/rpc/endpoint/other"
                );
            }
            let fqn = format!("future/{project}/{name}");
            if records
                .iter()
                .any(|r| matches!(r, Record::Future { fqn: f, .. } if f == &fqn))
            {
                anyhow::bail!("future `{name}` already exists in `{project}`");
            }
            records.push(Record::Future {
                fqn,
                kind,
                target: p.get("target").unwrap_or_default(),
            });
            write_through(&apg_root, project, &records)?;
            println!("Added future `{name}` to {project}");
        }
        "phase" => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!("usage: apg spec add <project> phase <n> --title … [--gate <phase-n>]*");
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("phase requires --title");
            };
            let fqn = format!("future/{project}/spec.phase-{n}");
            let mut recs = vec![Record::Phase {
                fqn: fqn.clone(),
                number: n,
                title,
            }];
            recs.push(Record::Contains {
                from: spec_fqn.clone(),
                to: fqn.clone(),
            });
            for g in p.all("gate") {
                let gate_n = g.parse::<u32>().map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
                if gate_n == n {
                    anyhow::bail!("a phase cannot gate on itself");
                }
                recs.push(Record::Gates {
                    from: fqn.clone(),
                    to: format!("future/{project}/spec.phase-{gate_n}"),
                });
            }
            remove_node(&mut records, &fqn);
            records.extend(recs);
            write_through(&apg_root, project, &records)?;
            println!("Added phase {n} to {project}");
        }
        "decision" => {
            let Some(id) = p.positional.get(2) else {
                anyhow::bail!("usage: apg spec add <project> decision <id> --summary …");
            };
            let Some(summary) = p.get("summary") else {
                anyhow::bail!("decision requires --summary");
            };
            let fqn = format!("future/{project}/spec.decision-{id}");
            let mut recs = vec![Record::Decision {
                fqn: fqn.clone(),
                id: id.clone(),
                summary,
            }];
            recs.push(Record::Contains {
                from: spec_fqn.clone(),
                to: fqn.clone(),
            });
            remove_node(&mut records, &fqn);
            records.extend(recs);
            write_through(&apg_root, project, &records)?;
            println!("Added decision `{id}` to {project}");
        }
        "non-goal" => {
            let body = p.get("body").ok_or_else(|| anyhow::anyhow!("non-goal requires --body"))?;
            let n = next_spec_counter(&records, "ng");
            let fqn = format!("future/{project}/spec.ng-{n}");
            records.push(Record::NonGoal { fqn: fqn.clone(), body });
            records.push(Record::Contains { from: spec_fqn.clone(), to: fqn });
            write_through(&apg_root, project, &records)?;
            println!("Added non-goal to {project}");
        }
        "acceptance-criterion" => {
            let body = p
                .get("body")
                .ok_or_else(|| anyhow::anyhow!("acceptance-criterion requires --body"))?;
            let n = next_spec_counter(&records, "ac");
            let fqn = format!("future/{project}/spec.ac-{n}");
            records.push(Record::AcceptanceCriterion { fqn: fqn.clone(), body });
            records.push(Record::Contains { from: spec_fqn.clone(), to: fqn });
            write_through(&apg_root, project, &records)?;
            println!("Added acceptance criterion to {project}");
        }
        "verification" => {
            let body = p
                .get("body")
                .ok_or_else(|| anyhow::anyhow!("verification requires --body"))?;
            let n = next_spec_counter(&records, "vi");
            let fqn = format!("future/{project}/spec.vi-{n}");
            records.push(Record::VerificationItem { fqn: fqn.clone(), body });
            records.push(Record::Contains { from: spec_fqn.clone(), to: fqn });
            write_through(&apg_root, project, &records)?;
            println!("Added verification item to {project}");
        }
        "note" => add_note(&p, &apg_root, project, &mut records)?,
        other => anyhow::bail!(
            "unknown spec add kind `{other}` — requirement|future|phase|decision|non-goal|acceptance-criterion|verification|note"
        ),
    }
    Ok(())
}

/// `apg spec add <project> note --body … [--kind …] [--on <fqn>]*` (R7).
/// A `--on` target that is a code FQN routes the note to the committed
/// `apg/notes/<module>.jsonl` ledger; a spec/Future FQN (or no target) to the
/// project's spec JSONL.
fn add_note(
    p: &ParsedArgs,
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let Some(body) = p.get("body") else {
        anyhow::bail!("note requires --body");
    };
    let kind = p.get("kind").unwrap_or_default();
    let ons = p.all("on");
    if ons.is_empty() {
        let n = artifacts::next_free(records, "note");
        let fqn = format!("future/{project}/note-{n}");
        records.push(Record::Note {
            fqn: fqn.clone(),
            body: body.clone(),
            kind: kind.clone(),
        });
        write_through(apg_root, project, records)?;
        println!("Added project note to {project}");
        return Ok(());
    }
    let db = artifacts::ArtifactDb::open(apg_root)?;
    for target in &ons {
        if !db.has_node(target) {
            anyhow::bail!("note target `{target}` does not exist in the graph");
        }
        // A code FQN routes to the per-module note ledger; a spec/Future FQN
        // (anything not in the code graph) to the project's spec JSONL.
        if db.code_label(target).is_some() {
            let file = db.note_file(apg_root, target);
            let mut ledger = if file.exists() {
                specs::read_jsonl(&file)?
            } else {
                Vec::new()
            };
            let n = artifacts::next_free(&ledger, "note");
            let fqn = format!("annotations/{n}");
            ledger.push(Record::Note {
                fqn: fqn.clone(),
                body: body.clone(),
                kind: kind.clone(),
            });
            ledger.push(Record::Details {
                from: fqn,
                to: target.clone(),
            });
            specs::write_jsonl(&file, &ledger)?;
            println!("Added note on `{target}` to {}", file.display());
        } else {
            let n = artifacts::next_free(records, "note");
            let fqn = format!("future/{project}/note-{n}");
            records.push(Record::Note {
                fqn: fqn.clone(),
                body: body.clone(),
                kind: kind.clone(),
            });
            records.push(Record::Details {
                from: fqn.clone(),
                to: target.clone(),
            });
            println!("Added note `{fqn}` to {project}");
        }
    }
    drop(db);
    // Code notes also land in the live DB immediately (R5): the ledger is
    // written above; `write_through` re-ingests the project spec/plan and
    // every note ledger (MERGE upserts the annotations nodes).
    write_through(apg_root, project, records)?;
    Ok(())
}

/// `apg spec anchor <project> <req-id> <fqn>` (R8).
fn spec_anchor(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(req_id), Some(fqn)) = (
        p.positional.first(),
        p.positional.get(1),
        p.positional.get(2),
    ) else {
        anyhow::bail!("usage: apg spec anchor <project> <req-id> <fqn>");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_project(&apg_root, project)?;
    let req_fqn = format!("future/{project}/spec.{req_id}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &req_fqn))
    {
        anyhow::bail!("requirement `{req_id}` does not exist in `{project}`");
    }
    {
        let db = artifacts::ArtifactDb::open(&apg_root)?;
        db.resolve_anchor(fqn)?;
    }
    artifacts::remove_incident_edges(&mut records, &req_fqn);
    records.push(Record::Anchors {
        from: req_fqn,
        to: fqn.clone(),
    });
    write_through(&apg_root, project, &records)?;
    println!("Anchored {project}.{req_id} → {fqn}");
    Ok(())
}

/// `apg spec link <project> <req-id> [--depends-on <id>]*` (R8).
fn spec_link(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(req_id)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg spec link <project> <req-id> [--depends-on <id>]*");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_project(&apg_root, project)?;
    link_depends_on(project, req_id, &p.all("depends-on"), &mut records)?;
    write_through(&apg_root, project, &records)?;
    println!("Linked {project}.{req_id} depends-on");
    Ok(())
}

/// Applies the depends-on edges for `req_id`: removes its existing edges once,
/// then adds every dep (R8). A dep may repeat across calls (idempotent upsert);
/// self-deps and undeclared targets are write-time errors.
fn link_depends_on(
    project: &str,
    req_id: &str,
    deps: &[String],
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let req_fqn = format!("future/{project}/spec.{req_id}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &req_fqn))
    {
        anyhow::bail!("requirement `{req_id}` does not exist in `{project}`");
    }
    artifacts::remove_incident_edges(records, &req_fqn);
    for dep in deps {
        let dep_fqn = format!("future/{project}/spec.{dep}");
        if dep.as_str() == req_id {
            anyhow::bail!("requirement `{req_id}` cannot depend on itself");
        }
        if !records
            .iter()
            .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &dep_fqn))
        {
            anyhow::bail!("depends-on target `{dep}` is not an existing requirement");
        }
        records.push(Record::DependsOn {
            from: req_fqn.clone(),
            to: dep_fqn,
        });
    }
    Ok(())
}

/// `apg spec rm <project> <fqn|id>` — remove a node and its incident edges.
fn spec_rm(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(id)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg spec rm <project> <fqn|id>");
    };
    let apg_root = require_apg_root()?;
    let mut records = load_project(&apg_root, project)?;
    let fqn = if id.starts_with("future/") {
        id.clone()
    } else {
        format!("future/{project}/spec.{id}")
    };
    let before = records.len();
    remove_node(&mut records, &fqn);
    if records.len() == before {
        anyhow::bail!("nothing to remove: `{fqn}` not found in `{project}`");
    }
    write_through(&apg_root, project, &records)?;
    println!("Removed {fqn}");
    Ok(())
}

/// `apg spec render <project> [--out <path>|-]` (R9/R16). The render is a
/// projection of the graph; editing it is never a supported path.
fn spec_render(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg spec render <project> [--out <path>|-]");
    };
    let apg_root = require_apg_root()?;
    let records = load_project(&apg_root, project)?;
    let db = artifacts::ArtifactDb::open(&apg_root)?;
    let md = render_spec(&records, &db)?;
    match p.get("out").as_deref() {
        Some("-") => print!("{md}"),
        Some(path) => {
            std::fs::write(path, &md)?;
            println!("Rendered spec {project} to {path}");
        }
        None => {
            let out = apg_root.join(specs::TRANS).join("specs").join(format!("{project}.md"));
            std::fs::create_dir_all(out.parent().unwrap())?;
            std::fs::write(&out, &md)?;
            println!("Rendered spec {project} to {}", out.display());
        }
    }
    Ok(())
}

/// The section-mapped markdown render (R16).
fn render_spec(records: &[Record], db: &artifacts::ArtifactDb) -> anyhow::Result<String> {
    let project = records
        .iter()
        .find_map(|r| match r {
            Record::Spec { fqn, .. } => project_of(fqn),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("no spec node in project"))?;
    let fqn_of = |prefix: &str| format!("future/{project}/{prefix}");

    let spec = records
        .iter()
        .find_map(|r| match r {
            Record::Spec { fqn, title, goal } if fqn == &fqn_of("spec") => {
                Some((title.clone(), goal.clone()))
            }
            _ => None,
        })
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!("# {}\n", spec.0));
    out.push_str(&format!("## Goal\n{}\n\n", spec.1));

    // Requirements grouped by feature.
    let mut reqs: Vec<&Record> = records
        .iter()
        .filter(|r| matches!(r, Record::Requirement { .. }))
        .collect();
    reqs.sort_by_key(|r| match r {
        Record::Requirement { id, .. } => id.clone(),
        _ => String::new(),
    });
    let mut by_feature: Vec<(String, Vec<&Record>)> = Vec::new();
    for r in &reqs {
        let feature = match r {
            Record::Requirement { feature, .. } if !feature.is_empty() => feature.clone(),
            _ => "General".to_string(),
        };
        match by_feature.iter_mut().find(|(f, _)| f == &feature) {
            Some((_, v)) => v.push(r),
            None => by_feature.push((feature, vec![*r])),
        }
    }

    let mut feature_lines = String::new();
    for (feature, items) in &by_feature {
        feature_lines.push_str(&format!("### {feature}\n"));
        for r in items {
            let Record::Requirement { fqn, id, title, body, .. } = r else { unreachable!() };
            feature_lines.push_str(&format!("**{id} — {title}.** {body}\n"));
            let consumes: Vec<String> = records
                .iter()
                .filter_map(|e| match e {
                    Record::DependsOn { from, to } if from == fqn => {
                        Some(dep_id_of(to, &project))
                    }
                    _ => None,
                })
                .collect();
            if !consumes.is_empty() {
                feature_lines.push_str(&format!("Consumes: {}\n", consumes.join(", ")));
            }
            let anchors: Vec<String> = records
                .iter()
                .filter_map(|e| match e {
                    Record::Anchors { from, to } if from == fqn => Some(to.clone()),
                    _ => None,
                })
                .collect();
            if !anchors.is_empty() {
                for a in &anchors {
                    let loc = db_anchor_loc(db, a);
                    match loc {
                        Some((path, line)) => {
                            feature_lines.push_str(&format!("Anchors: `{a}` ({path}:{line})\n"))
                        }
                        None => feature_lines.push_str(&format!("Anchors: `{a}`\n")),
                    }
                }
            }
        }
    }
    out.push_str(&format!("## Scope\n\n## Requirements\n{feature_lines}\n"));

    // Notes by kind → sections.
    let notes = |kind: &str| {
        records
            .iter()
            .filter_map(|r| match r {
                Record::Note { body, kind: k, .. } if k == kind => Some(body.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let nongoals: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::NonGoal { body, .. } => Some(body.clone()),
            _ => None,
        })
        .collect();
    out.push_str(&format!(
        "## Non-Goals\n{}\n",
        nongoals.iter().map(|b| format!("- {b}\n")).collect::<String>()
    ));

    let design = notes("design");
    if !design.is_empty() {
        out.push_str(&format!(
            "## Design\n{}\n",
            design.iter().map(|b| format!("- {b}\n")).collect::<String>()
        ));
    }
    let err = notes("error-handling");
    if !err.is_empty() {
        out.push_str(&format!(
            "## Error Handling\n{}\n",
            err.iter().map(|b| format!("- {b}\n")).collect::<String>()
        ));
    }

    let vis: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::VerificationItem { body, .. } => Some(body.clone()),
            _ => None,
        })
        .collect();
    out.push_str(&format!(
        "## Verification\n{}\n",
        vis.iter().map(|b| format!("- {b}\n")).collect::<String>()
    ));

    let acs: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::AcceptanceCriterion { body, .. } => Some(body.clone()),
            _ => None,
        })
        .collect();
    out.push_str(&format!(
        "## Acceptance Criteria\n{}\n",
        acs.iter().map(|b| format!("- [ ] {b}\n")).collect::<String>()
    ));

    let oq = notes("open-question");
    if !oq.is_empty() {
        out.push_str(&format!(
            "## Open Questions\n{}\n",
            oq.iter().map(|b| format!("- {b}\n")).collect::<String>()
        ));
    }

    let mut phases: Vec<u32> = records
        .iter()
        .filter_map(|r| match r {
            Record::Phase { number, .. } => Some(*number),
            _ => None,
        })
        .collect();
    phases.sort_unstable();
    if !phases.is_empty() {
        out.push_str("## Phases\n");
        for n in phases {
            let pfqn = fqn_of(&format!("spec.phase-{n}"));
            let gates: Vec<String> = records
                .iter()
                .filter_map(|e| match e {
                    Record::Gates { from, to } if from == &pfqn => Some(to.clone()),
                    _ => None,
                })
                .collect();
            let reqs_in: Vec<String> = records
                .iter()
                .filter_map(|e| match e {
                    Record::Contains { from, to }
                        if from == &pfqn
                            && matches!(to, t if t.starts_with(&format!("future/{project}/spec."))) =>
                    {
                        Some(dep_id_of(to, &project))
                    }
                    _ => None,
                })
                .collect();
            out.push_str(&format!("**Phase {n}.**{}", {
                let mut s = String::new();
                if !gates.is_empty() {
                    s.push_str(&format!(" gated on: {}", gates.join(", ")));
                }
                if !reqs_in.is_empty() {
                    s.push_str(&format!(" requirements: {}", reqs_in.join(", ")));
                }
                s
            }));
            out.push('\n');
        }
        out.push('\n');
    }

    let decisions: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Decision { id, summary, .. } => {
                Some(format!("**Decision `{id}`.** {summary}"))
            }
            _ => None,
        })
        .collect();
    if !decisions.is_empty() {
        out.push_str(&format!(
            "## Decisions\n{}\n",
            decisions.iter().map(|d| format!("- {d}\n")).collect::<String>()
        ));
    }

    let misc = notes("comment")
        .into_iter()
        .chain(notes("misc"))
        .chain(notes("decision"))
        .collect::<Vec<_>>();
    if !misc.is_empty() {
        out.push_str(&format!(
            "## Notes / Comments\n{}\n",
            misc.iter().map(|b| format!("- {b}\n")).collect::<String>()
        ));
    }
    Ok(out)
}

/// The requirement id (`R1`) of a requirement fqn.
fn dep_id_of(fqn: &str, project: &str) -> String {
    let prefix = format!("future/{project}/spec.");
    fqn.strip_prefix(&prefix).unwrap_or(fqn).to_string()
}

/// Resolves an anchor target's `path:start_line` from the live graph.
fn db_anchor_loc(db: &artifacts::ArtifactDb, fqn: &str) -> Option<(String, String)> {
    let q = format!(
        "MATCH (n {{fqn: {}}}) RETURN n.path, n.start_line",
        artifacts::lit(fqn)
    );
    let s = db.q(&q).ok()?;
    let mut lines = s.lines();
    lines.next()?;
    let row = lines.next()?.trim();
    let mut parts = row.split('|');
    let path = parts.next()?.to_string();
    let line = parts.next()?.to_string();
    // A Future (pending anchor) or module carries no path — no loc to show.
    if path.is_empty() {
        return None;
    }
    Some((path, line))
}

/// `apg spec promote <project> <future-name>|--all` (R17). For each
/// `Anchors(req→Future f)` whose `f.target` resolves in the code graph:
/// re-point the anchor to the real node, add `Implements(f.target→req)`, and
/// retire the Future (removed from the project JSONL, write-through).
fn spec_promote(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg spec promote <project> <future-name>|--all");
    };
    let all = p.has("all");
    let target: Option<&str> = if all {
        None
    } else {
        p.positional.get(1).map(|s| s.as_str())
    };
    if !all && target.is_none() {
        anyhow::bail!("usage: apg spec promote <project> <future-name>|--all");
    }

    let apg_root = require_apg_root()?;
    let records = load_project(&apg_root, project)?;

    // The futures to consider: all in the project, or the named one.
    let futures: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Future { fqn, .. }
                if all || fqn == &format!("future/{project}/{}", target.unwrap()) =>
            {
                Some(fqn.clone())
            }
            _ => None,
        })
        .collect();
    if futures.is_empty() {
        anyhow::bail!(
            "no future{} to promote in `{project}`",
            if all { "s" } else { " named as given" }
        );
    }
    let mut promoted = 0;
    for fqn in &futures {
        promote_future(&apg_root, project, fqn)?;
        promoted += 1;
    }
    println!("Promoted {promoted} future(s) in {project} → present");
    Ok(())
}

/// Promotes a single future (R17), shared by `apg spec promote` and `apg plan
/// done`. Re-points every `Anchors(req→f)` to `f.target`, adds
/// `Implements(f.target→req)`, and retires `f` from the project JSONL
/// (write-through). Errors if `f.target` does not resolve in the code graph.
pub fn promote_future(apg_root: &Path, project: &str, future_fqn: &str) -> anyhow::Result<()> {
    let mut records = load_project(apg_root, project)?;
    let target = {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        let Some(target) = records.iter().find_map(|r| match r {
            Record::Future { fqn, target, .. } if fqn == future_fqn => Some(target.clone()),
            _ => None,
        }) else {
            anyhow::bail!("future `{future_fqn}` not found in `{project}`");
        };
        if target.is_empty() {
            anyhow::bail!(
                "future `{future_fqn}` has no target — declare it with `apg spec add future`"
            );
        }
        if !db.has_node(&target) {
            anyhow::bail!(
                "future `{future_fqn}` target `{target}` does not resolve in the code graph — run `apg scan` and retry (a stale graph or target mismatch is never guessed)"
            );
        }
        target
    };
    let anchors: Vec<String> = records
        .iter()
        .filter_map(|e| match e {
            Record::Anchors { from, to } if to == future_fqn => Some(from.clone()),
            _ => None,
        })
        .collect();
    for req in anchors {
        artifacts::remove_incident_edges(&mut records, &req);
        records.push(Record::Anchors {
            from: req.clone(),
            to: target.clone(),
        });
        records.push(Record::Implements {
            from: target.clone(),
            to: req,
        });
    }
    // Retire the future: drop its record and its incident edges.
    remove_node(&mut records, future_fqn);
    write_through(apg_root, project, &records)?;
    Ok(())
}

/// `apg spec archive <project>` (R19). Refuses while any Feedback on the
/// spec's nodes is unresolved; then stops the JSONL from being discovered by
/// moving it to `apg/archived/` (retained as the historical record, still
/// committed, never re-scanned).
fn spec_archive(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(project) = p.positional.first() else {
        anyhow::bail!("usage: apg spec archive <project>");
    };
    let apg_root = require_apg_root()?;
    let records = load_project(&apg_root, project)?;

    let unresolved = records.iter().any(|r| {
        matches!(
            r,
            Record::Feedback { status, .. } if status != "resolved"
        )
    });
    if unresolved {
        anyhow::bail!(
            "spec `{project}` has unresolved review feedback — resolve every `Feedback` before archiving (R27)"
        );
    }

    let src = specs::spec_jsonl_path(&apg_root, project);
    let dst = apg_root.join("archived").join(format!("{project}.jsonl"));
    if dst.exists() {
        anyhow::bail!("archived spec already exists at {}", dst.display());
    }
    std::fs::create_dir_all(dst.parent().unwrap())?;
    std::fs::rename(&src, &dst)?;
    // The project's plan (if any) is transient working state; archive it too.
    let plan_path = specs::plan_jsonl_path(&apg_root, project);
    if plan_path.exists() {
        std::fs::remove_file(&plan_path)?;
    }
    println!(
        "Archived spec {project} -> {} (no longer discovered by `apg scan`; Implements edges keep delivered work traceable)",
        dst.display()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::load;
    use lbug::{Connection, Database};

    /// A temp `apg/` layout with a real `apg/.trans/db.lbug` holding a code
    /// graph (`github.com/x/y.Store` struct + file + module), plus an empty
    /// `apg/specs/`. Returns (apg_root, temp_dir).
    fn fixture_layout(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "apg-cli-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        std::fs::create_dir_all(dir.join("apg").join("specs")).unwrap();

        // Code graph → db.lbug.
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
        g.contains
            .insert(("/abs/store.go".to_string(), "github.com/x/y.Store".to_string()));

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

    #[test]
    fn link_depends_on_adds_all_edges_once() {
        let mut records = vec![
            Record::Requirement {
                fqn: "future/foo/spec.R1".into(),
                id: "R1".into(),
                title: "A".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "future/foo/spec.R2".into(),
                id: "R2".into(),
                title: "B".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "future/foo/spec.R3".into(),
                id: "R3".into(),
                title: "C".into(),
                body: String::new(),
                feature: String::new(),
            },
        ];
        // Multiple --depends-on in one call must all land (regression: the
        // incident-edge removal used to run inside the loop, dropping earlier
        // edges so only the last dep survived).
        link_depends_on(
            "foo",
            "R1",
            &["R2".into(), "R3".into()],
            &mut records,
        )
        .unwrap();
        let deps: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::DependsOn { from, to }
                    if from == "future/foo/spec.R1" =>
                {
                    Some(to.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(deps, vec!["future/foo/spec.R2", "future/foo/spec.R3"]);

        // Re-linking replaces, never duplicates.
        link_depends_on("foo", "R1", &["R3".into()], &mut records).unwrap();
        let deps: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::DependsOn { from, to }
                    if from == "future/foo/spec.R1" =>
                {
                    Some(to.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(deps, vec!["future/foo/spec.R3"]);

        // Self-deps and undeclared targets are write-time errors.
        assert!(link_depends_on("foo", "R1", &["R1".into()], &mut records).is_err());
        assert!(link_depends_on("foo", "R1", &["R9".into()], &mut records).is_err());
    }

    #[test]
    fn write_through_and_promote_roundtrip() {
        let (apg_root, dir) = fixture_layout("roundtrip");

        // Author a spec with a requirement anchored to a not-yet-built future.
        let recs = vec![
            Record::Spec {
                fqn: "future/foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: "G".to_string(),
            },
            Record::Requirement {
                fqn: "future/foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Timer".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "future/foo/spec".to_string(),
                to: "future/foo/spec.R1".to_string(),
            },
            Record::Future {
                fqn: "future/foo/gateway".to_string(),
                kind: "rpc".to_string(),
                target: "github.com/x/y.Store".to_string(),
            },
            Record::Anchors {
                from: "future/foo/spec.R1".to_string(),
                to: "future/foo/gateway".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        reingest_project(&apg_root, "foo").unwrap();

        // The pending anchor materializes in the live DB.
        {
            let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
            assert!(db.is_future("future/foo/gateway"));
            assert!(db.has_node("github.com/x/y.Store"));
        } // drop the handle before promote reopens the file DB

        // Promote: re-anchor to the code node, add Implements, retire the
        // future — in the JSONL and the DB.
        promote_future(&apg_root, "foo", "future/foo/gateway").unwrap();
        let records = load_project(&apg_root, "foo").unwrap();
        assert!(records.iter().all(|r| !matches!(r, Record::Future { .. })));
        assert!(records.iter().any(|r| {
            matches!(r, Record::Anchors { from, to }
                if from == "future/foo/spec.R1" && to == "github.com/x/y.Store")
        }));
        assert!(records.iter().any(|r| {
            matches!(r, Record::Implements { from, to }
                if from == "github.com/x/y.Store" && to == "future/foo/spec.R1")
        }));

        // The DB reflects the transition after the write-through re-ingest.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(!db.is_future("future/foo/gateway"));
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (s:Struct)-[:Implements]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec.R1"), "implements: {out}");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Requirement)-[:Anchors]->(s:Struct) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("github.com/x/y.Store"), "re-anchor: {out}");

        // A rebuild from the committed JSONL reproduces the same state.
        reingest_project(&apg_root, "foo").unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Struct)-[:Implements]->(r:Requirement) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "rebuild keeps Implements: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promote_unbuilt_future_errors() {
        let (apg_root, dir) = fixture_layout("unbuilt");
        let recs = vec![
            Record::Spec {
                fqn: "future/foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Future {
                fqn: "future/foo/gateway".to_string(),
                kind: "rpc".to_string(),
                target: "github.com/x/y.DoesNotExist".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        reingest_project(&apg_root, "foo").unwrap();
        // A stale target is never guessed: promote errors.
        let err = promote_future(&apg_root, "foo", "future/foo/gateway")
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not resolve"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_node_strips_incident_edges_only() {
        let mut records = vec![
            Record::Requirement {
                fqn: "future/foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "t".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Anchors {
                from: "future/foo/spec.R1".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
            Record::Anchors {
                from: "future/foo/spec.R2".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        artifacts::remove_incident_edges(&mut records, "future/foo/spec.R1");
        // R1's anchor is gone; R2's (same code target) survives; R1 node stays.
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|r| matches!(r, Record::Requirement { .. })));
        assert!(records.iter().any(|r| {
            matches!(r, Record::Anchors { from, .. } if from == "future/foo/spec.R2")
        }));
    }
}
