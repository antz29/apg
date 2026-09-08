//! `apg review` — the closed writer↔reviewer feedback cycle (SPEC R25/R26).
//! A reviewer attaches a `Feedback` (`open`); a writer actions or wont-fixes it
//! (`actioned`); the reviewer then resolves (terminal) or rejects (reopens).
//! The writer cannot resolve and the reviewer cannot action — enforced by tool
//! permissions (R28), never by convention.

use std::path::{Path, PathBuf};

use crate::artifacts::{self, node_fqn, parse_args, ParsedArgs};
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

/// `apg review add <target-fqn> --body … [--kind …] [--project <p>]
/// [--checks <invariant-fqn>]*` — attach a `Feedback` (`open`) to any artifact
/// node (spec, plan, task, or code). Routing (R1): a spec target
/// serializes in `apg/specs/<project>.jsonl`; a plan/task or code target in
/// `apg/.trans/plans/<project>.jsonl`. `--checks` cites the invariants the
/// comment enforces (a `Checks` Feedback → Invariant edge, PHASE_02); most
/// feedback has none.
fn review_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    if p.positional.is_empty() {
        anyhow::bail!("usage: apg review add <target-fqn> --body … [--kind …] [--project <p>] [--checks <invariant-fqn>]*");
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

    // Validate --checks targets before any write: each must be an Invariant
    // node in the graph (Checks is Feedback → Invariant). The handle is
    // scoped and dropped before the write-through (see the fn doc).
    let checks = p.all("checks");
    if !checks.is_empty() {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        for c in &checks {
            if !db
                .node_label(c)
                .is_some_and(|l| l == "Invariant")
            {
                anyhow::bail!("--checks target `{c}` is not an Invariant node");
            }
        }
    }

    // Discriminate a spec-family target from a code target by its DB node
    // label (the `future/` prefix is gone — PHASE_04): code nodes
    // (Module/Struct/Function/File/UnresolvedTarget) need an explicit
    // `--project`; spec/plan/review nodes derive the project from their FQN's
    // first path segment.
    //
    // The routing DB is scoped so it is dropped before the write-through
    // re-ingest below: opening a second `Database` on the same `db.lbug` while
    // a first is still live corrupts the file (lbug checkpoint from a stale
    // buffer-manager view) — the Feedback node vanishes and later write-throughs
    // SIGSEGV in the engine.
    let (project, target_is_spec) = {
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
                (proj, false)
            }
            Some(_) => {
                let proj = project_of(target).ok_or_else(|| {
                    anyhow::anyhow!("target `{target}` is a spec/plan node without a project prefix")
                })?;
                let is_plan = target.starts_with(&format!("{proj}/plan"));
                (proj, !is_plan)
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

    let file = if target_is_spec {
        specs::spec_jsonl_path(apg_root, &project)
    } else {
        specs::plan_jsonl_path(apg_root, &project)
    };
    let mut records = if file.exists() {
        specs::read_jsonl(&file)?
    } else {
        Vec::new()
    };
    records.push(rec);
    records.push(edge);
    // Optional Checks edges (Feedback → Invariant): a comment cites the rule it
    // enforces (PHASE_02; most feedback has none).
    for c in &checks {
        records.push(Record::Checks {
            from: fqn.clone(),
            to: c.clone(),
        });
    }
    artifacts::write_jsonl_and_reingest(apg_root, &file, &project, &records)?;
    println!("Attached {fqn} (open) → {target}");
    Ok(())
}

/// The next free `feedback-<n>` across the project's spec AND plan JSONLs.
/// Feedback FQNs share one `<project>/feedback-<n>` namespace regardless of
/// which file a review routes to (spec-family vs code/plan targets) — numbering
/// per-file would give a spec-target and a code-target review the same FQN and
/// the re-ingest would collapse them into one Feedback node with both edges.
fn feedback_number(apg_root: &Path, project: &str) -> anyhow::Result<u64> {
    let mut n: u64 = 0;
    for file in [
        specs::spec_jsonl_path(apg_root, project),
        specs::plan_jsonl_path(apg_root, project),
    ] {
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

/// Locates the file a feedback fqn lives in (spec or plan JSONL) and updates
/// its status/disposition, write-through.
fn set_feedback(
    fqn: &str,
    status: &str,
    disposition: Option<String>,
    _note: Option<String>,
) -> anyhow::Result<()> {
    let project = project_of(fqn).ok_or_else(|| {
        anyhow::anyhow!("feedback fqn `{fqn}` must be `<project>/feedback-<n>`")
    })?;
    let apg_root = require_apg_root()?;
    set_feedback_at(&apg_root, fqn, &project, status, disposition)
}

/// Core of `set_feedback`: update a feedback node's status/disposition in the
/// file that carries it (spec or plan JSONL), write-through.
fn set_feedback_at(
    apg_root: &Path,
    fqn: &str,
    project: &str,
    status: &str,
    disposition: Option<String>,
) -> anyhow::Result<()> {
    artifacts::acquire_spec_lock(apg_root)?;

    let candidates = [
        specs::spec_jsonl_path(apg_root, project),
        specs::plan_jsonl_path(apg_root, project),
    ];
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
    use lbug::{Connection, Database};

    /// A temp `apg/` layout with a real DB carrying a minimal code graph.
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("apg-review-test-{}-{name}", std::process::id()));
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

    #[test]
    fn wont_fix_is_not_terminal_until_reviewer_resolves() {
        let (apg_root, dir) = fixture("wont-fix");

        // A spec project with a requirement, plus an open Feedback reviewing it.
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let records = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "T".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/spec.R1".to_string(),
            },
            Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/spec.R1".to_string(),
            },
        ];
        specs::write_jsonl(&path, &records).unwrap();

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
                Record::Feedback { fqn, status, disposition, .. }
                    if fqn == "foo/feedback-1" =>
                {
                    Some((status.clone(), disposition.clone()))
                }
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
                Record::Feedback { fqn, status, disposition, .. }
                    if fqn == "foo/feedback-1" =>
                {
                    Some((status.clone(), disposition.clone()))
                }
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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_add_with_checks_roundtrips_and_does_not_poison_db() {
        // REVIEW.md item 1 + item 2 regression: `apg review add <target>
        // --checks <invariant>` wrote the feedback + reviews + checks records
        // to the JSONL but the re-ingest silently dropped them (no Feedback
        // node, no Reviews/Checks edges), and any later write-through SIGSEGV'd
        // (exit 139, lbug LocalNodeTable::isVisible). Root cause: the routing
        // `ArtifactDb` was left live across `write_jsonl_and_reingest`, so a
        // second `Database` opened on the same `db.lbug` while the first was
        // still open — the file got corrupted (Feedback node vanishes, engine
        // crashes on the next checkpoint). The routing handle is now scoped
        // and dropped before the write-through; this test drives the real
        // `apply_review_add` path end to end.
        let (apg_root, dir) = fixture("checks-roundtrip");

        // A spec project with one requirement, plus a project-scoped Invariant
        // to cite with --checks (established via write-through, like real use).
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let base = vec![
            Record::Spec {
                fqn: "foo/spec".to_string(),
                title: "Foo".to_string(),
                goal: String::new(),
            },
            Record::Requirement {
                fqn: "foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "T".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "foo/spec".to_string(),
                to: "foo/spec.R1".to_string(),
            },
            Record::Invariant {
                fqn: "foo/invariant/guard".to_string(),
                title: "guard".to_string(),
                body: String::new(),
                category: "product".to_string(),
                scope: "code".to_string(),
                status: "active".to_string(),
            },
        ];
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &base).unwrap();

        // `apg review add foo/spec.R1 --body … --checks foo/invariant/guard`.
        let p = parse_args(&[
            "foo/spec.R1".to_string(),
            "--body".to_string(),
            "violates the rule".to_string(),
            "--checks".to_string(),
            "foo/invariant/guard".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // The Feedback node round-trips with its Reviews and Checks edges.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(
            db.has_node("foo/feedback-1"),
            "feedback node dropped by re-ingest"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "reviews edge: {out}");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Checks]->(i:Invariant) RETURN i.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/invariant/guard"), "checks edge: {out}");
        drop(db);

        // A later write-through must not crash — PRE-FIX this SIGSEGV'd (the
        // checks path poisoned the DB so every mutation died with exit 139).
        let p = parse_args(&[
            "foo/spec.R1".to_string(),
            "--body".to_string(),
            "second review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-2"), "post-checks write-through lost the node");
        assert!(db.has_node("foo/feedback-1"));
        drop(db);

        // A code-target review routes to the plan JSONL but numbers from the
        // same project-wide `feedback-<n>` namespace (no FQN collision with
        // the spec-file feedback). Reviews(Feedback → code node).
        let p = parse_args(&[
            "github.com/x/y.Store".to_string(),
            "--body".to_string(),
            "code review".to_string(),
            "--project".to_string(),
            "foo".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-3"), "code-target feedback FQN collided");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-3'})-[:Reviews]->(n:Struct) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("github.com/x/y.Store"), "code reviews edge: {out}");
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_add_on_tier_node_roundtrips_reviews_edge() {
        // REVIEW.md item: `apg review add <project>/domain.X …` wrote the
        // Reviews edge to the JSONL but the re-ingest silently projected it
        // away — the Reviews rel-table had no tier-1/2/3
        // (Stakeholder/Domain/…/Component) pairs, so the Feedback node landed
        // with no Reviews edge. The rel-table now declares Feedback → every
        // tier label; the edge survives a tier-2 and a tier-3 target.
        let (apg_root, dir) = fixture("tier-review");

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

        // A review against the tier-2 Domain node, then one against the tier-3
        // System node — both routed to the spec JSONL (not code labels).
        let p = parse_args(&[
            "foo/domain.X".to_string(),
            "--body".to_string(),
            "domain review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();
        let p = parse_args(&[
            "foo/system.Y".to_string(),
            "--body".to_string(),
            "system review".to_string(),
        ]);
        apply_review_add(&apg_root, &p).unwrap();

        // The Feedback nodes round-trip WITH their Reviews edges — pre-fix the
        // edges vanished (projected away at re-ingest) despite the JSONL
        // records being written.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/feedback-1"), "feedback-1 node dropped");
        assert!(db.has_node("foo/feedback-2"), "feedback-2 node dropped");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/domain.X"), "tier-2 reviews edge dropped: {out}");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-2'})-[:Reviews]->(n) RETURN n.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/system.Y"), "tier-3 reviews edge dropped: {out}");
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
