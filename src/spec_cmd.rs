//! `apg spec` — authoring, lifecycle, and rendering of graph-native specs
//! (SPEC R6-R9, R17-R19). Every mutation is write-through (R5): load the
//! project JSONL, apply the change, write it back, re-ingest into the live DB.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::artifacts::{self, ParsedArgs, node_fqn, parse_args, remove_node};
use crate::graph::NodeKind;
use crate::load;
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
        anyhow::bail!("no spec for project `{project}` — run `apg spec init {project}` first");
    }
    specs::read_jsonl(&path)
}

/// Writes a project's spec JSONL and re-ingests it into the live DB (R5). A
/// missing DB (no scan yet) is not an error — the JSONL is the durable form.
pub(crate) fn write_through(
    apg_root: &Path,
    project: &str,
    records: &[Record],
) -> anyhow::Result<()> {
    artifacts::write_jsonl_and_reingest(
        apg_root,
        &specs::spec_jsonl_path(apg_root, project),
        project,
        records,
    )
}

/// The project of a project-scoped spec/plan/review fqn (`<project>/spec.R1`,
/// `<project>/plan.phase-01`, `<project>/feedback-1`). Callers must already
/// know the fqn is spec-family (a `Record::Spec`, a feedback fqn, a
/// requirement anchor) — for arbitrary fqns use the DB to discriminate code
/// nodes first (see `review_add`).
pub fn project_of(fqn: &str) -> Option<String> {
    fqn.split('/').next().map(|s| s.to_string())
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
        anyhow::bail!("usage: apg spec <init|add|anchor|link|spine|rm|render|unresolved> …");
    };
    match sub {
        "init" => spec_init(&args[1..]),
        "add" => spec_add(&args[1..]),
        "anchor" => spec_anchor(&args[1..]),
        "link" => spec_link(&args[1..]),
        "spine" => spec_spine(&args[1..]),
        "rm" => spec_rm(&args[1..]),
        "render" => spec_render(&args[1..]),
        "unresolved" => spec_unresolved(&args[1..]),
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
    artifacts::acquire_spec_lock(&apg_root)?;
    let path = specs::spec_jsonl_path(&apg_root, project);
    if path.exists() {
        anyhow::bail!(
            "spec for project `{project}` already exists at {}",
            path.display()
        );
    }
    let title = p.get("title").unwrap_or_else(|| project.clone());
    let goal = p.get("goal").unwrap_or_default();
    let records = vec![Record::Spec {
        fqn: format!("{project}/spec"),
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
        anyhow::bail!(
            "usage: apg spec add <project> <requirement|phase|decision|non-goal|acceptance-criterion|verification|note|stakeholder|domain|subdomain|entity|value-object|aggregate|domain-event|domain-process|domain-rule|actor|system|container|component> …"
        );
    };
    let Some(kind) = p.positional.get(1).map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg spec add <project> <kind> …");
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_project(&apg_root, project)?;

    let spec_fqn = format!("{project}/spec");
    match kind {
        "requirement" => add_requirement(&p, &apg_root, project, &mut records)?,
        "phase" => {
            let Some(n) = p.positional.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                anyhow::bail!(
                    "usage: apg spec add <project> phase <n> --title … [--gate <phase-n>]*"
                );
            };
            let Some(title) = p.get("title") else {
                anyhow::bail!("phase requires --title");
            };
            let fqn = format!("{project}/spec.phase-{n}");
            // Upsert semantics: drop the phase and its old incident edges BEFORE
            // validating new gates, so a retired edge can't resurrect as a false
            // cycle (an incoming gate severed by the re-add would otherwise trip
            // `cycle_closing_path`).
            remove_node(&mut records, &fqn);
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
                let gate_n = g
                    .parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("bad phase number `{g}`"))?;
                recs.push(phase_gate_edge(project, n, &fqn, gate_n, &records, &recs)?);
            }
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
            let fqn = format!("{project}/spec.decision-{id}");
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
            let body = p
                .get("body")
                .ok_or_else(|| anyhow::anyhow!("non-goal requires --body"))?;
            let n = next_spec_counter(&records, "ng");
            let fqn = format!("{project}/spec.ng-{n}");
            records.push(Record::NonGoal {
                fqn: fqn.clone(),
                body,
            });
            records.push(Record::Contains {
                from: spec_fqn.clone(),
                to: fqn,
            });
            write_through(&apg_root, project, &records)?;
            println!("Added non-goal to {project}");
        }
        "acceptance-criterion" => {
            let body = p
                .get("body")
                .ok_or_else(|| anyhow::anyhow!("acceptance-criterion requires --body"))?;
            let n = next_spec_counter(&records, "ac");
            let fqn = format!("{project}/spec.ac-{n}");
            records.push(Record::AcceptanceCriterion {
                fqn: fqn.clone(),
                body,
            });
            records.push(Record::Contains {
                from: spec_fqn.clone(),
                to: fqn,
            });
            write_through(&apg_root, project, &records)?;
            println!("Added acceptance criterion to {project}");
        }
        "verification" => {
            let body = p
                .get("body")
                .ok_or_else(|| anyhow::anyhow!("verification requires --body"))?;
            let n = next_spec_counter(&records, "vi");
            let fqn = format!("{project}/spec.vi-{n}");
            records.push(Record::VerificationItem {
                fqn: fqn.clone(),
                body,
            });
            records.push(Record::Contains {
                from: spec_fqn.clone(),
                to: fqn,
            });
            write_through(&apg_root, project, &records)?;
            println!("Added verification item to {project}");
        }
        "note" => add_note(&p, &apg_root, project, &mut records)?,
        // Tier-1/2/3 nodes (GraphModel-SPEC.md; PHASE_01). Each is a
        // project-scoped spec node with a short `name` (+ optional `--body`);
        // the FQN is `<project>/<slug>.<name>`. `--parent <fqn>` sets
        // the containing node (default: the spec root), so the DDD/C4
        // hierarchy (`Domain ⊃ Subdomain ⊃ Aggregate ⊃ Entity/ValueObject`,
        // `System ⊃ Container ⊃ Component`) is authorable.
        "stakeholder" => {
            add_tier_node(&p, project, "stakeholder", &spec_fqn, &mut records, |fqn, name, body| Record::Stakeholder { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added stakeholder to {project}");
        }
        "domain" | "bounded-context" => {
            add_tier_node(&p, project, "domain", &spec_fqn, &mut records, |fqn, name, body| Record::Domain { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added domain to {project}");
        }
        "subdomain" => {
            add_tier_node(&p, project, "subdomain", &spec_fqn, &mut records, |fqn, name, body| Record::Subdomain { fqn, name, kind: p.get("kind").unwrap_or_default(), body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added subdomain to {project}");
        }
        "entity" => {
            add_tier_node(&p, project, "entity", &spec_fqn, &mut records, |fqn, name, body| Record::Entity { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added entity to {project}");
        }
        "value-object" => {
            add_tier_node(&p, project, "value-object", &spec_fqn, &mut records, |fqn, name, body| Record::ValueObject { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added value object to {project}");
        }
        "aggregate" => {
            add_tier_node(&p, project, "aggregate", &spec_fqn, &mut records, |fqn, name, body| Record::Aggregate { fqn, name, root: p.get("root").unwrap_or_default(), body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added aggregate to {project}");
        }
        "domain-event" => {
            add_tier_node(&p, project, "domain-event", &spec_fqn, &mut records, |fqn, name, body| Record::DomainEvent { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added domain event to {project}");
        }
        "domain-process" => {
            add_tier_node(&p, project, "domain-process", &spec_fqn, &mut records, |fqn, name, body| Record::DomainProcess { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added domain process to {project}");
        }
        "domain-rule" => {
            let name = p.positional.get(2).ok_or_else(|| {
                anyhow::anyhow!("usage: apg spec add <project> domain-rule <name> --body …")
            })?;
            let body = p
                .get("body")
                .ok_or_else(|| anyhow::anyhow!("domain-rule requires --body"))?;
            add_tier_node(
                &p,
                project,
                "domain-rule",
                &spec_fqn,
                &mut records,
                |fqn, name, body| Record::DomainRule { fqn, name, body },
            )?;
            // DomainRule alignment (Invariants-SPEC.md / PHASE_02): a domain
            // rule IS the invariant mechanism at the domain tier — materialize
            // it as a project-scoped Invariant with category=product alongside
            // the DomainRule node, so the rule is guardable and citable.
            let ifqn = format!("{project}/invariant/{name}");
            records.retain(|r| !matches!(r, Record::Invariant { fqn, .. } if fqn == &ifqn));
            records.push(Record::Invariant {
                fqn: ifqn,
                title: name.clone(),
                body: body.clone(),
                category: "product".to_string(),
                scope: "code".to_string(),
                status: "active".to_string(),
            });
            write_through(&apg_root, project, &records)?;
            println!("Added domain rule `{name}` to {project}");
        }
        "actor" => {
            add_tier_node(&p, project, "actor", &spec_fqn, &mut records, |fqn, name, body| Record::Actor { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added actor to {project}");
        }
        "system" => {
            add_tier_node(&p, project, "system", &spec_fqn, &mut records, |fqn, name, body| Record::System { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added system to {project}");
        }
        "container" => {
            add_tier_node(&p, project, "container", &spec_fqn, &mut records, |fqn, name, body| Record::Container { fqn, name, kind: p.get("kind").unwrap_or_default(), body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added container to {project}");
        }
        "component" => {
            add_tier_node(&p, project, "component", &spec_fqn, &mut records, |fqn, name, body| Record::Component { fqn, name, body })?;
            write_through(&apg_root, project, &records)?;
            println!("Added component to {project}");
        }
        other => anyhow::bail!(
            "unknown spec add kind `{other}` — requirement|phase|decision|non-goal|acceptance-criterion|verification|note|stakeholder|domain|subdomain|entity|value-object|aggregate|domain-event|domain-process|domain-rule|actor|system|container|component"
        ),
    }
    Ok(())
}

/// `apg spec add <project> requirement <id> [--title …] [--body …]
/// [--feature …] [--depends-on <id>]* [--anchor <fqn>]*` (R7). The DB is
/// opened only when `--anchor` is present (anchors resolve against the
/// scanned code graph); a zero-anchor add is DB-less, like the other add
/// kinds, so a fresh project with no scan yet adds requirements fine —
/// `write_jsonl_and_reingest` skips the DB when db.lbug is absent
/// (SpecCreation-SPEC branch/write-through flow). `--anchor` still requires
/// a scan: it is rejected up front, never silently dropped.
fn add_requirement(
    p: &ParsedArgs,
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let Some(id) = p.positional.get(2) else {
        anyhow::bail!(
            "usage: apg spec add <project> requirement <id> [--title …] [--body …] [--feature …] [--depends-on <id>]* [--anchor <fqn>]*"
        );
    };
    let spec_fqn = format!("{project}/spec");
    let fqn = format!("{project}/spec.{id}");
    let mut recs = vec![Record::Requirement {
        fqn: fqn.clone(),
        id: id.clone(),
        title: p.get("title").unwrap_or_default(),
        body: p.get("body").unwrap_or_default(),
        feature: p.get("feature").unwrap_or_default(),
    }];
    recs.push(Record::Contains {
        from: spec_fqn,
        to: fqn.clone(),
    });
    for dep in p.all("depends-on") {
        let (dep_proj, dep_id) = dep_target(dep.as_str(), project, apg_root);
        if dep_proj == project && dep_id == id.as_str() {
            anyhow::bail!("requirement `{id}` cannot depend on itself");
        }
        let dep_fqn = format!("{dep_proj}/spec.{dep_id}");
        let exists = if dep_proj == project {
            records
                .iter()
                .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &dep_fqn))
        } else {
            requirement_exists(apg_root, &dep_proj, &dep_id)?
        };
        if !exists {
            anyhow::bail!(
                "depends-on target `{dep}` is not an existing requirement in `{dep_proj}`"
            );
        }
        recs.push(Record::DependsOn {
            from: fqn.clone(),
            to: dep_fqn,
        });
    }
    let anchors = p.all("anchor");
    if !anchors.is_empty() {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        for a in anchors {
            db.resolve_anchor(&a)?;
            recs.push(Record::Anchors {
                from: fqn.clone(),
                to: a,
            });
        }
    }
    remove_node(records, &fqn);
    records.extend(recs);
    write_through(apg_root, project, records)?;
    println!("Added requirement {id} to {project}");
    Ok(())
}

/// One phase `--gate` edge, validated before it lands (SpecCreation-SPEC §2:
/// "acyclic … DependsOn/Gates"). Rejects a self-gate and any *transitive*
/// Gates cycle — unlike the old self-gate-only check — by asking
/// `cycle_closing_path` whether the edge `phase-N → phase-G` closes a path
/// `phase-G → … → phase-N` already in the records that will be on disk
/// (`records` with `fqn`'s incident edges already stripped, plus `recs`, the
/// new phase's Phase/Contains/prior-gate records). Mirrors the DependsOn/
/// SpecDependsOn checks (spec_cmd.rs) and `apg plan link`'s Gates check.
fn phase_gate_edge(
    project: &str,
    n: u32,
    fqn: &str,
    gate_n: u32,
    records: &[Record],
    recs: &[Record],
) -> anyhow::Result<Record> {
    if gate_n == n {
        anyhow::bail!("a phase cannot gate on itself");
    }
    let target = format!("{project}/spec.phase-{gate_n}");
    let mut combined = records.to_vec();
    combined.extend(recs.iter().cloned());
    if let Some(path) = artifacts::cycle_closing_path(&combined, fqn, &target, |r| match r {
        Record::Gates { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    }) {
        let short: Vec<String> = path
            .iter()
            .map(|f| {
                f.strip_prefix(&format!("{project}/spec."))
                    .unwrap_or(f)
                    .to_string()
            })
            .collect();
        anyhow::bail!(
            "adding gate phase-{n} → phase-{gate_n} would create a cycle: {}",
            short.join(" → ")
        );
    }
    Ok(Record::Gates {
        from: fqn.to_string(),
        to: target,
    })
}

/// Adds a tier-1/2/3 node (`stakeholder`/`domain`/…), upserting by FQN. The
/// node hangs under `--parent <fqn>` (default: the spec root) via a Contains
/// edge. `build` turns `(fqn, name, body)` into the node record.
///
/// The `--parent` kind is validated against the `Contains` rel-table hierarchy
/// (`load::contains_pair_allowed`, GraphModel-SPEC.md) BEFORE any record is
/// pushed or any write happens: a pair outside the schema — a direct
/// `Domain ⊃ Aggregate`, a Subdomain/Entity/… hanging under the Spec root, a
/// parent that is not a spec node — is rejected up front instead of passing
/// CLI validation and silently vanishing at re-ingest.
fn add_tier_node(
    p: &ParsedArgs,
    project: &str,
    slug: &str,
    spec_fqn: &str,
    records: &mut Vec<Record>,
    build: impl Fn(String, String, String) -> Record,
) -> anyhow::Result<()> {
    let Some(name) = p.positional.get(2) else {
        anyhow::bail!(
            "usage: apg spec add <project> {slug} <name> [--body …] [--parent <fqn>]{}",
            if slug == "subdomain" || slug == "container" {
                " [--kind …]"
            } else if slug == "aggregate" {
                " [--root …]"
            } else {
                ""
            }
        );
    };
    let body = p.get("body").unwrap_or_default();
    let fqn = format!("{project}/{slug}.{name}");
    let parent = p.get("parent").unwrap_or_else(|| spec_fqn.to_string());
    if parent == fqn {
        anyhow::bail!("a {slug} node cannot contain itself");
    }
    let rec = build(fqn.clone(), name.clone(), body);
    let child_kind = spec_node_kind(&rec)
        .ok_or_else(|| anyhow::anyhow!("internal: tier node record for `{fqn}` has no kind"))?;
    let parent_kind = records.iter().find_map(|r| {
        (node_fqn(r) == Some(parent.as_str()))
            .then(|| spec_node_kind(r))
            .flatten()
    });
    let Some(parent_kind) = parent_kind else {
        anyhow::bail!(
            "tier node parent `{parent}` is not a spec node in `{project}` — pass --parent a spec-family FQN ({spec_fqn} for the spec root, or the containing Domain/Subdomain/Aggregate/System/Container)"
        );
    };
    if !load::contains_pair_allowed(parent_kind, child_kind) {
        anyhow::bail!(
            "a `{slug}` node (`{fqn}`) cannot hang under a `{}` node (`{parent}`) — the Contains hierarchy is `Domain ⊃ Subdomain ⊃ Aggregate ⊃ Entity/ValueObject`, `Domain ⊃ DomainEvent/DomainProcess/DomainRule/Actor`, `System ⊃ Container ⊃ Component`; pass --parent the right parent",
            load::label_of(parent_kind)
        );
    }
    remove_node(records, &fqn);
    records.push(rec);
    records.push(Record::Contains {
        from: parent,
        to: fqn,
    });
    Ok(())
}

/// The graph kind of a spec-family node record (`Spec`, `Requirement`, tier
/// nodes, …) — used to validate a tier node's `--parent` kind against the
/// `Contains` hierarchy before any write. Edge/control records are not nodes.
fn spec_node_kind(r: &Record) -> Option<NodeKind> {
    use Record::*;
    match r {
        Spec { .. } => Some(NodeKind::Spec),
        Requirement { .. } => Some(NodeKind::Requirement),
        Phase { .. } => Some(NodeKind::Phase),
        Decision { .. } => Some(NodeKind::Decision),
        NonGoal { .. } => Some(NodeKind::NonGoal),
        AcceptanceCriterion { .. } => Some(NodeKind::AcceptanceCriterion),
        VerificationItem { .. } => Some(NodeKind::VerificationItem),
        Note { .. } => Some(NodeKind::Note),
        Feedback { .. } => Some(NodeKind::Feedback),
        Plan { .. } => Some(NodeKind::Plan),
        PlanPhase { .. } => Some(NodeKind::PlanPhase),
        Task { .. } => Some(NodeKind::Task),
        Stakeholder { .. } => Some(NodeKind::Stakeholder),
        Domain { .. } => Some(NodeKind::Domain),
        Subdomain { .. } => Some(NodeKind::Subdomain),
        Entity { .. } => Some(NodeKind::Entity),
        ValueObject { .. } => Some(NodeKind::ValueObject),
        Aggregate { .. } => Some(NodeKind::Aggregate),
        DomainEvent { .. } => Some(NodeKind::DomainEvent),
        DomainProcess { .. } => Some(NodeKind::DomainProcess),
        DomainRule { .. } => Some(NodeKind::DomainRule),
        Actor { .. } => Some(NodeKind::Actor),
        System { .. } => Some(NodeKind::System),
        Container { .. } => Some(NodeKind::Container),
        Component { .. } => Some(NodeKind::Component),
        Invariant { .. } => Some(NodeKind::Invariant),
        Module { .. } => Some(NodeKind::Module),
        File { .. } => Some(NodeKind::File),
        Struct { .. } => Some(NodeKind::Struct),
        Function { .. } => Some(NodeKind::Function),
        Unresolved { .. } => Some(NodeKind::UnresolvedTarget),
        // Edge and pipeline-control records are not nodes.
        _ => None,
    }
}

/// `apg spec add <project> note --body … [--kind …] [--on <fqn>]*` (R7).
/// A `--on` target that is a code FQN routes the note to the committed
/// `apg/notes/<module>.jsonl` ledger; a spec FQN (or no target) to the
/// project's spec JSONL.
///
/// R2: every `--on` target is validated against the DB's Details rel-table
/// allow-list (`load::details_target_labels`, mirroring `spec_rel_pairs`)
/// BEFORE any record is pushed or any write happens. A Note/Feedback
/// target (or any label outside the allow-list) is rejected with a clear CLI
/// message instead of reaching the re-ingest, where LadybugDB would throw an
/// opaque binder exception for the undeclared edge pair (R3).
fn add_note(
    p: &ParsedArgs,
    apg_root: &Path,
    project: &str,
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let Some(body) = p.get("body") else {
        anyhow::bail!("note requires --body");
    };
    let kind = p.get("kind").unwrap_or_else(|| "note".to_string());
    let ons = p.all("on");
    if ons.is_empty() {
        validate_note_kind(&kind, "project")?;
        let n = artifacts::next_free(records, "note");
        let fqn = format!("{project}/note-{n}");
        records.push(Record::Note {
            fqn: fqn.clone(),
            body: body.clone(),
            kind: kind.to_string(),
        });
        write_through(apg_root, project, records)?;
        println!("Added project note to {project}");
        return Ok(());
    }
    let db = artifacts::ArtifactDb::open(apg_root)?;
    // R2: validate every `--on` target BEFORE any record is pushed or any
    // write happens (the loop below only mutates the in-memory `records`; the
    // JSONL + live DB are untouched until `write_through`). A target whose
    // node label is not an allowable Details target — Note, Feedback,
    // or anything outside the DB's Details rel-table pairs — is rejected here
    // with a clear CLI message instead of an opaque LadybugDB binder exception
    // from the re-ingest.
    for target in &ons {
        let Some(label) = db.node_label(target) else {
            anyhow::bail!("note target `{target}` does not exist in the graph");
        };
        if !load::details_target_labels().contains(&label) {
            anyhow::bail!(
                "note target `{target}` is a `{label}` node — a note may only attach to an allowable Details target ({})",
                load::details_target_labels().join(", ")
            );
        }
    }
    for target in &ons {
        let category = if db.code_label(target).is_some() {
            "code"
        } else {
            "spec"
        };
        validate_note_kind(&kind, category)?;
        // A code FQN routes to the per-module note ledger; a spec FQN
        // (anything not in the code graph) to the project's spec JSONL.
        if category == "code" {
            let file = db.note_file(apg_root, target);
            let mut ledger = if file.exists() {
                specs::read_jsonl(&file)?
            } else {
                Vec::new()
            };
            // Per-module ledger namespace: note fqns are
            // `annotations/<ledger-stem>/<n>`, so two modules' notes never
            // collide when the ledgers merge into one graph.
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("_root")
                .to_string();
            let n = artifacts::next_free_annotation(&ledger, &stem);
            let fqn = format!("annotations/{stem}/{n}");
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
            let fqn = format!("{project}/note-{n}");
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

/// Whether `kind` may be attached to a node of `category` ("project" = no
/// target, "spec" = a `…` graph node, "code" = Struct/Function/File).
/// A closed vocabulary so `WHERE n.kind = '…'` queries cannot silently miss
/// records written with a drifted/typo'd kind.
fn note_kind_allows(kind: &str, category: &str) -> bool {
    matches!(
        (kind, category),
        // Generic annotation: attach anywhere.
        ("note", "project" | "spec" | "code")
            // Spec-context notes.
            | ("background", "project" | "spec")
            | ("error-handling", "project" | "spec")
            | ("relationship-to-other-specs", "project" | "spec")
            | ("open-question", "project" | "spec")
            | ("materialization-fix", "project" | "spec")
            // Universal: rationale applies to either context.
            | ("design", "project" | "spec" | "code")
            | ("decision", "project" | "spec" | "code")
            | ("rationale", "spec" | "code")
            // Code-context notes.
            | ("warning", "code")
            | ("gotcha", "code")
    )
}

const NOTE_KIND_HELP: &str = "known kinds: note (any target), background (spec), \
    design (any), decision (any), error-handling (spec), \
    relationship-to-other-specs (spec), open-question (spec), \
    materialization-fix (spec), rationale (spec or code), warning (code), \
    gotcha (code)";

fn validate_note_kind(kind: &str, category: &str) -> anyhow::Result<()> {
    if !note_kind_allows(kind, category) {
        anyhow::bail!("invalid note kind `{kind}` for a {category} note — {NOTE_KIND_HELP}");
    }
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
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_project(&apg_root, project)?;
    let req_fqn = format!("{project}/spec.{req_id}");
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
    anchor_upsert(&req_fqn, fqn, &mut records);
    write_through(&apg_root, project, &records)?;
    println!("Anchored {project}.{req_id} → {fqn}");
    Ok(())
}

/// Adds the `Anchors(req→fqn)` edge, replacing only an existing edge on the
/// same `(from, to)` pair (idempotent upsert). Other anchors on `req` are
/// preserved — sequential anchor calls accumulate, they never clobber.
fn anchor_upsert(req_fqn: &str, fqn: &str, records: &mut Vec<Record>) {
    records.retain(|r| !matches!(r, Record::Anchors { from, to } if from == req_fqn && to == fqn));
    records.push(Record::Anchors {
        from: req_fqn.to_string(),
        to: fqn.to_string(),
    });
}

/// `apg spec link <project> <req-id|spec> [--depends-on <id|proj/id>]*` (R8).
/// Requirement targets are same-project ids (`R4`) or cross-project
/// `<project>/<id>`; `spec` links the spec node itself to other specs
/// (`--depends-on <project>`, a SpecDependsOn antecedent).
fn spec_link(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(req_id)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg spec link <project> <req-id|spec> [--depends-on <id|proj/id>]*");
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_project(&apg_root, project)?;
    if req_id == "spec" {
        link_spec_depends(&apg_root, project, &p.all("depends-on"), &mut records)?;
    } else {
        link_depends_on(
            &apg_root,
            project,
            req_id,
            &p.all("depends-on"),
            &mut records,
        )?;
    }
    write_through(&apg_root, project, &records)?;
    println!("Linked {project}.{req_id} depends-on");
    Ok(())
}

/// The (project, id) pair a `--depends-on` target names. A bare `R4` is a
/// same-project requirement; `<project>/R4` is cross-project when the prefix
/// names a spec that exists (or the current project). The syntax is
/// unambiguous in practice: requirement ids are `/`-free.
fn dep_target(dep: &str, project: &str, apg_root: &Path) -> (String, String) {
    if let Some((proj, id)) = dep.split_once('/')
        && (proj == project || specs::spec_jsonl_path(apg_root, proj).exists())
    {
        return (proj.to_string(), id.to_string());
    }
    (project.to_string(), dep.to_string())
}

/// Whether `id` is a declared requirement of spec project `proj`.
fn requirement_exists(apg_root: &Path, proj: &str, id: &str) -> anyhow::Result<bool> {
    let path = specs::spec_jsonl_path(apg_root, proj);
    if !path.exists() {
        return Ok(false);
    }
    let fqn = format!("{proj}/spec.{id}");
    Ok(specs::read_jsonl(&path)?
        .iter()
        .any(|r| matches!(r, Record::Requirement { fqn: f, .. } if *f == fqn)))
}

/// Every spec project's records merged into one list — the cross-project
/// view used for cycle detection (a DependsOn/SpecDependsOn edge may route
/// through other projects' records back into this one).
fn all_spec_records(
    apg_root: &Path,
    project: &str,
    records: &[Record],
) -> anyhow::Result<Vec<Record>> {
    let mut all = records.to_vec();
    for f in specs::jsonl_files(&apg_root.join("specs")) {
        if f.file_stem().and_then(|s| s.to_str()) == Some(project) {
            continue;
        }
        all.extend(specs::read_jsonl(&f)?);
    }
    Ok(all)
}

/// Applies `SpecDependsOn(spec → spec)` edges for `project`: removes its
/// existing outgoing edges once, then adds every antecedent project. The
/// target must be a declared spec; self-deps and SpecDependsOn cycles (across
/// all spec projects) are write-time errors.
fn link_spec_depends(
    apg_root: &Path,
    project: &str,
    deps: &[String],
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let spec_fqn = format!("{project}/spec");
    records.retain(|r| !matches!(r, Record::SpecDepends { from, .. } if from == &spec_fqn));
    for dep in deps {
        if dep == project {
            anyhow::bail!("spec `{project}` cannot depend on itself");
        }
        if !specs::spec_jsonl_path(apg_root, dep).exists() {
            anyhow::bail!("spec `{dep}` does not exist — declare it with `apg spec init {dep}`");
        }
        let dep_fqn = format!("{dep}/spec");
        let all = all_spec_records(apg_root, project, records)?;
        if let Some(path) = artifacts::cycle_closing_path(&all, &spec_fqn, &dep_fqn, |r| match r {
            Record::SpecDepends { from, to } => Some((from.as_str(), to.as_str())),
            _ => None,
        }) {
            let short: Vec<String> = path.iter().map(|f| f.to_string()).collect();
            anyhow::bail!(
                "adding spec dependency {project} → {dep} would create a cycle: {}",
                short.join(" → ")
            );
        }
        records.push(Record::SpecDepends {
            from: spec_fqn.clone(),
            to: dep_fqn,
        });
    }
    Ok(())
}

/// Applies the depends-on edges for `req_id`: removes its existing edges once,
/// then adds every dep (R8). A dep may repeat across calls (idempotent upsert);
/// self-deps and undeclared targets are write-time errors. Targets may be
/// same-project ids (`R4`) or cross-project `<project>/<id>`; cycle detection
/// runs over every spec project's DependsOn edges.
fn link_depends_on(
    apg_root: &Path,
    project: &str,
    req_id: &str,
    deps: &[String],
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    let req_fqn = format!("{project}/spec.{req_id}");
    if !records
        .iter()
        .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &req_fqn))
    {
        anyhow::bail!("requirement `{req_id}` does not exist in `{project}`");
    }
    // Drop only this requirement's own outgoing DependsOn edges. `rm`'s
    // incident-edge removal must not be used here: it also deletes edges INTO
    // the requirement (other requirements depending on it), silently severing
    // the rest of the dependency graph when a requirement is re-linked.
    records.retain(|r| !matches!(r, Record::DependsOn { from, .. } if from.as_str() == req_fqn));
    for dep in deps {
        let (dep_proj, dep_id) = dep_target(dep, project, apg_root);
        if dep_proj == project && dep_id == req_id {
            anyhow::bail!("requirement `{req_id}` cannot depend on itself");
        }
        let dep_fqn = format!("{dep_proj}/spec.{dep_id}");
        // Same-project targets validate against the in-memory records (they are
        // the loaded file); cross-project targets against the target spec file.
        let exists = if dep_proj == project {
            records
                .iter()
                .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == &dep_fqn))
        } else {
            requirement_exists(apg_root, &dep_proj, &dep_id)?
        };
        if !exists {
            anyhow::bail!(
                "depends-on target `{dep}` is not an existing requirement (in `{dep_proj}`)"
            );
        }
        let all = all_spec_records(apg_root, project, records)?;
        if let Some(path) = artifacts::cycle_closing_path(&all, &req_fqn, &dep_fqn, |r| match r {
            Record::DependsOn { from, to } => Some((from.as_str(), to.as_str())),
            _ => None,
        }) {
            let short: Vec<String> = path
                .iter()
                .map(|f| {
                    f.strip_prefix(&format!("{project}/spec."))
                        .unwrap_or(f)
                        .to_string()
                })
                .collect();
            anyhow::bail!(
                "adding dependency {req_id} → {dep} would create a cycle: {}",
                short.join(" → ")
            );
        }
        records.push(Record::DependsOn {
            from: req_fqn.clone(),
            to: dep_fqn,
        });
    }
    Ok(())
}

/// `apg spec spine <project> <from-fqn> --drives <to> --requires <to>
/// --realises <to> --represents <to> --implemented-by <to>` (PHASE_01). Sets
/// the spine edges out of a tier-1/2/3 node (a Requirement drives/requires a
/// Domain; a Domain realises/represents a Solution node; a Solution node is
/// implemented-by code). Each edge kind is a set: repeated calls replace only
/// that kind's outgoing edges. Targets must be declared spec nodes (or code
/// FQNs for `--implemented-by`); a Requirement source may also be given by
/// bare id (`R1`).
fn spec_spine(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(from)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!(
            "usage: apg spec spine <project> <from-fqn> [--drives <to>]* [--requires <to>]* [--realises <to>]* [--represents <to>]* [--implemented-by <to>]*"
        );
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    apply_spine(&apg_root, project, from, &p)?;
    println!("Spined {project} {from}");
    Ok(())
}

/// Applies the spine edges out of `from` for the flags in `p` (write-through).
/// A bare `from` is a requirement id (`R1`); otherwise a full FQN. Each edge
/// kind's outgoing set is replaced by the given targets. Shared with tests so
/// they can drive the spine against a fixture root.
fn apply_spine(
    apg_root: &Path,
    project: &str,
    from: &str,
    p: &ParsedArgs,
) -> anyhow::Result<()> {
    let mut records = load_project(apg_root, project)?;

    // Resolve the source fqn: a bare requirement id (`R1`) or a full fqn.
    let from_fqn = if from.contains('/') {
        from.to_string()
    } else {
        format!("{project}/spec.{from}")
    };
    if !records
        .iter()
        .any(|r| node_fqn(r) == Some(from_fqn.as_str()))
    {
        anyhow::bail!("source `{from}` is not a declared node of `{project}`");
    }

    // Every flag name maps to (Record variant, relational table, target-kind
    // validator). The validator checks the *target* kind — the same kind pair
    // the scan load path enforces (ingest.rs) — so an edge that could never
    // materialize in the DB is rejected here with a clear CLI message. The
    // `--implemented-by` target is a code FQN (validated against the live
    // graph below), not a spec node. The source kind is checked per edge too.
    #[allow(clippy::type_complexity)]
    let sets: &[(&str, &str, fn(&[Record], &str) -> bool)] = &[
        ("drives", "Drives", |r, t| domain_is(r, t)),
        ("requires", "Requires", |r, t| domain_is(r, t)),
        ("realises", "Realises", |r, t| solution_is(r, t)),
        ("represents", "Represents", |r, t| solution_is(r, t)),
        ("implemented-by", "ImplementedBy", |_, _| true),
    ];
    let mut touched = false;
    for (flag, table, valid) in sets {
        let targets = p.all(flag);
        if targets.is_empty() {
            continue;
        }
        touched = true;
        // The source kind must match the edge's direction: Drives/Requires
        // originate on a Requirement, Realises/Represents on a Domain,
        // ImplementedBy on a Solution node.
        let src_ok = match *table {
            "Drives" | "Requires" => req_is(&records, &from_fqn),
            "Realises" | "Represents" => domain_is(&records, &from_fqn),
            "ImplementedBy" => solution_is(&records, &from_fqn),
            _ => unreachable!(),
        };
        if !src_ok {
            anyhow::bail!(
                "spine edge {table}({from_fqn} → …) has an invalid source kind"
            );
        }
        // Replace only this kind's outgoing edges from the source.
        let tbl: &str = table;
        records.retain(|r| match r {
            Record::Drives { from, .. } => !(from == &from_fqn && tbl == "Drives"),
            Record::Requires { from, .. } => !(from == &from_fqn && tbl == "Requires"),
            Record::Realises { from, .. } => !(from == &from_fqn && tbl == "Realises"),
            Record::Represents { from, .. } => !(from == &from_fqn && tbl == "Represents"),
            Record::ImplementedBy { from, .. } => {
                !(from == &from_fqn && tbl == "ImplementedBy")
            }
            _ => true,
        });
        // The live DB is needed only for --implemented-by targets (code-FQN
        // resolution against the graph).
        let db = if tbl == "ImplementedBy" {
            Some(artifacts::ArtifactDb::open(apg_root)?)
        } else {
            None
        };
        for t in &targets {
            let to_fqn = if t.contains('/') || tbl == "ImplementedBy" {
                t.clone()
            } else {
                format!("{project}/{t}")
            };
            if tbl == "ImplementedBy" {
                // The --implemented-by target must be an implementation code
                // node — exactly the labels the ImplementedBy rel-table
                // carries. Any other code label (UnresolvedTarget) or a spec
                // node would pass resolution and vanish silently at re-ingest
                // (no rel-table pair), so it is rejected up front (the same
                // allow-list pattern as `add_note --on`/`invariant add --guard`).
                match db.as_ref().unwrap().code_label(&to_fqn) {
                    Some(label) if load::implemented_by_to_labels().contains(&label) => {}
                    Some(label) => anyhow::bail!(
                        "spine target `{t}` is a `{label}` node, not implementation code — ImplementedBy runs Solution → Module/File/Struct/Function"
                    ),
                    None => anyhow::bail!(
                        "spine target `{t}` is not a resolved code FQN (--implemented-by requires an implementation code node)"
                    ),
                }
            } else if !records.iter().any(|r| node_fqn(r) == Some(to_fqn.as_str())) {
                anyhow::bail!("spine target `{t}` is not a declared node of `{project}`");
            }
            if !valid(&records, &to_fqn) {
                anyhow::bail!(
                    "spine edge {table}({from_fqn} → {to_fqn}) has an invalid kind pair"
                );
            }
            let rec = match tbl {
                "Drives" => Record::Drives {
                    from: from_fqn.clone(),
                    to: to_fqn,
                },
                "Requires" => Record::Requires {
                    from: from_fqn.clone(),
                    to: to_fqn,
                },
                "Realises" => Record::Realises {
                    from: from_fqn.clone(),
                    to: to_fqn,
                },
                "Represents" => Record::Represents {
                    from: from_fqn.clone(),
                    to: to_fqn,
                },
                "ImplementedBy" => Record::ImplementedBy {
                    from: from_fqn.clone(),
                    to: to_fqn,
                },
                _ => unreachable!(),
            };
            records.push(rec);
        }
    }
    if !touched {
        anyhow::bail!(
            "usage: apg spec spine <project> <from-fqn> --drives/--requires/--realises/--represents/--implemented-by <to> …"
        );
    }
    write_through(apg_root, project, &records)?;
    Ok(())
}

/// Whether `t` names a Requirement node in the project's records.
fn req_is(records: &[Record], t: &str) -> bool {
    records
        .iter()
        .any(|r| matches!(r, Record::Requirement { fqn, .. } if fqn == t))
}

/// Whether `t` names a Domain node.
fn domain_is(records: &[Record], t: &str) -> bool {
    records.iter().any(|r| matches!(r, Record::Domain { fqn, .. } if fqn == t))
}

/// Whether `t` names a Solution-tier node (System/Container/Component).
fn solution_is(records: &[Record], t: &str) -> bool {
    records.iter().any(|r| {
        matches!(
            r,
            Record::System { fqn, .. }
                | Record::Container { fqn, .. }
                | Record::Component { fqn, .. }
                if fqn == t
        )
    })
}

/// `apg spec rm <project> <fqn|id>` — remove a node and its incident edges.
fn spec_rm(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let (Some(project), Some(id)) = (p.positional.first(), p.positional.get(1)) else {
        anyhow::bail!("usage: apg spec rm <project> <fqn|id>");
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    let mut records = load_project(&apg_root, project)?;
    let fqn = if id.contains('/') {
        id.clone()
    } else {
        format!("{project}/spec.{id}")
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
            let out = apg_root
                .join(specs::TRANS)
                .join("specs")
                .join(format!("{project}.md"));
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
    let fqn_of = |prefix: &str| format!("{project}/{prefix}");

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
            let Record::Requirement {
                fqn,
                id,
                title,
                body,
                ..
            } = r
            else {
                unreachable!()
            };
            feature_lines.push_str(&format!("**{id} — {title}.** {body}\n"));
            let consumes: Vec<String> = records
                .iter()
                .filter_map(|e| match e {
                    Record::DependsOn { from, to } if from == fqn => Some(short_req(to, &project)),
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

    // Tiers 1–3 + the spine (GraphModel-SPEC.md): the proposed reality and the
    // why-to-code chain — invisible in the requirements-only projection.
    let stakeholders: Vec<&Record> = records
        .iter()
        .filter(|r| matches!(r, Record::Stakeholder { .. }))
        .collect();
    if !stakeholders.is_empty() {
        out.push_str("## Stakeholders\n");
        for r in &stakeholders {
            let (_, _, name, _, body) = tier_display(r).unwrap();
            if body.is_empty() {
                out.push_str(&format!("- **`{name}`**\n"));
            } else {
                out.push_str(&format!("- **`{name}`** — {body}\n"));
            }
        }
        out.push('\n');
    }

    let domain_tree = tier_tree_markdown(records, &project, &TIER2_LABELS);
    if !domain_tree.is_empty() {
        out.push_str(&format!("## Domain\n{domain_tree}\n"));
    }

    let solution_tree = tier_tree_markdown(records, &project, &TIER3_LABELS);
    if !solution_tree.is_empty() {
        out.push_str(&format!("## Solution\n{solution_tree}\n"));
    }

    let spine = spine_markdown(records, &project);
    if !spine.is_empty() {
        out.push_str(&format!("## Spine\n{spine}\n"));
    }

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
        nongoals
            .iter()
            .map(|b| format!("- {b}\n"))
            .collect::<String>()
    ));

    let design = notes("design");
    if !design.is_empty() {
        out.push_str(&format!(
            "## Design\n{}\n",
            design
                .iter()
                .map(|b| format!("- {b}\n"))
                .collect::<String>()
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
        acs.iter()
            .map(|b| format!("- [ ] {b}\n"))
            .collect::<String>()
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
                            && matches!(to, t if t.starts_with(&format!("{project}/spec."))) =>
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
            Record::Decision { id, summary, .. } => Some(format!("**Decision `{id}`.** {summary}")),
            _ => None,
        })
        .collect();
    if !decisions.is_empty() {
        out.push_str(&format!(
            "## Decisions\n{}\n",
            decisions
                .iter()
                .map(|d| format!("- {d}\n"))
                .collect::<String>()
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
    let prefix = format!("{project}/spec.");
    fqn.strip_prefix(&prefix).unwrap_or(fqn).to_string()
}

/// Renders a DependsOn target compactly: the bare id for same-project deps,
/// `<project>/<id>` for cross-project ones.
fn short_req(fqn: &str, project: &str) -> String {
    let prefix = format!("{project}/spec.");
    if let Some(id) = fqn.strip_prefix(&prefix) {
        return id.to_string();
    }
    if let Some((proj, id)) = fqn.split_once("/spec.") {
        return format!("{proj}/{id}");
    }
    fqn.to_string()
}

/// The short form of a project-scoped FQN (`<project>/spec.R1` → `spec.R1`);
/// code FQNs (no project prefix) pass through unchanged.
fn short_fqn(fqn: &str, project: &str) -> String {
    let prefix = format!("{project}/");
    fqn.strip_prefix(&prefix).unwrap_or(fqn).to_string()
}

/// The tier-2 DDD label set (GraphModel-SPEC.md) rendered under `## Domain`.
const TIER2_LABELS: [&str; 9] = [
    "Domain",
    "Subdomain",
    "Entity",
    "ValueObject",
    "Aggregate",
    "DomainEvent",
    "DomainProcess",
    "DomainRule",
    "Actor",
];

/// The tier-3 C4 label set (GraphModel-SPEC.md) rendered under `## Solution`.
const TIER3_LABELS: [&str; 3] = ["System", "Container", "Component"];

/// The tier-node display tuple `(fqn, label, name, meta, body)` of a
/// tier-1/2/3 record, or `None` for non-tier records. `meta` is the node's
/// type metadata (`kind: core` for a Subdomain, `root: Order` for an
/// Aggregate, `kind: app` for a Container).
fn tier_display(r: &Record) -> Option<(String, &'static str, &str, String, &str)> {
    match r {
        Record::Stakeholder { fqn, name, body } => Some((fqn.clone(), "Stakeholder", name, String::new(), body)),
        Record::Domain { fqn, name, body } => Some((fqn.clone(), "Domain", name, String::new(), body)),
        Record::Subdomain { fqn, name, kind, body } => {
            Some((fqn.clone(), "Subdomain", name, tier_meta("kind", kind), body))
        }
        Record::Entity { fqn, name, body } => Some((fqn.clone(), "Entity", name, String::new(), body)),
        Record::ValueObject { fqn, name, body } => Some((fqn.clone(), "ValueObject", name, String::new(), body)),
        Record::Aggregate { fqn, name, root, body } => {
            Some((fqn.clone(), "Aggregate", name, tier_meta("root", root), body))
        }
        Record::DomainEvent { fqn, name, body } => {
            Some((fqn.clone(), "DomainEvent", name, String::new(), body))
        }
        Record::DomainProcess { fqn, name, body } => {
            Some((fqn.clone(), "DomainProcess", name, String::new(), body))
        }
        Record::DomainRule { fqn, name, body } => Some((fqn.clone(), "DomainRule", name, String::new(), body)),
        Record::Actor { fqn, name, body } => Some((fqn.clone(), "Actor", name, String::new(), body)),
        Record::System { fqn, name, body } => Some((fqn.clone(), "System", name, String::new(), body)),
        Record::Container { fqn, name, kind, body } => {
            Some((fqn.clone(), "Container", name, tier_meta("kind", kind), body))
        }
        Record::Component { fqn, name, body } => Some((fqn.clone(), "Component", name, String::new(), body)),
        _ => None,
    }
}

/// `label: value` when the value is non-empty (the `kind`/`root` type
/// metadata of a Subdomain/Container/Aggregate).
fn tier_meta(label: &str, value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        format!("{label}: {value}")
    }
}

/// Renders the tier nodes of one tier (`TIER2_LABELS`/`TIER3_LABELS`) as a
/// nested markdown list following the Contains hierarchy (`Domain ⊃ Subdomain
/// ⊃ Aggregate ⊃ Entity/ValueObject`, `System ⊃ Container ⊃ Component`). The
/// walk descends from the spec root, emitting only nodes in `tier`; non-tier
/// ancestors (the spec root, a skipped other-tier node) pass through without
/// rendering.
fn tier_tree_markdown(records: &[Record], project: &str, tier: &[&str]) -> String {
    let mut tier_by_fqn: HashMap<String, (&'static str, String, String, String)> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for r in records {
        if let Some((fqn, label, name, meta, body)) = tier_display(r) {
            tier_by_fqn.insert(fqn, (label, name.to_string(), meta, body.to_string()));
        }
        if let Record::Contains { from, to } = r {
            children.entry(from.clone()).or_default().push(to.clone());
        }
    }
    let mut out = String::new();
    walk_tier_tree(
        &children,
        &tier_by_fqn,
        &format!("{project}/spec"),
        0,
        tier,
        &mut out,
    );
    out
}

fn walk_tier_tree(
    children: &HashMap<String, Vec<String>>,
    tier_by_fqn: &HashMap<String, (&'static str, String, String, String)>,
    fqn: &str,
    depth: usize,
    tier: &[&str],
    out: &mut String,
) {
    let child_depth = match tier_by_fqn.get(fqn) {
        Some((label, name, meta, body)) if tier.contains(label) => {
            let indent = "  ".repeat(depth);
            out.push_str(&format!("{indent}- **{label} `{name}`**"));
            if !meta.is_empty() {
                out.push_str(&format!(" ({meta})"));
            }
            out.push('\n');
            if !body.is_empty() {
                out.push_str(&format!("{indent}  {body}\n"));
            }
            depth + 1
        }
        _ => depth,
    };
    if let Some(cs) = children.get(fqn) {
        for c in cs {
            walk_tier_tree(children, tier_by_fqn, c, child_depth, tier, out);
        }
    }
}

/// Renders the spine edges (Requirement → Domain → Solution → code) as a
/// markdown list in short FQN form — the why-to-code chain.
fn spine_markdown(records: &[Record], project: &str) -> String {
    let short = |fqn: &str| short_fqn(fqn, project);
    let mut out = String::new();
    for r in records {
        let (from, edge, to) = match r {
            Record::Drives { from, to } => (from, "Drives", to),
            Record::Requires { from, to } => (from, "Requires", to),
            Record::Realises { from, to } => (from, "Realises", to),
            Record::Represents { from, to } => (from, "Represents", to),
            Record::ImplementedBy { from, to } => (from, "ImplementedBy", to),
            _ => continue,
        };
        out.push_str(&format!("- `{}` --{}--> `{}`\n", short(from), edge, short(to)));
    }
    out
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
    // A planned node (no location yet) or module carries no path — no loc to
    // show.
    if path.is_empty() {
        return None;
    }
    Some((path, line))
}

/// `apg spec unresolved [project]` (SpecCreation-SPEC §1 self-lint, §2
    /// orphan/coverage lint) — a read-only lint over the spec/plan graph, mirroring
/// the `apg_spec_unresolved.ts` suite tool. It reads the durable JSONL directly
/// (the graph's source of truth — so it works on the committed records a write
/// would re-ingest) and consults the live DB only to decide whether a planned
/// Implementation node has been realized (a scan replaced it). Reports, per
/// project: pending anchors (to a planned node or proposed Solution node),
/// realized/unbuilt/unreferenced planned nodes, orphan requirements (no
/// Satisfies, no Implements), ACs without a covering requirement, dangling
/// depends_on/gates refs, and open/actioned or drift feedback.
///
/// Orphan/coverage are whole-spec, post-hoc properties (a freshly added
/// requirement is trivially an orphan until a plan Satisfies it), so this is a
/// lint command, not a write-time rejection gate — the SPEC's §2 "write-time"
/// placement is reconciled to the read-only lint the flow actually needs.
fn spec_unresolved(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let filter = p.positional.first().cloned();
    let apg_root = require_apg_root()?;
    print!("{}", spec_unresolved_report(&apg_root, filter.as_deref())?);
    Ok(())
}

/// The `spec unresolved` report as a string (unit-testable without a stdout
/// capture or a chdir). Returns the same text `spec_unresolved` prints.
fn spec_unresolved_report(apg_root: &Path, filter: Option<&str>) -> anyhow::Result<String> {
    let records = specs::read_all(apg_root);
    let db = artifacts::ArtifactDb::open(apg_root).ok();

    let mut projects: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            Record::Spec { fqn, .. } => fqn.split('/').next().map(|s| s.to_string()),
            _ => None,
        })
        .collect();
    projects.sort();
    projects.dedup();
    if let Some(f) = &filter {
        projects.retain(|p| p == f);
    }
    if projects.is_empty() {
        return Ok("No specs found — nothing to lint.\n".to_string());
    }

    // Cross-project sets for dangling depends_on/gates: spec graphs merge into
    // one space, so a depends target may be another spec's requirement.
    let all_reqs: HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::Requirement { fqn, .. } => Some(fqn.as_str()),
            _ => None,
        })
        .collect();
    let all_phases: HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::Phase { fqn, .. } => Some(fqn.as_str()),
            _ => None,
        })
        .collect();
    // Planned Implementation nodes (plan-writer-authored tier-4 additions) and
    // proposed Solution nodes — the pending-anchor targets of the finalized
    // model (GraphModel-SPEC.md). The placeholder node is gone.
    let planned_nodes: HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::PlannedNode { fqn, .. } => Some(fqn.as_str()),
            _ => None,
        })
        .collect();
    let solution_nodes: HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::System { fqn, .. }
            | Record::Container { fqn, .. }
            | Record::Component { fqn, .. } => Some(fqn.as_str()),
            _ => None,
        })
        .collect();
    // Planned nodes grouped by their owning project. A planned node's FQN is
    // the real code FQN it will land at (not project-scoped), so its project
    // is the plan that authored it — read from each plan file directly.
    let mut planned_by_project: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for f in specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans")) {
        let recs = specs::read_jsonl(&f).unwrap_or_default();
        let Some(proj) = recs.iter().find_map(|r| match r {
            Record::Plan { fqn, .. } => fqn.split('/').next().map(|s| s.to_string()),
            _ => None,
        }) else {
            continue;
        };
        planned_by_project.entry(proj).or_default().extend(
            recs.iter()
                .filter_map(|r| match r {
                    Record::PlannedNode { fqn, kind, .. } => {
                        Some((fqn.clone(), kind.clone()))
                    }
                    _ => None,
                }),
        );
    }

    let satisfies = edges_of(&records, |r| match r {
        Record::Satisfies { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });
    let implements = edges_of(&records, |r| match r {
        Record::Implements { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });
    let anchors = edges_of(&records, |r| match r {
        Record::Anchors { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });
    let deps = edges_of(&records, |r| match r {
        Record::DependsOn { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });
    let gates = edges_of(&records, |r| match r {
        Record::Gates { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });
    let contains = edges_of(&records, |r| match r {
        Record::Contains { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });
    let builds = edges_of(&records, |r| match r {
        Record::Builds { from, to } => Some((from.as_str(), to.as_str())),
        _ => None,
    });

    let mut out: Vec<String> = Vec::new();
    for proj in &projects {
        let pfx = format!("{proj}/");
        let in_p = |s: &str| s.starts_with(&pfx);

        let reqs: Vec<&Record> = records
            .iter()
            .filter(|r| matches!(r, Record::Requirement { .. }) && node_fqn(r).is_some_and(in_p))
            .collect();
        let impl_reqs: HashSet<&str> = implements
            .iter()
            .filter(|(_, to)| in_p(to))
            .map(|(_, to)| *to)
            .collect();
        let sat_reqs: HashSet<&str> = satisfies
            .iter()
            .filter(|(_, to)| in_p(to))
            .map(|(_, to)| *to)
            .collect();

        // Planned code (the plan's tier-4 additions).
        let mut pending: Vec<String> = Vec::new();
        let mut realized: Vec<String> = Vec::new();
        let mut unsatisfied: Vec<String> = Vec::new();
        for (fqn, kind) in planned_by_project.get(proj).into_iter().flatten() {
            let built = builds.iter().any(|(_, to)| *to == fqn.as_str());
            if let Some(db) = &db {
                if db.code_label(fqn).is_some() && !db.is_planned(fqn) {
                    realized.push(format!(
                        "  {fqn} ({kind}) — a scan realized it (present code, no `planned` status)"
                    ));
                } else {
                    unsatisfied.push(format!(
                        "  {fqn} ({kind}) — planned code not yet built (no real node at the FQN)"
                    ));
                }
            } else {
                unsatisfied.push(format!(
                    "  {fqn} ({kind}) — realization not verifiable without a scan"
                ));
            }
            if !built {
                pending.push(format!("  {fqn} ({kind}) — no task Builds it"));
            }
        }

        // Pending anchors: an Anchors edge whose target is a planned
        // Implementation node or a proposed Solution node.
        let pending_anchors: Vec<&str> = anchors
            .iter()
            .filter(|(from, to)| in_p(from) && (planned_nodes.contains(to) || solution_nodes.contains(to)))
            .map(|(_, to)| *to)
            .collect();

        // Orphan requirements: no Satisfies, no Implements.
        let mut orphans: Vec<(String, String)> = Vec::new();
        for r in &reqs {
            let fqn = node_fqn(r).unwrap();
            if impl_reqs.contains(fqn) || sat_reqs.contains(fqn) {
                continue;
            }
            if let Record::Requirement { id, title, .. } = r {
                orphans.push((id.clone(), title.clone()));
            }
        }

        // AC coverage: an AC in a PlanPhase that Satisfies no requirement is
        // uncovered; a spec-level AC with no requirements at all is uncovered.
        let plan_phase_sat: HashSet<&str> = satisfies.iter().map(|(from, _)| *from).collect();
        let mut uncovered_ac: Vec<String> = Vec::new();
        for (from, to) in &contains {
            if !in_p(to) {
                continue;
            }
            let is_ac = records.iter().any(|r| {
                matches!(r, Record::AcceptanceCriterion { .. }) && node_fqn(r) == Some(*to)
            });
            if !is_ac {
                continue;
            }
            if from.contains("/plan.") {
                if !plan_phase_sat.contains(from) {
                    uncovered_ac.push(format!("  {to} — in {from} which Satisfies no requirement"));
                }
            } else if reqs.is_empty() {
                uncovered_ac.push(format!("  {to} — spec has no requirements to cover"));
            }
        }

        // Dangling depends_on / gates.
        let mut dangling: Vec<String> = Vec::new();
        for (from, to) in &deps {
            if in_p(from) && !all_reqs.contains(to) {
                dangling.push(format!("  depends_on {from} -> {to}"));
            }
        }
        for (from, to) in &gates {
            if in_p(from) && !all_phases.contains(to) {
                dangling.push(format!("  gates {from} -> {to}"));
            }
        }

        // Feedback under review + status/disposition drift (hand-edited JSONL
        // that would silently break the resolved gate).
        let mut feedback: Vec<(String, String)> = Vec::new();
        let mut drift: Vec<String> = Vec::new();
        for r in records.iter().filter(|r| {
            matches!(r, Record::Feedback { .. }) && node_fqn(r).is_some_and(in_p)
        }) {
            if let Record::Feedback { fqn, status, disposition, .. } = r {
                if status != "resolved" {
                    feedback.push((fqn.clone(), status.clone()));
                }
                if !["open", "actioned", "resolved"].contains(&status.as_str())
                    || !["", "fixed", "wont-fix", "rejected"].contains(&disposition.as_str())
                {
                    drift.push(format!("  {fqn} (status: {status}, disposition: {disposition})"));
                }
            }
        }

        let mut info: Vec<String> = Vec::new();
        let mut findings: Vec<String> = Vec::new();
        // Informational (expected / already-resolved) — shown only alongside
        // actual findings, never blocking "Lint clean".
        if !pending_anchors.is_empty() {
            info.push(format!(
                "pending anchors (expected — proposed code, {}):",
                pending_anchors.len()
            ));
            for t in &pending_anchors {
                info.push(format!("  {t}"));
            }
        }
        if !realized.is_empty() {
            info.push(format!(
                "realized planned nodes — a branch scan replaced them with present code ({}):",
                realized.len()
            ));
            info.extend(realized);
        }
        // Findings (unresolved work that blocks delivery).
        if !unsatisfied.is_empty() {
            findings.push(format!(
                "unbuilt planned code — planned nodes not yet realized ({}):",
                unsatisfied.len()
            ));
            findings.extend(unsatisfied);
        }
        if !pending.is_empty() {
            findings.push(format!(
                "unreferenced planned nodes — no task Builds them ({}):",
                pending.len()
            ));
            findings.extend(pending);
        }
        if !orphans.is_empty() {
            findings.push(format!(
                "orphan requirements — no Satisfies, no Implements ({}):",
                orphans.len()
            ));
            for (id, title) in &orphans {
                findings.push(format!("  {id} — {title}"));
            }
        }
        if !uncovered_ac.is_empty() {
            findings.push(format!(
                "acceptance criteria without a covering requirement ({}):",
                uncovered_ac.len()
            ));
            findings.extend(uncovered_ac);
        }
        if !dangling.is_empty() {
            findings.push(format!("dangling refs ({}):", dangling.len()));
            findings.extend(dangling);
        }
        if !feedback.is_empty() {
            findings.push(format!(
                "feedback under review — must be resolved before apply ({}):",
                feedback.len()
            ));
            for (fqn, status) in &feedback {
                findings.push(format!("  {fqn} ({status})"));
            }
        }
        if !drift.is_empty() {
            findings.push(format!(
                "feedback status/disposition drift — hand-edited JSONL, breaks the resolved gate ({}):",
                drift.len()
            ));
            findings.extend(drift);
        }

        if !findings.is_empty() {
            out.push(format!("## {proj}"));
            out.extend(info.iter().map(|s| format!("- {s}")));
            out.extend(findings.iter().map(|s| format!("- {s}")));
        }
    }

    if out.is_empty() {
        Ok("Lint clean: no unresolved spec/plan issues found.\n".to_string())
    } else {
        Ok(format!("{}\n", out.join("\n")))
    }
}

/// Collects the (from, to) pairs of a specific spec-family edge type across
/// every project's records, in record order.
fn edges_of(
    records: &[Record],
    f: impl Fn(&Record) -> Option<(&str, &str)>,
) -> Vec<(&str, &str)> {
    records.iter().filter_map(f).collect()
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
        let dir = std::env::temp_dir().join(format!("apg-cli-test-{}-{name}", std::process::id()));
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
        g.contains.insert((
            "/abs/store.go".to_string(),
            "github.com/x/y.Store".to_string(),
        ));
        // An unresolved target (stdlib call the scanner couldn't resolve) — a
        // *code* node with no rel-table pair on ImplementedBy.
        g.nodes.insert(
            "fmt.Errorf".to_string(),
            Node {
                kind: NodeKind::UnresolvedTarget,
                category: Some("stdlib".to_string()),
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

    #[test]
    fn link_depends_on_adds_all_edges_once() {
        let mut records = vec![
            Record::Requirement {
                fqn: "foo/spec.R1".into(),
                id: "R1".into(),
                title: "A".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R2".into(),
                id: "R2".into(),
                title: "B".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R3".into(),
                id: "R3".into(),
                title: "C".into(),
                body: String::new(),
                feature: String::new(),
            },
        ];
        // Multiple --depends-on in one call must all land (regression: the
        // incident-edge removal used to run inside the loop, dropping earlier
        // edges so only the last dep survived).
        let apg = Path::new("/nonexistent");
        link_depends_on(apg, "foo", "R1", &["R2".into(), "R3".into()], &mut records).unwrap();
        let deps: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::DependsOn { from, to } if from == "foo/spec.R1" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deps, vec!["foo/spec.R2", "foo/spec.R3"]);

        // Re-linking replaces, never duplicates.
        link_depends_on(apg, "foo", "R1", &["R3".into()], &mut records).unwrap();
        let deps: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::DependsOn { from, to } if from == "foo/spec.R1" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deps, vec!["foo/spec.R3"]);

        // Self-deps and undeclared targets are write-time errors.
        assert!(link_depends_on(apg, "foo", "R1", &["R1".into()], &mut records).is_err());
        assert!(link_depends_on(apg, "foo", "R1", &["R9".into()], &mut records).is_err());
    }

    #[test]
    fn link_depends_on_preserves_incoming_edges() {
        // Regression: re-linking a requirement used `remove_incident_edges`,
        // which deletes edges in BOTH directions — re-linking R2 wiped
        // R3→R2 (a requirement that depends on R2) from the graph.
        let mut records = vec![
            Record::Requirement {
                fqn: "foo/spec.R1".into(),
                id: "R1".into(),
                title: "A".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R2".into(),
                id: "R2".into(),
                title: "B".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R3".into(),
                id: "R3".into(),
                title: "C".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::DependsOn {
                from: "foo/spec.R3".into(),
                to: "foo/spec.R2".into(),
            },
        ];
        link_depends_on(
            Path::new("/nonexistent"),
            "foo",
            "R2",
            &["R1".into()],
            &mut records,
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::DependsOn { from, to }
                if from == "foo/spec.R2" && to == "foo/spec.R1"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::DependsOn { from, to }
                if from == "foo/spec.R3" && to == "foo/spec.R2"
        )));
        assert_eq!(
            records
                .iter()
                .filter(|r| matches!(r, Record::DependsOn { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn note_kind_vocabulary_enforces_kind_and_target_category() {
        // Project notes: every known kind is valid; unknown kinds are rejected.
        for kind in [
            "note",
            "background",
            "design",
            "decision",
            "error-handling",
            "relationship-to-other-specs",
            "open-question",
            "materialization-fix",
        ] {
            validate_note_kind(kind, "project").unwrap();
        }
        assert!(validate_note_kind("backgroud", "project").is_err());
        assert!(validate_note_kind("", "project").is_err());

        // materialization-fix: spec nodes only, never code.
        validate_note_kind("materialization-fix", "spec").unwrap();
        assert!(validate_note_kind("materialization-fix", "code").is_err());
        // warning/gotcha: code nodes only.
        validate_note_kind("warning", "code").unwrap();
        assert!(validate_note_kind("warning", "spec").is_err());
        // rationale: spec or code, not a bare project note.
        validate_note_kind("rationale", "spec").unwrap();
        validate_note_kind("rationale", "code").unwrap();
        assert!(validate_note_kind("rationale", "project").is_err());
        // decision/design: anywhere.
        validate_note_kind("decision", "spec").unwrap();
        validate_note_kind("decision", "code").unwrap();
        // generic note: anywhere.
        validate_note_kind("note", "code").unwrap();
    }

    #[test]
    fn link_depends_on_rejects_cycles() {
        // The platform dogfood case: O5↔O6 mutual dependence. Once O5→O6
        // exists, linking O6 → O5 must be rejected — "delivered when its
        // dependencies are delivered" is circular otherwise.
        let mut records = vec![
            Record::Requirement {
                fqn: "foo/spec.O5".into(),
                id: "O5".into(),
                title: "token exchange".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.O6".into(),
                id: "O6".into(),
                title: "token store".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.O7".into(),
                id: "O7".into(),
                title: "sharding".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::DependsOn {
                from: "foo/spec.O6".into(),
                to: "foo/spec.O7".into(),
            },
        ];
        // Longer cycle first: O6 → O7 exists, so O7 → O6 closes O7→O6→O7.
        let err = link_depends_on(
            Path::new("/nonexistent"),
            "foo",
            "O7",
            &["O6".into()],
            &mut records,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("O7 → O6"));
        // O5 → O6 is fine on its own.
        link_depends_on(
            Path::new("/nonexistent"),
            "foo",
            "O5",
            &["O6".into()],
            &mut records,
        )
        .unwrap();
        // O6 → O5 closes O5 → O6 → O5 — rejected.
        let err = link_depends_on(
            Path::new("/nonexistent"),
            "foo",
            "O6",
            &["O5".into()],
            &mut records,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("would create a cycle"), "got: {msg}");
        assert!(msg.contains("O6 → O5"), "got: {msg}");
        // The invalid edge was not persisted.
        assert!(!records.iter().any(|r| matches!(
            r,
            Record::DependsOn { from, to }
                if from == "foo/spec.O6" && to == "foo/spec.O5"
        )));
    }

    #[test]
    fn spec_add_phase_rejects_transitive_gate_cycles() {
        // Phase-1 gates on phase-2, phase-2 gates on phase-3. Adding
        // phase-3 → phase-1 closes 3 → 1 → 2 → 3 — the old code only rejected
        // a self-gate, so this transitive cycle was written and only the
        // `apg_plan_phases` reviewer backstop caught it (SpecCreation-SPEC §2:
        // "acyclic … DependsOn/Gates").
        let records = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Phase {
                fqn: "foo/spec.phase-1".into(),
                number: 1,
                title: "p1".into(),
            },
            Record::Phase {
                fqn: "foo/spec.phase-2".into(),
                number: 2,
                title: "p2".into(),
            },
            Record::Phase {
                fqn: "foo/spec.phase-3".into(),
                number: 3,
                title: "p3".into(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/spec.phase-1".into(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/spec.phase-2".into(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/spec.phase-3".into(),
            },
            Record::Gates {
                from: "foo/spec.phase-1".into(),
                to: "foo/spec.phase-2".into(),
            },
            Record::Gates {
                from: "foo/spec.phase-2".into(),
                to: "foo/spec.phase-3".into(),
            },
        ];
        // The re-added phase-3 with its Contains edge, as `spec add` would
        // have accumulated by the time the first --gate is validated.
        let recs = vec![
            Record::Phase {
                fqn: "foo/spec.phase-3".into(),
                number: 3,
                title: "p3".into(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/spec.phase-3".into(),
            },
        ];
        let err = phase_gate_edge("foo", 3, "foo/spec.phase-3", 1, &records, &recs).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("would create a cycle"), "got: {msg}");
        assert!(msg.contains("phase-3 → phase-1"), "got: {msg}");
        // The rejected edge is never accumulated into recs.
        assert!(!recs.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/spec.phase-3" && to == "foo/spec.phase-1"
        )));
        // A self-gate keeps its dedicated rejection.
        let err = phase_gate_edge("foo", 3, "foo/spec.phase-3", 3, &records, &recs).unwrap_err();
        assert!(format!("{err:#}").contains("cannot gate on itself"));
        // A benign gate still lands: phase-3 → phase-4 closes nothing.
        let g = phase_gate_edge("foo", 3, "foo/spec.phase-3", 4, &records, &recs).unwrap();
        match g {
            Record::Gates { from, to } => {
                assert_eq!((from.as_str(), to.as_str()), ("foo/spec.phase-3", "foo/spec.phase-4"))
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn write_through_and_planned_node_roundtrip() {
        let (apg_root, dir) = fixture_layout("roundtrip");

        // Author a spec with a requirement anchored to a proposed Solution
        // node (the pending tier-3 anchor of the finalized model), plus a plan
        // that carries a planned Implementation node and a task that Builds it.
        let recs = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: "G".to_string(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Timer".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/spec.R1".to_string(),
            },
            Record::System {
                fqn: "foo/system.Gateway".to_string(),
                name: "Gateway".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/system.Gateway".to_string(),
            },
            Record::Anchors {
                from: "foo/spec.R1".to_string(),
                to: "foo/system.Gateway".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
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
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Store".to_string(),
                kind: "struct".to_string(),
                name: "Store".to_string(),
                parent: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::Builds {
                from: "foo/plan.phase-01.task-1".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        specs::write_jsonl(&specs::plan_jsonl_path(&apg_root, "foo"), &plan).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // The planned node lands as a Struct with status=planned; the pending
        // anchor to the proposed Solution node lands; the Builds edge lands.
        {
            let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
            assert!(db.is_planned("github.com/x/y.Store"));
            let out = db
                .conn()
                .unwrap()
                .query("MATCH (s:Struct {fqn: 'github.com/x/y.Store'}) RETURN s.status")
                .unwrap()
                .to_string();
            assert!(out.contains("planned"), "planned status: {out}");
            let out = db
                .conn()
                .unwrap()
                .query("MATCH (:Requirement)-[:Anchors]->(s:System) RETURN s.fqn")
                .unwrap()
                .to_string();
            assert!(out.contains("foo/system.Gateway"), "pending anchor: {out}");
            let out = db
                .conn()
                .unwrap()
                .query("MATCH (:Task)-[:Builds]->(s:Struct) RETURN s.fqn")
                .unwrap()
                .to_string();
            assert!(
                out.contains("github.com/x/y.Store"),
                "planned build: {out}"
            );
        }

        // The write-through re-ingest reproduces the same state from the
        // committed JSONLs.
        artifacts::reingest_project(&apg_root, "foo").unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.is_planned("github.com/x/y.Store"));
        assert!(db.has_node("foo/system.Gateway"));
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Task)-[:Builds]->(s:Struct) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "rebuild keeps Builds: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dangling_requirement_anchor_is_dropped_not_synthesized() {
        let (apg_root, dir) = fixture_layout("dangling-anchor");
        // A requirement anchored to code that is not in the graph: the anchor
        // is dropped at re-ingest (the placeholder node is gone — a pending
        // anchor must name a proposed Solution node or a planned node, both
        // authored, never auto-created).
        let recs = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Timer".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/spec.R1".to_string(),
            },
            Record::Anchors {
                from: "foo/spec.R1".to_string(),
                to: "github.com/x/y.DoesNotExist".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Requirement)-[:Anchors]->() RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("0"),
            "no placeholder may be synthesized: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_node_strips_incident_edges_only() {
        let mut records = vec![
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "t".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Anchors {
                from: "foo/spec.R1".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
            Record::Anchors {
                from: "foo/spec.R2".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        artifacts::remove_node(&mut records, "foo/spec.R1");
        // R1's node and its anchor are gone; R2's anchor (same code target)
        // survives untouched.
        assert_eq!(records.len(), 1);
        assert!(records.iter().any(|r| {
            matches!(r, Record::Anchors { from, .. } if from == "foo/spec.R2")
        }));
    }

    #[test]
    fn anchor_upsert_accumulates_across_calls() {
        let mut records = Vec::new();
        // Two sequential anchor calls on the same requirement: the first must
        // survive the second (last-wins was the bug that lost anchors).
        anchor_upsert("foo/spec.R1", "github.com/x/y.Store", &mut records);
        anchor_upsert("foo/spec.R1", "github.com/x/y.Loader", &mut records);
        anchor_upsert("foo/spec.R2", "github.com/x/y.Loader", &mut records);
        let edges: Vec<(&str, &str)> = records
            .iter()
            .filter_map(|r| match r {
                Record::Anchors { from, to } => Some((from.as_str(), to.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(edges.len(), 3);
        assert!(edges.contains(&("foo/spec.R1", "github.com/x/y.Store")));
        assert!(edges.contains(&("foo/spec.R1", "github.com/x/y.Loader")));
        assert!(edges.contains(&("foo/spec.R2", "github.com/x/y.Loader")));
        // Re-adding an existing (from, to) pair is an idempotent no-op.
        anchor_upsert("foo/spec.R1", "github.com/x/y.Store", &mut records);
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn add_requirement_works_with_no_db_and_zero_anchors() {
        // A bare `apg/` layout with a specs dir but NO db.lbug — a fresh
        // project before any scan. The documented write-through principle
        // (artifacts.rs) says a missing DB is not an error, and the other add
        // kinds work DB-less; requirement used to open `ArtifactDb`
        // unconditionally even with zero `--anchor` flags and fail
        // ("db.lbug does not exist"). Anchors resolve against the scanned
        // code graph, so they should be the ONLY thing that demands a scan.
        let dir =
            std::env::temp_dir().join(format!("apg-spec-test-{}-req-nodb", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        std::fs::create_dir_all(dir.join("apg").join("specs")).unwrap();
        let apg_root = dir.join("apg");
        assert!(
            !apg_root.join(specs::TRANS).join("db.lbug").exists(),
            "fixture must start DB-less"
        );

        // Seed a project spec.
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        specs::write_jsonl(
            &path,
            &[Record::Spec {
                fqn: "foo/spec".into(),
                title: "Foo".into(),
                goal: String::new(),
            }],
        )
        .unwrap();
        let mut records = load_project(&apg_root, "foo").unwrap();

        // Zero-anchor requirement add must succeed DB-less (regression: this
        // used to fail on the unconditional ArtifactDb::open).
        let p = parse_args(&[
            "foo".to_string(),
            "requirement".to_string(),
            "R1".to_string(),
            "--title".to_string(),
            "Do the thing".to_string(),
            "--body".to_string(),
            "The thing must happen".to_string(),
            "--feature".to_string(),
            "feature-a".to_string(),
        ]);
        add_requirement(&p, &apg_root, "foo", &mut records).unwrap();
        assert!(
            !apg_root.join(specs::TRANS).join("db.lbug").exists(),
            "zero-anchor add must not create a DB"
        );
        let saved = specs::read_jsonl(&path).unwrap();
        assert!(saved.iter().any(|r| matches!(
            r,
            Record::Requirement { fqn, id, title, body, feature }
                if fqn == "foo/spec.R1"
                    && id == "R1"
                    && title == "Do the thing"
                    && body == "The thing must happen"
                    && feature == "feature-a"
        )));
        assert!(saved.iter().any(|r| matches!(
            r,
            Record::Contains { from, to } if from == "foo/spec" && to == "foo/spec.R1"
        )));
        assert!(saved.iter().all(|r| !matches!(r, Record::Anchors { .. })));

        // --anchor still requires a scanned DB — rejected up front, never
        // silently dropped — and nothing extra lands in the JSONL.
        let p = parse_args(&[
            "foo".to_string(),
            "requirement".to_string(),
            "R2".to_string(),
            "--anchor".to_string(),
            "github.com/x/y.Store".to_string(),
        ]);
        let err = add_requirement(&p, &apg_root, "foo", &mut records).unwrap_err();
        assert!(
            format!("{err:#}").contains("db.lbug"),
            "anchor must demand a scan: {err:#}"
        );
        let saved = specs::read_jsonl(&path).unwrap();
        assert!(saved.iter().all(|r| !matches!(
            r,
            Record::Requirement { fqn, .. } if fqn == "foo/spec.R2"
        )));

        // A depends-on (cross-project) target also resolves from the JSONL,
        // DB-less — the same way `spec link` does.
        specs::write_jsonl(
            &specs::spec_jsonl_path(&apg_root, "bar"),
            &[Record::Spec {
                fqn: "bar/spec".into(),
                title: "Bar".into(),
                goal: String::new(),
            }],
        )
        .unwrap();
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&[
            "foo".to_string(),
            "requirement".to_string(),
            "R3".to_string(),
            "--depends-on".to_string(),
            "bar/R9".to_string(),
        ]);
        // R9 doesn't exist yet in bar — a DB-less write-time error, not a
        // DB-dependent one.
        let err = add_requirement(&p, &apg_root, "foo", &mut records).unwrap_err();
        assert!(
            format!("{err:#}").contains("not an existing requirement"),
            "depends-on must resolve from JSONL: {err:#}"
        );
        // Seed bar/R9 and retry — lands DB-less.
        let mut bar = load_project(&apg_root, "bar").unwrap();
        let p = parse_args(&[
            "bar".to_string(),
            "requirement".to_string(),
            "R9".to_string(),
        ]);
        add_requirement(&p, &apg_root, "bar", &mut bar).unwrap();
        let p = parse_args(&[
            "foo".to_string(),
            "requirement".to_string(),
            "R3".to_string(),
            "--depends-on".to_string(),
            "bar/R9".to_string(),
        ]);
        add_requirement(&p, &apg_root, "foo", &mut records).unwrap();
        let saved = specs::read_jsonl(&path).unwrap();
        assert!(saved.iter().any(|r| matches!(
            r,
            Record::DependsOn { from, to } if from == "foo/spec.R3" && to == "bar/spec.R9"
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_requirement_with_anchor_lands_on_scanned_db() {
        // The DB-ful path: with a real db.lbug, an --anchor resolves against
        // the code graph and the Anchors edge survives the write-through.
        let (apg_root, dir) = fixture_layout("req-anchor");
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        specs::write_jsonl(
            &path,
            &[Record::Spec {
                fqn: "foo/spec".into(),
                title: "Foo".into(),
                goal: String::new(),
            }],
        )
        .unwrap();
        let mut records = load_project(&apg_root, "foo").unwrap();

        let p = parse_args(&[
            "foo".to_string(),
            "requirement".to_string(),
            "R1".to_string(),
            "--title".to_string(),
            "T".to_string(),
            "--anchor".to_string(),
            "github.com/x/y.Store".to_string(),
        ]);
        add_requirement(&p, &apg_root, "foo", &mut records).unwrap();

        let saved = specs::read_jsonl(&path).unwrap();
        assert!(saved.iter().any(|r| matches!(
            r,
            Record::Anchors { from, to }
                if from == "foo/spec.R1" && to == "github.com/x/y.Store"
        )));

        // Re-ingest merged the anchor: the live DB carries the edge.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query(
                "MATCH (:Requirement {fqn: 'foo/spec.R1'})-[:Anchors]->(:Struct {fqn: 'github.com/x/y.Store'}) RETURN count(*)",
            )
            .unwrap()
            .to_string();
        assert_eq!(out.lines().last(), Some("1"), "anchors edge: {out}");
        drop(db);

        // An unresolved anchor is rejected up front and nothing is written.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&[
            "foo".to_string(),
            "requirement".to_string(),
            "R2".to_string(),
            "--anchor".to_string(),
            "does.not.Exist".to_string(),
        ]);
        let err = add_requirement(&p, &apg_root, "foo", &mut records).unwrap_err();
        assert!(
            format!("{err:#}").contains("anchor target"),
            "unresolved anchor: {err:#}"
        );
        let saved = specs::read_jsonl(&path).unwrap();
        assert!(saved.iter().all(|r| !matches!(
            r,
            Record::Requirement { fqn, .. } if fqn == "foo/spec.R2"
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_depends_links_cross_project_specs() {
        let (apg_root, dir) = fixture_layout("xspec");
        let foo = vec![Record::Spec {
            fqn: "foo/spec".into(),
            title: "Foo".into(),
            goal: String::new(),
        }];
        let bar = vec![Record::Spec {
            fqn: "bar/spec".into(),
            title: "Bar".into(),
            goal: String::new(),
        }];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &foo).unwrap();
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "bar"), &bar).unwrap();

        // foo depends on bar (SpecDependsOn).
        let mut records = foo.clone();
        link_spec_depends(&apg_root, "foo", &["bar".into()], &mut records).unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::SpecDepends { from, to }
                if from == "foo/spec" && to == "bar/spec"
        )));

        // Self-dep and undeclared-spec targets are write-time errors.
        assert!(link_spec_depends(&apg_root, "foo", &["foo".into()], &mut records).is_err());
        assert!(link_spec_depends(&apg_root, "foo", &["baz".into()], &mut records).is_err());

        // Re-linking replaces, never duplicates.
        link_spec_depends(&apg_root, "foo", &["bar".into()], &mut records).unwrap();
        let deps: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::SpecDepends { from, to } if from == "foo/spec" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deps, vec!["bar/spec"]);

        // Cycle across projects: once bar → foo exists (in bar's file), foo →
        // bar would close bar → foo → bar.
        let mut bar_records = bar.clone();
        link_spec_depends(&apg_root, "bar", &["foo".into()], &mut bar_records).unwrap();
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "bar"), &bar_records).unwrap();
        let err = link_spec_depends(&apg_root, "foo", &["bar".into()], &mut records).unwrap_err();
        assert!(format!("{err:#}").contains("would create a cycle"));
        // The invalid edge was not persisted.
        assert!(!records.iter().any(|r| matches!(
            r,
            Record::SpecDepends { from, to }
                if from == "foo/spec" && to == "bar/spec"
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spine_sets_replace_per_kind_and_validate_pairs() {
        let (apg_root, dir) = fixture_layout("spine-cmd");
        let recs = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Auth".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/spec.R1".to_string(),
            },
            Record::Domain {
                fqn: "foo/domain.Auth".to_string(),
                name: "Auth".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/domain.Auth".to_string(),
            },
            Record::System {
                fqn: "foo/system.Platform".to_string(),
                name: "Platform".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/system.Platform".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // A Requirement drives a Domain by bare id; re-calling replaces the
        // outgoing Drives set (never duplicates).
        let p = parse_args(&[
            "foo".into(),
            "R1".into(),
            "--drives".into(),
            "foo/domain.Auth".into(),
        ]);
        apply_spine(&apg_root, "foo", "R1", &p).unwrap();
        let records = load_project(&apg_root, "foo").unwrap();
        let drives: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Drives { from, to } if from == "foo/spec.R1" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(drives, vec!["foo/domain.Auth"]);
        drop(records);

        // The domain realises the system.
        let p = parse_args(&[
            "foo".into(),
            "foo/domain.Auth".into(),
            "--realises".into(),
            "foo/system.Platform".into(),
        ]);
        apply_spine(&apg_root, "foo", "foo/domain.Auth", &p).unwrap();
        let records = load_project(&apg_root, "foo").unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Realises { from, to }
                if from == "foo/domain.Auth" && to == "foo/system.Platform"
        )));

        // Invalid kind pairs are rejected: a Requirement cannot Realises a
        // System (Realises is Domain → Solution), and Drives is Requirement →
        // Domain (a System source is rejected).
        let p = parse_args(&[
            "foo".into(),
            "R1".into(),
            "--realises".into(),
            "foo/system.Platform".into(),
        ]);
        let err = apply_spine(&apg_root, "foo", "R1", &p).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid source kind")
                || format!("{err:#}").contains("invalid kind pair"),
            "{err}"
        );
        let p = parse_args(&[
            "foo".into(),
            "foo/system.Platform".into(),
            "--drives".into(),
            "foo/domain.Auth".into(),
        ]);
        let err = apply_spine(&apg_root, "foo", "foo/system.Platform", &p).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid source kind")
                || format!("{err:#}").contains("invalid kind pair"),
            "{err}"
        );

        // A System implemented-by a code node: the code FQN must exist in the
        // graph. fixture_layout's graph has `github.com/x/y.Store` (a Struct).
        let p = parse_args(&[
            "foo".into(),
            "foo/system.Platform".into(),
            "--implemented-by".into(),
            "github.com/x/y.Store".into(),
        ]);
        apply_spine(&apg_root, "foo", "foo/system.Platform", &p).unwrap();
        let records = load_project(&apg_root, "foo").unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::ImplementedBy { from, to }
                if from == "foo/system.Platform" && to == "github.com/x/y.Store"
        )));
        drop(records);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spine_implemented_by_rejects_non_implementation_targets() {
        let (apg_root, dir) = fixture_layout("spine-implby");
        let recs = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::System {
                fqn: "foo/system.Platform".to_string(),
                name: "Platform".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/system.Platform".to_string(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // An UnresolvedTarget is a code node (code_label resolves) but has no
        // ImplementedBy rel-table pair — the edge would pass old CLI validation
        // and vanish at re-ingest. Rejected up front, and nothing is written.
        let p = parse_args(&[
            "foo".into(),
            "foo/system.Platform".into(),
            "--implemented-by".into(),
            "fmt.Errorf".into(),
        ]);
        let err = apply_spine(&apg_root, "foo", "foo/system.Platform", &p).unwrap_err();
        assert!(
            format!("{err:#}").contains("UnresolvedTarget")
                && format!("{err:#}").contains("not implementation code"),
            "{err}"
        );
        let records = load_project(&apg_root, "foo").unwrap();
        assert!(
            !records.iter().any(|r| matches!(
                r,
                Record::ImplementedBy { from, to }
                    if from == "foo/system.Platform" && to == "fmt.Errorf"
            )),
            "rejected edge must not reach the JSONL"
        );
        drop(records);

        // A spec node is likewise not implementation code.
        let p = parse_args(&[
            "foo".into(),
            "foo/system.Platform".into(),
            "--implemented-by".into(),
            "foo/system.Platform".into(),
        ]);
        let err = apply_spine(&apg_root, "foo", "foo/system.Platform", &p).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a resolved code FQN")
                || format!("{err:#}").contains("not implementation code"),
            "{err}"
        );

        // A real implementation code node still lands.
        let p = parse_args(&[
            "foo".into(),
            "foo/system.Platform".into(),
            "--implemented-by".into(),
            "github.com/x/y.Store".into(),
        ]);
        apply_spine(&apg_root, "foo", "foo/system.Platform", &p).unwrap();
        let records = load_project(&apg_root, "foo").unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::ImplementedBy { from, to }
                if from == "foo/system.Platform" && to == "github.com/x/y.Store"
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn depends_on_crosses_projects() {
        let (apg_root, dir) = fixture_layout("xreq");
        let foo = vec![
            Record::Spec {
                fqn: "foo/spec".into(),
                title: "Foo".into(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".into(),
                id: "R1".into(),
                title: "t".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/spec.R1".into(),
            },
        ];
        let bar = vec![
            Record::Spec {
                fqn: "bar/spec".into(),
                title: "Bar".into(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "bar/spec.R2".into(),
                id: "R2".into(),
                title: "t".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "bar/spec".into(),
                to: "bar/spec.R2".into(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &foo).unwrap();
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "bar"), &bar).unwrap();

        // foo.R1 depends on bar.R2 (cross-project DependsOn).
        let mut records = foo.clone();
        link_depends_on(&apg_root, "foo", "R1", &["bar/R2".into()], &mut records).unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::DependsOn { from, to }
                if from == "foo/spec.R1" && to == "bar/spec.R2"
        )));

        // Unknown cross-project target rejected; unknown project prefix is
        // treated as a (rejected) same-project id.
        assert!(link_depends_on(&apg_root, "foo", "R1", &["bar/R9".into()], &mut records).is_err());
        assert!(link_depends_on(&apg_root, "foo", "R1", &["baz/R2".into()], &mut records).is_err());

        // Cross-project cycle: bar.R2 → foo.R1 (persisted), then foo.R1 →
        // bar.R2 would close foo.R1 → bar.R2 → foo.R1.
        let mut bar_records = bar.clone();
        link_depends_on(&apg_root, "bar", "R2", &["foo/R1".into()], &mut bar_records).unwrap();
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "bar"), &bar_records).unwrap();
        let err =
            link_depends_on(&apg_root, "foo", "R1", &["bar/R2".into()], &mut records).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("would create a cycle"), "got: {msg}");
        // The invalid edge was not persisted.
        assert!(!records.iter().any(|r| matches!(
            r,
            Record::DependsOn { from, to }
                if from == "foo/spec.R1" && to == "bar/spec.R2"
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dep_target_parses_cross_project() {
        let (apg_root, dir) = fixture_layout("dparse");
        specs::write_jsonl(
            &specs::spec_jsonl_path(&apg_root, "bar"),
            &[Record::Spec {
                fqn: "bar/spec".into(),
                title: "Bar".into(),
                goal: String::new(),
            }],
        )
        .unwrap();
        assert_eq!(
            dep_target("R4", "foo", &apg_root),
            ("foo".to_string(), "R4".to_string())
        );
        assert_eq!(
            dep_target("bar/R2", "foo", &apg_root),
            ("bar".to_string(), "R2".to_string())
        );
        // An unknown project prefix is not a known spec — stays a same-project id.
        assert_eq!(
            dep_target("baz/R2", "foo", &apg_root),
            ("foo".to_string(), "baz/R2".to_string())
        );
        // Explicit same-project prefix.
        assert_eq!(
            dep_target("foo/R1", "foo", &apg_root),
            ("foo".to_string(), "R1".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tier_nodes_and_spine_author_roundtrip() {
        let (apg_root, dir) = fixture_layout("spine-author");
        let recs = vec![Record::Spec {
            fqn: "foo/spec".to_string(),
            title: "Foo".to_string(),
            goal: String::new(),
        }];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // Author a Domain node hanging under the spec root.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&["foo".into(), "domain".into(), "Auth".into(), "--body".into(), "the auth area".into()]);
        add_tier_node(
            &p,
            "foo",
            "domain",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Domain { fqn, name, body },
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Domain { fqn, name, .. }
                if fqn == "foo/domain.Auth" && name == "Auth"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/spec" && to == "foo/domain.Auth"
        )));

        // A subdomain hanging under the domain (the DDD hierarchy).
        let p = parse_args(&[
            "foo".into(),
            "subdomain".into(),
            "Access".into(),
            "--kind".into(),
            "core".into(),
            "--parent".into(),
            "foo/domain.Auth".into(),
        ]);
        add_tier_node(
            &p,
            "foo",
            "subdomain",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Subdomain {
                fqn,
                name,
                kind: "core".into(),
                body,
            },
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Contains { from, to }
                if from == "foo/domain.Auth" && to == "foo/subdomain.Access"
        )));

        // An aggregate under the subdomain with a root.
        let p = parse_args(&[
            "foo".into(),
            "aggregate".into(),
            "Session".into(),
            "--root".into(),
            "Session".into(),
            "--parent".into(),
            "foo/subdomain.Access".into(),
        ]);
        add_tier_node(
            &p,
            "foo",
            "aggregate",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Aggregate {
                fqn,
                name,
                root: "Session".into(),
                body,
            },
        )
        .unwrap();

        // A System + Container + Component solution tier.
        let p = parse_args(&["foo".into(), "system".into(), "Platform".into()]);
        add_tier_node(
            &p,
            "foo",
            "system",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::System { fqn, name, body },
        )
        .unwrap();
        let p = parse_args(&[
            "foo".into(),
            "container".into(),
            "Api".into(),
            "--kind".into(),
            "app".into(),
            "--parent".into(),
            "foo/system.Platform".into(),
        ]);
        add_tier_node(
            &p,
            "foo",
            "container",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Container {
                fqn,
                name,
                kind: "app".into(),
                body,
            },
        )
        .unwrap();

        // A requirement to anchor the spine.
        let req_fqn = "foo/spec.R1";
        records.push(Record::Requirement {
            fqn: req_fqn.to_string(),
            id: "R1".to_string(),
            title: "Auth".to_string(),
            body: String::new(),
            feature: String::new(),
        });
        records.push(Record::Contains {
            from: "foo/spec".to_string(),
            to: req_fqn.to_string(),
        });
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &records).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contains_hierarchy_rejects_domain_contains_aggregate() {
        // GraphModel-SPEC's DDD chain is strict: `Domain ⊃ Subdomain ⊃
        // Aggregate ⊃ Entity/ValueObject`. A direct `Domain ⊃ Aggregate` is
        // not a valid Contains pair — the schema (`load::contains_pair_allowed`)
        // excludes it, and the authoring path (`add_tier_node --parent`)
        // rejects it up front instead of writing an edge that would silently
        // vanish at re-ingest. The Spec-root attachments the model requires
        // (Stakeholder/Domain/System) stay valid.
        assert!(!load::contains_pair_allowed(NodeKind::Domain, NodeKind::Aggregate));
        assert!(load::contains_pair_allowed(NodeKind::Subdomain, NodeKind::Aggregate));
        assert!(load::contains_pair_allowed(NodeKind::Aggregate, NodeKind::Entity));
        assert!(load::contains_pair_allowed(NodeKind::Aggregate, NodeKind::ValueObject));
        assert!(load::contains_pair_allowed(NodeKind::Spec, NodeKind::Stakeholder));
        assert!(load::contains_pair_allowed(NodeKind::Spec, NodeKind::Domain));
        assert!(load::contains_pair_allowed(NodeKind::Spec, NodeKind::System));

        let (apg_root, dir) = fixture_layout("contains-hierarchy");
        let recs = vec![
            Record::Spec {
                fqn: "foo/spec".into(),
                title: "Foo".into(),
                goal: String::new(),
            },
            Record::Domain {
                fqn: "foo/domain.Auth".into(),
                name: "Auth".into(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/domain.Auth".into(),
            },
        ];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // `aggregate Session --parent foo/domain.Auth` is rejected BEFORE any
        // record is pushed — the in-memory records are untouched.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let before = records.len();
        let p = parse_args(&[
            "foo".into(),
            "aggregate".into(),
            "Session".into(),
            "--parent".into(),
            "foo/domain.Auth".into(),
        ]);
        let err = add_tier_node(
            &p,
            "foo",
            "aggregate",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Aggregate {
                fqn,
                name,
                root: String::new(),
                body,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cannot hang under a `Domain` node"), "{msg}");
        assert_eq!(records.len(), before, "rejected pair must not touch records");

        // The legal chain still authors: a Subdomain hangs under the Domain.
        let p = parse_args(&[
            "foo".into(),
            "subdomain".into(),
            "Access".into(),
            "--parent".into(),
            "foo/domain.Auth".into(),
        ]);
        add_tier_node(
            &p,
            "foo",
            "subdomain",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Subdomain {
                fqn,
                name,
                kind: String::new(),
                body,
            },
        )
        .unwrap();
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Subdomain { fqn, .. } if fqn == "foo/subdomain.Access"
        )));

        // A JSONL carrying `Domain ⊃ Aggregate` re-ingests with the edge
        // dropped (the schema no longer declares the pair) — it cannot land.
        let mut loose = load_project(&apg_root, "foo").unwrap();
        loose.push(Record::Aggregate {
            fqn: "foo/aggregate.Session".into(),
            name: "Session".into(),
            root: "Session".into(),
            body: String::new(),
        });
        loose.push(Record::Contains {
            from: "foo/domain.Auth".into(),
            to: "foo/aggregate.Session".into(),
        });
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &loose).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query(
                "MATCH (d:Domain {fqn: 'foo/domain.Auth'})-[:Contains]->(a:Aggregate {fqn: 'foo/aggregate.Session'}) RETURN count(*)",
            )
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("0"),
            "Domain ⊃ Aggregate must not land: {out}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn domain_rule_materializes_product_invariant() {
        let (apg_root, dir) = fixture_layout("domain-rule");
        let recs = vec![Record::Spec {
            fqn: "foo/spec".to_string(),
            title: "Foo".to_string(),
            goal: String::new(),
        }];
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // Author a domain rule: the DomainRule node AND a project-scoped
        // Invariant (category=product) land together. A rule hangs under its
        // Domain (the DDD hierarchy), never under the Spec root.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let domain_p = parse_args(&["foo".into(), "domain".into(), "Auth".into()]);
        add_tier_node(
            &domain_p,
            "foo",
            "domain",
            "foo/spec",
            &mut records,
            |fqn, name, body| Record::Domain { fqn, name, body },
        )
        .unwrap();
        let p = parse_args(&[
            "foo".into(),
            "domain-rule".into(),
            "NoNegativeBalance".into(),
            "--body".into(),
            "A balance never goes below zero".into(),
            "--parent".into(),
            "foo/domain.Auth".into(),
        ]);
        let spec_fqn = "foo/spec".to_string();
        // The full arm lives in `spec_add`; here we mirror the two pushes to
        // assert the alignment invariant (DomainRule → Invariant).
        add_tier_node(
            &p,
            "foo",
            "domain-rule",
            &spec_fqn,
            &mut records,
            |fqn, name, body| Record::DomainRule { fqn, name, body },
        )
        .unwrap();
        let ifqn = "foo/invariant/NoNegativeBalance".to_string();
        records.push(Record::Invariant {
            fqn: ifqn.clone(),
            title: "NoNegativeBalance".to_string(),
            body: "A balance never goes below zero".to_string(),
            category: "product".to_string(),
            scope: "code".to_string(),
            status: "active".to_string(),
        });
        assert!(records.iter().any(|r| matches!(
            r,
            Record::DomainRule { fqn, name, .. }
                if fqn == "foo/domain-rule.NoNegativeBalance" && name == "NoNegativeBalance"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, category, status, .. }
                if fqn == &ifqn && category == "product" && status == "active"
        )));

        // Both land in the DB after a re-ingest, and the rule hangs under its
        // Domain (previously the `Spec ⊃ DomainRule` default parent was not a
        // valid Contains pair, so the edge was silently projected away).
        specs::write_jsonl(&specs::spec_jsonl_path(&apg_root, "foo"), &records).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/domain-rule.NoNegativeBalance"));
        assert!(db.has_node("foo/invariant/NoNegativeBalance"));
        let out = db
            .conn()
            .unwrap()
            .query(
                "MATCH (d:Domain {fqn: 'foo/domain.Auth'})-[:Contains]->(r:DomainRule {fqn: 'foo/domain-rule.NoNegativeBalance'}) RETURN r.fqn",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/domain-rule.NoNegativeBalance"),
            "domain-rule must hang under its Domain, not be orphaned: {out}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_note_rejects_note_and_feedback_targets_before_any_write() {
        // R2 (task 2.2): `--on` a Note or a Feedback target must be rejected
        // with a clear CLI message BEFORE any JSONL write or DB re-ingest —
        // both are excluded from the DB's Details rel-table targets.
        let (apg_root, dir) = fixture_layout("r2-validation");
        let recs = vec![
            Record::Spec {
                fqn: "foo/spec".into(),
                title: "Foo".into(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".into(),
                id: "R1".into(),
                title: "t".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".into(),
                to: "foo/spec.R1".into(),
            },
            Record::Note {
                fqn: "foo/note-1".into(),
                body: "existing".into(),
                kind: "background".into(),
            },
            Record::Details {
                from: "foo/note-1".into(),
                to: "foo/spec".into(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "review".into(),
                status: "open".into(),
                disposition: String::new(),
            },
        ];
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        specs::write_jsonl(&path, &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // --on a Note target: clear CLI rejection, no write.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&[
            "--body".into(),
            "n".into(),
            "--on".into(),
            "foo/note-1".into(),
        ]);
        let err = add_note(&p, &apg_root, "foo", &mut records).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("note target `foo/note-1` is a `Note` node"),
            "{msg}"
        );
        assert!(msg.contains("may only attach"), "{msg}");

        // --on a Feedback target: rejected too (Feedback is not a Details target).
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&[
            "--body".into(),
            "n".into(),
            "--on".into(),
            "foo/feedback-1".into(),
        ]);
        let err = add_note(&p, &apg_root, "foo", &mut records).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("note target `foo/feedback-1` is a `Feedback` node"),
            "{msg}"
        );

        // JSONL unchanged and the live DB carries no new Note nodes.
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (n:Note) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "no new note: {out}");
        drop(db);

        // An allowable target (Spec) still writes through.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&[
            "--body".into(),
            "ok".into(),
            "--on".into(),
            "foo/spec".into(),
        ]);
        add_note(&p, &apg_root, "foo", &mut records).unwrap();
        let after = specs::read_jsonl(&path).unwrap();
        assert!(after.iter().any(|r| {
            matches!(r, Record::Details { from, to }
                if to == "foo/spec" && from != "foo/note-1")
        }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_note_attaches_to_spec_and_decision_in_any_project() {
        // R4 (task 2.6): the mystery commands — `--on` a Spec and a Decision
        // target — succeed without a binder error, in any project.
        let (apg_root, dir) = fixture_layout("r4-binder");
        let mk = |project: &str, decisions: &[&str]| -> Vec<Record> {
            let mut v = vec![Record::Spec {
                fqn: format!("{project}/spec"),
                title: project.into(),
                goal: String::new(),
            }];
            for d in decisions {
                let fqn = format!("{project}/spec.decision-{d}");
                v.push(Record::Decision {
                    fqn: fqn.clone(),
                    id: (*d).into(),
                    summary: "s".into(),
                });
                v.push(Record::Contains {
                    from: format!("{project}/spec"),
                    to: fqn,
                });
            }
            v
        };
        for (project, decisions) in [
            ("cosanima-rename", &["D1"][..]),
            ("cosanima-mcp", &[][..]),
            ("cosanima-1.0", &["D5"][..]),
        ] {
            let recs = mk(project, decisions);
            let path = specs::spec_jsonl_path(&apg_root, project);
            specs::write_jsonl(&path, &recs).unwrap();
            artifacts::reingest_project(&apg_root, project).unwrap();
        }

        // A note attaches to each project's Spec node.
        for (i, project) in ["cosanima-rename", "cosanima-mcp", "cosanima-1.0"]
            .into_iter()
            .enumerate()
        {
            let target = format!("{project}/spec");
            let mut records = load_project(&apg_root, project).unwrap();
            let p = parse_args(&[
                "--body".into(),
                format!("note-{i}"),
                "--on".into(),
                target.clone(),
            ]);
            add_note(&p, &apg_root, project, &mut records).unwrap();
            let after = specs::read_jsonl(&specs::spec_jsonl_path(&apg_root, project)).unwrap();
            assert!(after.iter().any(|r| {
                matches!(r, Record::Details { from, to }
                    if to == &target
                        && from.starts_with(&format!("{project}/note-")))
            }));
            let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
            let out = db
                .conn()
                .unwrap()
                .query(&format!(
                    "MATCH (:Note)-[:Details]->(s {{fqn: {}}}) RETURN count(*)",
                    artifacts::lit(&target)
                ))
                .unwrap()
                .to_string();
            assert!(
                out.lines().last() == Some("1"),
                "{project} spec note must land: {out}"
            );
            drop(db);
        }

        // The exact rename Decision target from the mystery.
        let mut records = load_project(&apg_root, "cosanima-rename").unwrap();
        let p = parse_args(&[
            "--body".into(),
            "on-d1".into(),
            "--on".into(),
            "cosanima-rename/spec.decision-D1".into(),
        ]);
        add_note(&p, &apg_root, "cosanima-rename", &mut records).unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note)-[:Details]->(:Decision {fqn: 'cosanima-rename/spec.decision-D1'}) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "rename D1 note must land: {out}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_note_attaches_to_tier_nodes() {
        // REVIEW.md item: `apg spec add note --on <domain-fqn>` was explicitly
        // rejected because the Details rel-table excluded the tier labels —
        // the prose-narrative Notes mechanism was unavailable on Domain/Solution
        // nodes even though tiers 1-3 are the prose-heavy part of the proposed
        // reality. The rel-table now declares Note → every tier label; the
        // edge survives a tier-2 and a tier-3 target.
        let (apg_root, dir) = fixture_layout("note-tier");

        // A spec project carrying a Domain (tier-2) and a System (tier-3) node.
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/domain.X".to_string(),
            },
            Record::Domain {
                fqn: "foo/domain.X".to_string(),
                name: "X".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/system.Y".to_string(),
            },
            Record::System {
                fqn: "foo/system.Y".to_string(),
                name: "Y".to_string(),
                body: String::new(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();

        // A note on the tier-2 Domain node, then one on the tier-3 System node.
        let mut records = load_project(&apg_root, "foo").unwrap();
        let p = parse_args(&[
            "--body".into(),
            "domain note".into(),
            "--on".into(),
            "foo/domain.X".into(),
        ]);
        add_note(&p, &apg_root, "foo", &mut records).unwrap();
        let p = parse_args(&[
            "--body".into(),
            "system note".into(),
            "--on".into(),
            "foo/system.Y".into(),
        ]);
        add_note(&p, &apg_root, "foo", &mut records).unwrap();

        // The Note nodes round-trip WITH their Details edges — pre-fix these
        // were rejected up front ("note target `foo/domain.X` is a `Domain`
        // node").
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note)-[:Details]->(s {fqn: 'foo/domain.X'}) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "tier-2 note must land: {out}"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note)-[:Details]->(s {fqn: 'foo/system.Y'}) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "tier-3 note must land: {out}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_spec_shows_tiers_and_spine() {
        // REVIEW.md item: the one markdown projection of a spec omitted tiers
        // 2-3 (Domain/Solution nodes) and every spine edge, so the 4-tier
        // structure and the why-to-code chain were invisible. The render now
        // emits Stakeholders (tier 1), the Domain tree (tier 2), the Solution
        // tree (tier 3), and the Spine edges.
        let (apg_root, dir) = fixture_layout("render-tiers");
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: "A goal".to_string(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Auth".to_string(),
                body: "users authenticate".to_string(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/spec.R1".to_string(),
            },
            Record::Stakeholder {
                fqn: "foo/stakeholder.Ops".to_string(),
                name: "Ops".to_string(),
                body: "runs it".to_string(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/stakeholder.Ops".to_string(),
            },
            Record::Domain {
                fqn: "foo/domain.Auth".to_string(),
                name: "Auth".to_string(),
                body: "the auth area".to_string(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/domain.Auth".to_string(),
            },
            Record::Subdomain {
                fqn: "foo/subdomain.Access".to_string(),
                name: "Access".to_string(),
                kind: "core".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/domain.Auth".to_string(),
                to: "foo/subdomain.Access".to_string(),
            },
            Record::System {
                fqn: "foo/system.Platform".to_string(),
                name: "Platform".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/system.Platform".to_string(),
            },
            Record::Container {
                fqn: "foo/container.Api".to_string(),
                name: "Api".to_string(),
                kind: "app".to_string(),
                body: String::new(),
            },
            Record::Contains {
                from: "foo/system.Platform".to_string(),
                to: "foo/container.Api".to_string(),
            },
            Record::Drives {
                from: "foo/spec.R1".to_string(),
                to: "foo/domain.Auth".to_string(),
            },
            Record::Realises {
                from: "foo/domain.Auth".to_string(),
                to: "foo/system.Platform".to_string(),
            },
            Record::ImplementedBy {
                from: "foo/container.Api".to_string(),
                to: "github.com/x/y.Store".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();

        let md = render_spec(&records, &db).unwrap();
        drop(db);

        // Tier 1 stakeholders.
        assert!(md.contains("## Stakeholders"), "stakeholders section: {md}");
        assert!(md.contains("**`Ops`** — runs it"), "stakeholder body: {md}");
        // Tier 2 Domain tree, nested by Contains with metadata.
        assert!(md.contains("## Domain"), "domain section: {md}");
        assert!(md.contains("**Domain `Auth`**"), "domain node: {md}");
        assert!(md.contains("the auth area"), "domain body: {md}");
        assert!(md.contains("**Subdomain `Access`** (kind: core)"), "subdomain + kind: {md}");
        assert!(md.contains("  - **Subdomain"), "subdomain indented under domain: {md}");
        // Tier 3 Solution tree.
        assert!(md.contains("## Solution"), "solution section: {md}");
        assert!(md.contains("**System `Platform`**"), "system node: {md}");
        assert!(md.contains("**Container `Api`** (kind: app)"), "container + kind: {md}");
        assert!(md.contains("  - **Container"), "container indented under system: {md}");
        // Spine edges, in short FQN form.
        assert!(md.contains("## Spine"), "spine section: {md}");
        assert!(
            md.contains("`spec.R1` --Drives--> `domain.Auth`"),
            "drives edge: {md}"
        );
        assert!(
            md.contains("`domain.Auth` --Realises--> `system.Platform`"),
            "realises edge: {md}"
        );
        assert!(
            md.contains("`container.Api` --ImplementedBy--> `github.com/x/y.Store`"),
            "implemented-by edge (code fqn passes through): {md}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_unresolved_lints_orphans_and_planned_nodes() {
        // A spec with an orphan requirement (no Satisfies/Implements), a
        // satisfiable future (target code `github.com/x/y.Store` exists in the
        // fixture DB), an unsatisfiable future, and an open feedback — the
        // `apg spec unresolved` CLI subcommand should report each section.
        let (apg_root, dir) = fixture_layout("unresolved");
        let records = vec![
            Record::Spec { fqn: "foo/spec".into(), title: "T".into(), goal: String::new() },
            Record::Requirement { fqn: "foo/spec.R1".into(), id: "R1".into(), title: "Timer".into(), body: String::new(), feature: String::new() },
            Record::Requirement { fqn: "foo/spec.R2".into(), id: "R2".into(), title: "Delivered".into(), body: String::new(), feature: String::new() },
            Record::Anchors { from: "foo/spec.R1".into(), to: "github.com/x/y.Gateway".into() },
            Record::Implements { from: "github.com/x/y.Store".into(), to: "foo/spec.R2".into() },
            Record::Feedback { fqn: "foo/feedback-1".into(), body: "b".into(), status: "open".into(), disposition: String::new() },
        ];
        specs::write_jsonl(&apg_root.join("specs").join("foo.jsonl"), &records).unwrap();
        // The plan carries the planned tier-4 additions: one that the DB has
        // realized (Store is real code) and one still unbuilt (Gateway).
        let plan_dir = apg_root.join(specs::TRANS).join("plans");
        std::fs::create_dir_all(&plan_dir).unwrap();
        let plan = vec![
            Record::Plan { fqn: "foo/plan".into(), title: "P".into(), strategy: String::new() },
            Record::PlanPhase { fqn: "foo/plan.phase-01".into(), number: 1, title: "P1".into(), deliverable: String::new() },
            Record::Task { fqn: "foo/plan.phase-01.task-1".into(), title: "t".into(), kind: "source".into(), tier: String::new(), status: "pending".into() },
            Record::PlannedNode { fqn: "github.com/x/y.Store".into(), kind: "struct".into(), name: "Store".into(), parent: String::new() },
            Record::PlannedNode { fqn: "github.com/x/y.Gateway".into(), kind: "struct".into(), name: "Gateway".into(), parent: String::new() },
            Record::Contains { from: "foo/plan".into(), to: "foo/plan.phase-01".into() },
            Record::Contains { from: "foo/plan.phase-01".into(), to: "foo/plan.phase-01.task-1".into() },
            Record::Builds { from: "foo/plan.phase-01.task-1".into(), to: "github.com/x/y.Store".into() },
        ];
        specs::write_jsonl(&plan_dir.join("foo.jsonl"), &plan).unwrap();

        let out = spec_unresolved_report(&apg_root, Some("foo")).unwrap();
        // Orphan R1 (no Satisfies, no Implements); R2 is delivered, not orphan.
        assert!(out.contains("orphan requirements — no Satisfies, no Implements (1):"), "orphan: {out}");
        assert!(out.contains("  R1 — Timer"), "orphan line: {out}");
        assert!(!out.contains("R2"), "delivered requirement must not be an orphan: {out}");
        // Store is realized (a scan replaced it); Gateway is still planned.
        assert!(out.contains("realized planned nodes"), "realized: {out}");
        assert!(out.contains("github.com/x/y.Store"), "realized line: {out}");
        assert!(out.contains("unbuilt planned code"), "unbuilt: {out}");
        assert!(out.contains("github.com/x/y.Gateway"), "unbuilt line: {out}");
        // Gateway is unreferenced (no task Builds it).
        assert!(out.contains("unreferenced planned nodes"), "unreferenced: {out}");
        // Pending anchor to the planned Gateway node is expected.
        assert!(out.contains("pending anchors"), "pending anchor: {out}");
        assert!(out.contains("github.com/x/y.Gateway"), "pending anchor line: {out}");
        // Open feedback surfaced.
        assert!(out.contains("feedback under review"), "feedback: {out}");
        assert!(out.contains("foo/feedback-1 (open)"), "feedback line: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_unresolved_clean_and_filter_and_missing_db() {
        // A delivered-only spec (every requirement Satisfied/Implements'd) lints
        // clean; filtering to an unknown project reports nothing; a DB-less
        // layout still lints (planned-node realization unverifiable, not a
        // crash).
        let (apg_root, dir) = fixture_layout("unresolved-clean");
        let records = vec![
            Record::Spec { fqn: "foo/spec".into(), title: "T".into(), goal: String::new() },
            Record::Requirement { fqn: "foo/spec.R1".into(), id: "R1".into(), title: "A".into(), body: String::new(), feature: String::new() },
            Record::Satisfies { from: "foo/plan.phase-01".into(), to: "foo/spec.R1".into() },
        ];
        specs::write_jsonl(&apg_root.join("specs").join("foo.jsonl"), &records).unwrap();
        let plan_dir = apg_root.join(specs::TRANS).join("plans");
        std::fs::create_dir_all(&plan_dir).unwrap();
        let plan = vec![
            Record::Plan { fqn: "foo/plan".into(), title: "P".into(), strategy: String::new() },
            Record::PlanPhase { fqn: "foo/plan.phase-01".into(), number: 1, title: "P1".into(), deliverable: String::new() },
            Record::Task { fqn: "foo/plan.phase-01.task-1".into(), title: "t".into(), kind: "source".into(), tier: String::new(), status: "pending".into() },
            Record::PlannedNode { fqn: "github.com/x/y.Store".into(), kind: "struct".into(), name: "Store".into(), parent: String::new() },
            Record::Contains { from: "foo/plan".into(), to: "foo/plan.phase-01".into() },
            Record::Contains { from: "foo/plan.phase-01".into(), to: "foo/plan.phase-01.task-1".into() },
            Record::Builds { from: "foo/plan.phase-01.task-1".into(), to: "github.com/x/y.Store".into() },
        ];
        specs::write_jsonl(&plan_dir.join("foo.jsonl"), &plan).unwrap();

        // All projects — every requirement covered, no orphans; the planned
        // Store is realized (its FQN is real code in the DB) and Builds'd, so
        // the lint is clean (no findings).
        let out = spec_unresolved_report(&apg_root, None).unwrap();
        assert!(
            out.contains("Lint clean"),
            "realized Store leaves nothing unresolved: {out}"
        );
        assert!(!out.contains("orphan requirements"), "clean must not report orphans: {out}");
        assert!(!out.contains("unbuilt planned code"), "store realized, nothing unbuilt: {out}");
        assert!(!out.contains("unreferenced planned nodes"), "store is Builds'd, nothing unreferenced: {out}");

        // Filtering to a project that does not exist reports nothing.
        let filtered = spec_unresolved_report(&apg_root, Some("nope")).unwrap();
        assert!(filtered.is_empty() || !filtered.contains("nope"), "filter: {filtered}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
