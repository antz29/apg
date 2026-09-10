//! `apg review` — the closed writer↔reviewer feedback cycle (SPEC R25/R26).
//! A reviewer attaches a `Feedback` (`open`); a writer actions or wont-fixes it
//! (`actioned`); the reviewer then resolves (terminal) or rejects (reopens).
//! The writer cannot resolve and the reviewer cannot action — enforced by tool
//! permissions (R28), never by convention.
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
    artifacts::acquire_spec_lock(&apg_root)?;
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
fn apply_review_add(apg_root: &Path, p: &ParsedArgs) -> anyhow::Result<()> {
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
fn set_feedback_at(
    apg_root: &Path,
    fqn: &str,
    project: &str,
    status: &str,
    disposition: Option<String>,
) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::load;
    use crate::testutil::{self, Repo};
    use lbug::{Connection, Database};

    /// A temp repo with a real project context for `foo` (R4 — non-git
    /// fixtures are gone): the worktree on branch `foo` carries a real DB
    /// with a minimal code graph and a fresh scan_meta.
    fn fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
        let repo = Repo::new(&format!("review-{name}"));
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

        // A durable layer node (SPEC §3.1): a requirement under
        // `apg/layers/requirements/requirement/`, FQN without a project
        // prefix. Durable-node reviews resolve against it in the graph.
        g.nodes.insert(
            "requirements.requirement.timer".to_string(),
            Node {
                kind: NodeKind::Requirement,
                ..Node::default()
            },
        );

        // One durable node per remaining file-backed tier (SPEC §3.1) — a
        // domain Entity, a solution System, a global Constraint — so reviews
        // of every tier route to their own `.trans` mirror.
        g.nodes.insert(
            "domain.entity.order".to_string(),
            Node {
                kind: NodeKind::Entity,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "solution.system.checkout".to_string(),
            Node {
                kind: NodeKind::System,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "global.constraint.law".to_string(),
            Node {
                kind: NodeKind::Constraint,
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

    #[test]
    fn wont_fix_is_not_terminal_until_reviewer_resolves() {
        let (apg_root, repo, _wt) = fixture("wont-fix");

        // A transient plan with a task, plus an open Feedback reviewing it.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
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
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        // Seeded through the funnel (auto-committed on the branch, DB fresh).
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();

        // Writer actions with --wont-fix: a *proposal*, not terminal.
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "actioned",
            Some("wont-fix".to_string()),
        )
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        let f = recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback {
                    fqn,
                    status,
                    disposition,
                    ..
                } if fqn == "foo/feedback-1" => Some((status.clone(), disposition.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, ("actioned".to_string(), "wont-fix".to_string()));

        // A wont-fix actioned item is NOT terminal: the reviewer must approve.
        // Reject reopens it (writer must rework).
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "open",
            Some("rejected".to_string()),
        )
        .unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        let f = recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback {
                    fqn,
                    status,
                    disposition,
                    ..
                } if fqn == "foo/feedback-1" => Some((status.clone(), disposition.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, ("open".to_string(), "rejected".to_string()));

        // The reviewer may instead approve (resolve) — terminal.
        set_feedback_at(&apg_root, "foo/feedback-1", "foo", "resolved", None).unwrap();
        let recs = specs::read_jsonl(&path).unwrap();
        let f = recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback { fqn, status, .. } if fqn == "foo/feedback-1" => {
                    Some(status.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(f, "resolved");

        testutil::remove(&repo);
    }

    #[test]
    fn first_review_without_transient_files_numbers_feedback_1() {
        // Numbering quirk fix (note-27): the shared `<project>/feedback-<n>`
        // namespace starts at 1 even when NO transient file exists yet — a
        // plan-less project reviewing a durable/code node. The first review
        // must land `foo/feedback-1` (never `feedback-0`); a second review
        // lands `foo/feedback-2` (the counter is shared across mirrors).
        let (apg_root, repo, _wt) = fixture("first-number");

        // The fixture starts with no plan store and no tier mirrors at all.
        for f in specs::project_transient_files(&apg_root, "foo") {
            assert!(
                !f.exists(),
                "fixture must start with no transient file: {}",
                f.display()
            );
        }

        // First review: a code node → the implementation tier mirror.
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // Second review: a durable node → the requirements tier mirror.
        let p = parse_args(&[
            "requirements.requirement.timer".to_string(),
            "--body".to_string(),
            "req review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // The mirrors carry feedback-1 then feedback-2 — no 0 slot.
        let impl_mirror = apg_root
            .join(specs::TRANS)
            .join("implementation")
            .join("foo.jsonl");
        let req_mirror = apg_root
            .join(specs::TRANS)
            .join("requirements")
            .join("foo.jsonl");
        let impl_recs = specs::read_jsonl(&impl_mirror).unwrap();
        let req_recs = specs::read_jsonl(&req_mirror).unwrap();
        assert!(
            impl_recs.iter().any(|r| matches!(
                r,
                Record::Feedback { fqn, .. } if fqn == "foo/feedback-1"
            )),
            "the first review of a plan-less project must number feedback-1"
        );
        assert!(
            req_recs.iter().any(|r| matches!(
                r,
                Record::Feedback { fqn, .. } if fqn == "foo/feedback-2"
            )),
            "the second review must number feedback-2"
        );

        // Both round-trip into the branch DB; the 0 slot never exists.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-1"), "feedback-1 node dropped");
        assert!(db.has_node("foo/feedback-2"), "feedback-2 node dropped");
        assert!(
            !db.has_node("foo/feedback-0"),
            "feedback-0 must never be created"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("github.com/x/y.Store"),
            "feedback-1 reviews edge dropped: {out}"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-2'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("requirements.requirement.timer"),
            "feedback-2 reviews edge dropped: {out}"
        );
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    fn review_add_roundtrips_reviews_edge_to_task_and_code() {
        // `apg review add <target>` attaches an open Feedback with a Reviews
        // edge; the edge survives the re-ingest for both a plan/task target
        // and a code target (which needs an explicit --project). Both halves
        // land in `.trans` — the plan store for the task, the implementation
        // tier mirror for the code node.
        let (apg_root, repo, _wt) = fixture("roundtrip");

        // A transient plan with one task.
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
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
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();

        // Review the task (a plan target — routes to the transient plan
        // store, co-located with the plan records).
        let p = parse_args(&[
            "foo/plan.phase-01.task-1".to_string(),
            "--body".to_string(),
            "task review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // Review a code node (needs --project — routes to the implementation
        // tier mirror, NOT the plan file).
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // The Feedback nodes round-trip WITH their Reviews edges.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-1"), "feedback-1 node dropped");
        assert!(db.has_node("foo/feedback-2"), "feedback-2 node dropped");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan.phase-01.task-1"),
            "task reviews edge dropped: {out}"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-2'})-[:Reviews]->(n:Struct) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("github.com/x/y.Store"),
            "code reviews edge dropped: {out}"
        );
        drop(db);

        // Both halves live in `.trans`: the task review in the plan store,
        // the code review in the implementation tier mirror — and the legacy
        // durable stores never exist.
        let task_recs = specs::read_jsonl(&specs::plan_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(
            task_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "foo/plan.phase-01.task-1"
            )),
            "task review must co-locate with the plan records"
        );
        let impl_mirror = apg_root
            .join(specs::TRANS)
            .join("implementation")
            .join("foo.jsonl");
        let code_recs = specs::read_jsonl(&impl_mirror).unwrap();
        assert!(
            code_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-2" && to == "github.com/x/y.Store"
            )),
            "code review must land in the implementation tier mirror"
        );
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );
        assert!(
            !apg_root.join("notes").exists(),
            "apg/notes must never be written"
        );

        testutil::remove(&repo);
    }

    #[test]
    fn review_add_routes_to_transient_mirrors_and_never_commits() {
        // Transience enforcement (SPEC §5): a review of a durable layer node,
        // a code node, and a plan node each lands its Feedback + Reviews
        // halves under `apg/.trans/` — the tier mirror of the attached node
        // (requirements/implementation) or the plan store — never in
        // `apg/specs/`/`apg/notes/`, never polluting the committed node file,
        // and never committed (the branch HEAD does not move).
        let (apg_root, repo, _wt) = fixture("mirrors");

        // The durable side of the requirement review: the node file in the
        // layers store (committed identity — must stay untouched).
        let req_file = crate::layers::node_file_path(
            &apg_root,
            crate::layers::Layer::Requirements,
            "requirement",
            "timer",
        );
        std::fs::create_dir_all(req_file.parent().unwrap()).unwrap();
        let req_json = r#"{"name":"timer","type":"requirement","layer":"requirements","body":"x","properties":{},"out":[],"in":[]}"#;
        std::fs::write(&req_file, req_json).unwrap();
        // The untracked node file dirties the worktree tree: re-record the
        // scan_meta as dirty so the branch DB stays fresh under the mutations
        // (a recorded dirty matching a dirty tree is fresh — git.rs).
        testutil::write_scan_meta(
            &apg_root,
            Some(&repo.head_sha()),
            false,
            "2026-09-07T00:00:00Z",
        );

        // A transient plan with one task (plan-family target).
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
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
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan".to_string(),
                to: "foo/plan.phase-01".to_string(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".to_string(),
                to: "foo/plan.phase-01.task-1".to_string(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &records).unwrap();
        let head_before = repo.head_sha();

        // 1. A durable requirement (--project required: layer FQNs carry no
        //    project prefix) → the requirements tier mirror.
        let p = parse_args(&[
            "requirements.requirement.timer".to_string(),
            "--body".to_string(),
            "req review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // 2. A code node → the implementation tier mirror.
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // 3. A plan task → the plan store (co-located with the plan records).
        let p = parse_args(&[
            "foo/plan.phase-01.task-1".to_string(),
            "--body".to_string(),
            "task review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // Both halves land in the tier dir of the attached node.
        let req_mirror = apg_root
            .join(specs::TRANS)
            .join("requirements")
            .join("foo.jsonl");
        let impl_mirror = apg_root
            .join(specs::TRANS)
            .join("implementation")
            .join("foo.jsonl");
        let req_recs = specs::read_jsonl(&req_mirror).unwrap();
        assert!(
            req_recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-1")),
            "requirement review Feedback must land in the requirements mirror"
        );
        assert!(
            req_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-1" && to == "requirements.requirement.timer"
            )),
            "requirement review Reviews edge must land beside its Feedback"
        );
        let impl_recs = specs::read_jsonl(&impl_mirror).unwrap();
        assert!(
            impl_recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-2")),
            "code review Feedback must land in the implementation mirror"
        );
        assert!(
            impl_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-2" && to == "github.com/x/y.Store"
            )),
            "code review Reviews edge must land beside its Feedback"
        );
        let plan_recs = specs::read_jsonl(&path).unwrap();
        assert!(
            plan_recs
                .iter()
                .any(|r| matches!(r, Record::Feedback { fqn, .. } if fqn == "foo/feedback-3")),
            "task review Feedback must co-locate with the plan records"
        );
        assert!(
            plan_recs.iter().any(|r| matches!(
                r,
                Record::Reviews { from, to }
                    if from == "foo/feedback-3" && to == "foo/plan.phase-01.task-1"
            )),
            "task review Reviews edge must land beside its Feedback"
        );

        // Never in the legacy durable stores; the committed node file is
        // byte-identical (transient references never pollute node files).
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );
        assert!(
            !apg_root.join("notes").exists(),
            "apg/notes must never be written"
        );
        assert_eq!(
            std::fs::read_to_string(&req_file).unwrap(),
            req_json,
            "the durable node file must never carry transient references"
        );

        // Review state dies with the branch: nothing was committed.
        assert_eq!(
            repo.head_sha(),
            head_before,
            "feedback writes are transient — the branch HEAD must not move"
        );

        // The write-throughs re-ingested every mirror: all three feedback
        // nodes + their edges are in the branch DB, and the requirement
        // review pairs against the durable node.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        for f in ["foo/feedback-1", "foo/feedback-2", "foo/feedback-3"] {
            assert!(db.has_node(f), "{f} must survive the re-ingest");
        }
        let out = db
            .conn()
            .unwrap()
            .query(
                "MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(:Requirement {fqn: 'requirements.requirement.timer'}) RETURN count(*)",
            )
            .unwrap()
            .to_string();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the requirement review must pair against the durable node: {out}"
        );
        drop(db);

        // Status updates also find feedback across the mirrors: action the
        // mirror-resident review (feedback-1, requirements mirror).
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "actioned",
            Some("fixed".to_string()),
        )
        .unwrap();
        let req_recs = specs::read_jsonl(&req_mirror).unwrap();
        let f = req_recs
            .iter()
            .find_map(|r| match r {
                Record::Feedback {
                    fqn,
                    status,
                    disposition,
                    ..
                } if fqn == "foo/feedback-1" => Some((status.clone(), disposition.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, ("actioned".to_string(), "fixed".to_string()));
        assert_eq!(
            repo.head_sha(),
            head_before,
            "feedback status writes are transient too"
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
    fn review_all_tier_mirrors_and_status_verbs_never_commit() {
        // Transience surface completion (SPEC §5): the task-1 test covers the
        // requirements/implementation tier mirrors, the plan store, and one
        // action; this closes the rest — a review of a domain, a solution,
        // and a global target each land their Feedback + Reviews halves in
        // that tier's `.trans` mirror, and the action/resolve/reject status
        // verbs write through the same transient files. Review state dies
        // with the branch: the project branch HEAD (where an auto-commit
        // would land), the main HEAD, and the tree all stay untouched.
        let (apg_root, repo, wt) = fixture("all-tiers");
        let head_before = repo.head_sha();
        let branch_head_before = wt_head(&wt);

        // Seed the plan store (the realistic project shape — a plan exists
        // before any review) so the shared feedback namespace starts at
        // `feedback-1` (the task-1 fixture does the same).
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        artifacts::write_jsonl_and_reingest(
            &apg_root,
            &plan_path,
            "foo",
            &[Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Foo".to_string(),
                strategy: String::new(),
            }],
        )
        .unwrap();

        // One review per untested tier mirror (the fixture DB carries one
        // durable node per file-backed tier).
        for (target, tier, n) in [
            ("domain.entity.order", "domain", 1u64),
            ("solution.system.checkout", "solution", 2),
            ("global.constraint.law", "global", 3),
        ] {
            let p = parse_args(&[
                target.to_string(),
                "--body".to_string(),
                format!("{tier} review"),
                "--project".to_string(),
                "foo".to_string(),
            ]);
            apply_review_add(&apg_root, &p).unwrap();

            // Both halves land in the tier dir of the attached node.
            let mirror = apg_root.join(specs::TRANS).join(tier).join("foo.jsonl");
            let recs = specs::read_jsonl(&mirror).unwrap();
            assert!(
                recs.iter().any(|r| matches!(
                    r,
                    Record::Feedback { fqn, .. } if fqn == &format!("foo/feedback-{n}")
                )),
                "{tier} mirror must carry feedback-{n}"
            );
            assert!(
                recs.iter().any(|r| matches!(
                    r,
                    Record::Reviews { from, to }
                        if from == &format!("foo/feedback-{n}") && to == target
                )),
                "{tier} mirror must carry the Reviews edge beside its Feedback"
            );
        }

        // The status verbs find feedback across the mirrors and write through
        // the same transient files: action, resolve (terminal), reject.
        set_feedback_at(
            &apg_root,
            "foo/feedback-1",
            "foo",
            "actioned",
            Some("fixed".to_string()),
        )
        .unwrap();
        set_feedback_at(&apg_root, "foo/feedback-2", "foo", "resolved", None).unwrap();
        set_feedback_at(
            &apg_root,
            "foo/feedback-3",
            "foo",
            "open",
            Some("rejected".to_string()),
        )
        .unwrap();
        let status_of = |recs: &[Record], fqn: &str| -> (String, String) {
            recs.iter()
                .find_map(|r| match r {
                    Record::Feedback {
                        fqn: f,
                        status,
                        disposition,
                        ..
                    } if f == fqn => Some((status.clone(), disposition.clone())),
                    _ => None,
                })
                .unwrap()
        };
        let dom = specs::read_jsonl(&apg_root.join(specs::TRANS).join("domain").join("foo.jsonl"))
            .unwrap();
        let sol = specs::read_jsonl(
            &apg_root
                .join(specs::TRANS)
                .join("solution")
                .join("foo.jsonl"),
        )
        .unwrap();
        let glo = specs::read_jsonl(&apg_root.join(specs::TRANS).join("global").join("foo.jsonl"))
            .unwrap();
        assert_eq!(
            status_of(&dom, "foo/feedback-1"),
            ("actioned".to_string(), "fixed".to_string())
        );
        assert_eq!(
            status_of(&sol, "foo/feedback-2"),
            ("resolved".to_string(), String::new())
        );
        assert_eq!(
            status_of(&glo, "foo/feedback-3"),
            ("open".to_string(), "rejected".to_string())
        );

        // Never in the legacy durable stores.
        assert!(
            !apg_root.join("specs").exists(),
            "apg/specs must never be written"
        );
        assert!(
            !apg_root.join("notes").exists(),
            "apg/notes must never be written"
        );

        // Review state dies with the branch: nothing was committed.
        assert_eq!(
            wt_head(&wt),
            branch_head_before,
            "feedback writes are transient — the project branch HEAD must not move"
        );
        assert_eq!(
            repo.head_sha(),
            head_before,
            "feedback writes must never move the main HEAD either"
        );
        assert!(
            repo.is_clean(),
            "the tree must stay clean — the mirrors are gitignored"
        );

        testutil::remove(&repo);
    }
}
