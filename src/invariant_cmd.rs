//! `apg invariant add` / `apg invariant rm` / `apg invariants` — the
//! graph-wide invariant mechanism (Invariants-SPEC.md; PHASE_02). Invariants
//! are first-class graph nodes:
//! rules artifacts must respect, materialized via `apg invariant add`, guarded
//! onto artifacts (`GuardedBy`), citable from review feedback (`Checks`), and
//! listable via `apg invariants`.
//!
//! Roots: `invariant/<name>` for universal rules (serialized to
//! `apg/specs/_invariants.jsonl`), `<project>/invariant/<name>` for
//! repo/project-specific rules (serialized to the project's spec JSONL —
//! project-scoped and stable; present-ness is branch-determined, never an FQN
//! property). The flow works identically with zero invariants: they are
//! emergent, never a precondition.

use std::path::{Path, PathBuf};

use crate::artifacts::{self, ParsedArgs, parse_args};
use crate::load;
use crate::schema::Record;
use crate::specs;

fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

/// `apg invariant <add|rm|list>` — the write (add/retire) and read (list)
/// commands. The bare `apg invariants` command (main.rs dispatch) is the same
/// `list`.
pub fn cmd_invariant(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg invariant <add|rm|list> …  (or `apg invariants`)");
    };
    match sub {
        "add" => invariant_add(&args[1..]),
        "rm" | "remove" | "retire" => invariant_rm(&args[1..]),
        "list" | "ls" => invariant_list(&args[1..]),
        other => anyhow::bail!("unknown apg invariant subcommand: {other}"),
    }
}

/// `apg invariants [--scope …] [--project …]` — the read command (also
/// dispatched directly from main as `apg invariants`).
pub fn cmd_invariants(args: &[String]) -> anyhow::Result<()> {
    invariant_list(args)
}

/// The allowed invariant categories (Invariants-SPEC.md): process rules about
/// spec/plan/review structure and workflow, product rules about the codebase
/// or artifacts, graph-integrity rules about the graph itself.
fn validate_category(cat: &str) -> anyhow::Result<()> {
    if !["process", "product", "graph-integrity"].contains(&cat) {
        anyhow::bail!(
            "invalid invariant category `{cat}` — one of process|product|graph-integrity"
        );
    }
    Ok(())
}

/// The guardable artifact labels (the GuardedBy rel-table from-kinds, mirroring
/// `load::guarded_by_from_labels`). Any code, spec/plan, or tier-1/2/3 node may
/// be guarded; Note/Feedback/UnresolvedTarget/Scan/Invariant are not artifacts
/// an invariant constrains and are rejected up front (the DB has no rel-table
/// pair for them — a guard there would be silently projected away at re-ingest).
fn guardable_labels() -> &'static [&'static str] {
    load::guarded_by_from_labels()
}

/// `apg invariant add [<project>] <name> --title … --body … --category …
/// --scope … [--guard <fqn>]*`. Materializes the Invariant node (universal
/// `invariant/<name>` when no project, else `<project>/invariant/<name>`),
/// optionally linking `GuardedBy(artifact → invariant)`.
fn invariant_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(name) = p.positional.last() else {
        anyhow::bail!(
            "usage: apg invariant add [<project>] <name> --title … --body … --category <process|product|graph-integrity> --scope … [--guard <fqn>]*"
        );
    };
    // The optional leading positional is the project; the last positional is
    // the invariant name. `apg invariant add <name>` → universal;
    // `apg invariant add <project> <name>` → project-scoped.
    let project = if p.positional.len() >= 2 {
        Some(p.positional[0].as_str())
    } else {
        None
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    apply_invariant_add(&apg_root, project, name, &p)?;
    println!(
        "Added invariant {name} ({}) — {} artifact(s) guarded",
        if project.is_some() {
            "project-scoped"
        } else {
            "universal"
        },
        p.all("guard").len()
    );
    Ok(())
}

/// Materializes an invariant (write-through), validating the category and
/// every `--guard` target before any write. Separated from `invariant_add` so
/// tests can drive it against a fixture root.
fn apply_invariant_add(
    apg_root: &Path,
    project: Option<&str>,
    name: &str,
    p: &ParsedArgs,
) -> anyhow::Result<()> {
    let title = p
        .get("title")
        .ok_or_else(|| anyhow::anyhow!("invariant requires --title"))?;
    let body = p.get("body").unwrap_or_default();
    let category = p
        .get("category")
        .ok_or_else(|| anyhow::anyhow!("invariant requires --category <process|product|graph-integrity>"))?;
    validate_category(&category)?;
    let scope = p.get("scope").unwrap_or_default();
    let status = "active".to_string();

    let (fqn, file, project_name): (String, PathBuf, &str) = match project {
        Some(proj) => {
            // Project-scoped: `<project>/invariant/<name>` (the FQN is
            // project-scoped and stable; present-ness is branch membership, not an FQN
// prefix).
            (
                format!("{proj}/invariant/{name}"),
                specs::spec_jsonl_path(apg_root, proj),
                proj,
            )
        }
        None => {
            // Universal: `invariant/<name>` in the shared invariant ledger.
            (
                format!("invariant/{name}"),
                apg_root.join("specs").join("_invariants.jsonl"),
                "_invariants",
            )
        }
    };

    // Validate every --guard target before any write: it must resolve in the
    // graph and be a guardable artifact label. Only a DB-backed check — with
    // zero --guard flags there is nothing to validate, so a missing DB is not
    // an error (invariants are emergent; the flow works identically with zero
    // invariants, Invariants-SPEC "No precondition").
    let guards = p.all("guard");
    if !guards.is_empty() {
        let db = artifacts::ArtifactDb::open(apg_root)?;
        for g in &guards {
            let Some(label) = db.node_label(g) else {
                anyhow::bail!("guard target `{g}` does not exist in the graph");
            };
            if !guardable_labels().contains(&label) {
                anyhow::bail!("guard target `{g}` is a `{label}` node — not a guardable artifact");
            }
        }
    }

    // Load the target file (create the ledger if universal), upsert by fqn.
    let mut records = if file.exists() {
        specs::read_jsonl(&file)?
    } else {
        Vec::new()
    };
    artifacts::remove_node(&mut records, &fqn);
    records.push(Record::Invariant {
        fqn: fqn.clone(),
        title,
        body,
        category,
        scope,
        status,
    });
    for g in &guards {
        records.push(Record::GuardedBy {
            from: g.clone(),
            to: fqn.clone(),
        });
    }
    artifacts::write_jsonl_and_reingest(apg_root, &file, project_name, &records)?;
    Ok(())
}

/// `apg invariant rm [<project>] <name>` — retire an invariant (status flip
/// active → retired; Invariants-SPEC lifecycle "retire/amend via status flip").
/// The node and its incident GuardedBy/Checks edges are preserved so historical
/// citations stay traceable — a retired invariant is simply no longer active
/// (the navigator injects, and writers/reviewers query, the active set).
fn invariant_rm(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(name) = p.positional.last() else {
        anyhow::bail!("usage: apg invariant rm [<project>] <name>");
    };
    let project = if p.positional.len() >= 2 {
        Some(p.positional[0].as_str())
    } else {
        None
    };
    let apg_root = require_apg_root()?;
    artifacts::acquire_spec_lock(&apg_root)?;
    apply_invariant_rm(&apg_root, project, name)?;
    println!(
        "Retired invariant {name} ({})",
        if project.is_some() {
            "project-scoped"
        } else {
            "universal"
        }
    );
    Ok(())
}

/// The retirement write-through: load the ledger/project file, flip the
/// invariant's status to `retired`, re-ingest. Separated from `invariant_rm` so
/// tests can drive it against a fixture root (the `apply_invariant_add` pattern).
fn apply_invariant_rm(
    apg_root: &Path,
    project: Option<&str>,
    name: &str,
) -> anyhow::Result<()> {
    let (fqn, file, project_name) = match project {
        Some(proj) => (
            format!("{proj}/invariant/{name}"),
            specs::spec_jsonl_path(apg_root, proj),
            proj,
        ),
        None => (
            format!("invariant/{name}"),
            apg_root.join("specs").join("_invariants.jsonl"),
            "_invariants",
        ),
    };
    let mut records = if file.exists() {
        specs::read_jsonl(&file)?
    } else {
        Vec::new()
    };
    {
        let Some(status) = records.iter_mut().find_map(|r| match r {
            Record::Invariant { fqn: f, status, .. } if *f == fqn => Some(status),
            _ => None,
        }) else {
            anyhow::bail!("invariant `{fqn}` does not exist");
        };
        if status == "retired" {
            anyhow::bail!("invariant `{fqn}` is already retired");
        }
        *status = "retired".to_string();
    }
    artifacts::write_jsonl_and_reingest(apg_root, &file, project_name, &records)?;
    Ok(())
}

/// `apg invariants [--scope …] [--project …]` — list invariant nodes with
/// their category/scope/status, filtered by scope (a substring match) and/or
/// project (only `<project>/invariant/…` project-scoped invariants).
fn invariant_list(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let apg_root = require_apg_root()?;
    let db = artifacts::ArtifactDb::open(&apg_root)?;
    let mut conds: Vec<String> = Vec::new();
    if let Some(scope) = p.get("scope") {
        conds.push(format!("i.scope CONTAINS {}", artifacts::lit(&scope)));
    }
    if let Some(project) = p.get("project") {
        conds.push(format!(
            "i.fqn STARTS WITH {} OR i.fqn STARTS WITH {}",
            artifacts::lit(&format!("{project}/invariant/")),
            artifacts::lit(&format!("{project}/invariant/"))
        ));
    }
    let where_clause = if conds.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conds.join(" AND "))
    };
    let q = format!(
        "MATCH (i:Invariant){where_clause} RETURN i.fqn, i.title, i.category, i.scope, i.status ORDER BY i.fqn"
    );
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

    /// A temp `apg/` layout with a real `apg/.trans/db.lbug` holding a code
    /// graph (github.com/x/y.Store struct + file + module).
    fn fixture_layout(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("apg-invariant-test-{}-{name}", std::process::id()));
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

        // A non-guardable artifact (Note) — the invariant_add --guard
        // allow-list must reject it up front, never silently drop the edge.
        g.nodes.insert(
            "note-1".to_string(),
            Node {
                kind: NodeKind::Note,
                body: Some("not guardable".to_string()),
                code_type: String::new(),
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
    fn invariant_roundtrip_with_guard() {
        let (apg_root, dir) = fixture_layout("roundtrip");

        // Universal invariant guarding a code node (the classic product rule:
        // "release records' version equals the tag").
        let p = parse_args(&[
            "task-kind".to_string(),
            "--title".to_string(),
            "Task kind".to_string(),
            "--body".to_string(),
            "Every plan task carries exactly one kind".to_string(),
            "--category".to_string(),
            "process".to_string(),
            "--scope".to_string(),
            "plan".to_string(),
            "--guard".to_string(),
            "github.com/x/y.Store".to_string(),
        ]);
        apply_invariant_add(&apg_root, None, "task-kind", &p).unwrap();

        // The invariant lives in the universal ledger, guarded onto the code.
        let ledger = apg_root.join("specs").join("_invariants.jsonl");
        let recs = specs::read_jsonl(&ledger).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, title, category, .. }
                if fqn == "invariant/task-kind"
                    && title == "Task kind"
                    && category == "process"
        )));
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::GuardedBy { from, to }
                if from == "github.com/x/y.Store" && to == "invariant/task-kind"
        )));

        // The live DB carries the invariant + GuardedBy edge.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (i:Invariant {fqn: 'invariant/task-kind'}) RETURN i.category, i.status")
            .unwrap()
            .to_string();
        assert!(
            out.contains("process") && out.contains("active"),
            "invariant rows: {out}"
        );
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Struct {fqn: 'github.com/x/y.Store'})-[:GuardedBy]->(:Invariant) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "guarded_by edge: {out}"
        );
        drop(db);

        // Invalid category rejected before any write.
        let p = parse_args(&[
            "x".to_string(),
            "--title".to_string(),
            "X".to_string(),
            "--category".to_string(),
            "bogus".to_string(),
            "--scope".to_string(),
            "spec".to_string(),
        ]);
        assert!(apply_invariant_add(&apg_root, None, "x", &p).is_err());
        assert!(!apg_root.join("specs").join("_invariants.jsonl").exists()
            || specs::read_jsonl(&apg_root.join("specs").join("_invariants.jsonl"))
                .unwrap()
                .iter()
                .all(|r| !matches!(r, Record::Invariant { fqn, .. } if fqn == "invariant/x")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_guardable_label_rejected_before_write() {
        let (apg_root, dir) = fixture_layout("non-guardable");

        // Guarding a Note — not a GuardedBy from-kind — must be rejected up
        // front, before any record lands in the ledger.
        let p = parse_args(&[
            "x".to_string(),
            "--title".to_string(),
            "X".to_string(),
            "--category".to_string(),
            "process".to_string(),
            "--scope".to_string(),
            "spec".to_string(),
            "--guard".to_string(),
            "note-1".to_string(),
        ]);
        let err = apply_invariant_add(&apg_root, None, "x", &p).unwrap_err();
        assert!(
            err.to_string().contains("not a guardable artifact"),
            "error: {err}"
        );

        // Nothing was written: no invariant node and no GuardedBy edge.
        let ledger = apg_root.join("specs").join("_invariants.jsonl");
        assert!(
            !ledger.exists()
                || specs::read_jsonl(&ledger).unwrap().iter().all(|r| {
                    !matches!(r, Record::Invariant { fqn, .. } if fqn == "invariant/x")
                        && !matches!(
                            r,
                            Record::GuardedBy { from, to }
                                if from == "note-1" && to == "invariant/x"
                        )
                }),
            "ledger must not carry the rejected invariant"
        );

        // The guardable path still works (a Struct guard lands).
        let p = parse_args(&[
            "task-kind".to_string(),
            "--title".to_string(),
            "Task kind".to_string(),
            "--category".to_string(),
            "process".to_string(),
            "--scope".to_string(),
            "plan".to_string(),
            "--guard".to_string(),
            "github.com/x/y.Store".to_string(),
        ]);
        apply_invariant_add(&apg_root, None, "task-kind", &p).unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Struct {fqn: 'github.com/x/y.Store'})-[:GuardedBy]->(:Invariant) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "guarded_by edge: {out}");
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invariant_add_works_with_no_db_and_zero_guards() {
        // A bare `apg/` layout with a specs dir but NO db.lbug — the
        // "emergent, works with zero invariants" flow (Invariants-SPEC
        // "No precondition"). With zero --guard flags there is nothing to
        // validate against the graph, so the write must not demand a scan.
        let dir =
            std::env::temp_dir().join(format!("apg-invariant-test-{}-nodb", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        std::fs::create_dir_all(dir.join("apg").join("specs")).unwrap();
        let apg_root = dir.join("apg");
        assert!(
            !apg_root.join(specs::TRANS).join("db.lbug").exists(),
            "fixture must start DB-less"
        );

        let p = parse_args(&[
            "task-kind".to_string(),
            "--title".to_string(),
            "Task kind".to_string(),
            "--body".to_string(),
            "Every plan task carries exactly one kind".to_string(),
            "--category".to_string(),
            "process".to_string(),
            "--scope".to_string(),
            "plan".to_string(),
        ]);
        apply_invariant_add(&apg_root, None, "task-kind", &p).unwrap();

        // The invariant lands in the universal ledger with no DB involved.
        let ledger = apg_root.join("specs").join("_invariants.jsonl");
        let recs = specs::read_jsonl(&ledger).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, title, category, .. }
                if fqn == "invariant/task-kind"
                    && title == "Task kind"
                    && category == "process"
        )));

        // A project-scoped invariant works DB-less too.
        let p = parse_args(&[
            "foo".to_string(),
            "NoNegativeBalance".to_string(),
            "--title".to_string(),
            "No negative balance".to_string(),
            "--category".to_string(),
            "product".to_string(),
            "--scope".to_string(),
            "code".to_string(),
        ]);
        apply_invariant_add(&apg_root, Some("foo"), "NoNegativeBalance", &p).unwrap();
        let recs = specs::read_jsonl(&specs::spec_jsonl_path(&apg_root, "foo")).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, category, .. }
                if fqn == "foo/invariant/NoNegativeBalance" && category == "product"
        )));

        // --guard still requires a scanned DB (there is nothing to validate
        // against without one) — rejected up front, never silently dropped.
        let p = parse_args(&[
            "g".to_string(),
            "--title".to_string(),
            "G".to_string(),
            "--category".to_string(),
            "process".to_string(),
            "--scope".to_string(),
            "spec".to_string(),
            "--guard".to_string(),
            "github.com/x/y.Store".to_string(),
        ]);
        let err = apply_invariant_add(&apg_root, None, "g", &p).unwrap_err();
        assert!(
            err.to_string().contains("run `apg scan` first"),
            "error: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invariant_retire_flips_status_and_preserves_edges() {
        let (apg_root, dir) = fixture_layout("retire");

        // Add a universal invariant guarding a code node (status active).
        let p = parse_args(&[
            "task-kind".to_string(),
            "--title".to_string(),
            "Task kind".to_string(),
            "--body".to_string(),
            "Every plan task carries exactly one kind".to_string(),
            "--category".to_string(),
            "process".to_string(),
            "--scope".to_string(),
            "plan".to_string(),
            "--guard".to_string(),
            "github.com/x/y.Store".to_string(),
        ]);
        apply_invariant_add(&apg_root, None, "task-kind", &p).unwrap();
        let ledger = apg_root.join("specs").join("_invariants.jsonl");
        assert!(specs::read_jsonl(&ledger).unwrap().iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, status, .. }
                if fqn == "invariant/task-kind" && status == "active"
        )));

        // Retire it: the status flips to retired, the node + GuardedBy edge
        // survive (the retirement path, Invariants-SPEC "status flip").
        apply_invariant_rm(&apg_root, None, "task-kind").unwrap();
        assert!(specs::read_jsonl(&ledger).unwrap().iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, status, .. }
                if fqn == "invariant/task-kind" && status == "retired"
        )));
        assert!(specs::read_jsonl(&ledger).unwrap().iter().any(|r| matches!(
            r,
            Record::GuardedBy { from, to }
                if from == "github.com/x/y.Store" && to == "invariant/task-kind"
        )));

        // The live DB reflects the flip and keeps the GuardedBy edge.
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (i:Invariant {fqn: 'invariant/task-kind'}) RETURN i.status")
            .unwrap()
            .to_string();
        assert!(out.contains("retired"), "invariant rows: {out}");
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Struct {fqn: 'github.com/x/y.Store'})-[:GuardedBy]->(:Invariant) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "guarded_by edge: {out}");
        drop(db);

        // Retiring a nonexistent / already-retired invariant errors, and the
        // file is untouched.
        assert!(apply_invariant_rm(&apg_root, None, "nope").is_err());
        assert!(apply_invariant_rm(&apg_root, None, "task-kind").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_scoped_invariant_and_checks_roundtrip() {
        let (apg_root, dir) = fixture_layout("project-scoped");

        // A spec project + requirement (so the project-scoped invariant has a
        // project file and a feedback target exists).
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let recs = vec![
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
                body: "violates the rule".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            },
            Record::Reviews {
                from: "foo/feedback-1".to_string(),
                to: "foo/spec.R1".to_string(),
            },
        ];
        specs::write_jsonl(&path, &recs).unwrap();
        artifacts::reingest_project(&apg_root, "foo").unwrap();

        // A domain rule materialized as a project-scoped Invariant
        // (category=product) — the PHASE_02 DomainRule alignment.
        let p = parse_args(&[
            "foo".to_string(),
            "NoNegativeBalance".to_string(),
            "--title".to_string(),
            "No negative balance".to_string(),
            "--body".to_string(),
            "A balance never goes below zero".to_string(),
            "--category".to_string(),
            "product".to_string(),
            "--scope".to_string(),
            "code".to_string(),
            "--guard".to_string(),
            "foo/spec".to_string(),
        ]);
        apply_invariant_add(&apg_root, Some("foo"), "NoNegativeBalance", &p).unwrap();

        // The project-scoped invariant serializes into the project's spec JSONL
        // and lands in the DB as a GuardedBy edge.
        let recs = specs::read_jsonl(&path).unwrap();
        assert!(recs.iter().any(|r| matches!(
            r,
            Record::Invariant { fqn, category, .. }
                if fqn == "foo/invariant/NoNegativeBalance" && category == "product"
        )));
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Spec {fqn: 'foo/spec'})-[:GuardedBy]->(:Invariant) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "guarded spec: {out}");
        drop(db);

        // A review comment cites the invariant (Checks Feedback → Invariant),
        // written through the project's spec JSONL like any feedback.
        let mut recs = specs::read_jsonl(&path).unwrap();
        recs.push(Record::Checks {
            from: "foo/feedback-1".to_string(),
            to: "foo/invariant/NoNegativeBalance".to_string(),
        });
        artifacts::write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();
        let db = artifacts::ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Feedback {fqn: 'foo/feedback-1'})-[:Checks]->(:Invariant) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "checks edge: {out}");
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The PHASE_02 done gate's "tool smoke for the two suite tools" —
    /// `apg_invariant_add.ts` / `apg_invariants.ts` must ship (SUITE_TOOLS
    /// embed) and actually run. The hermetic half asserts registration + the
    /// tool→CLI arg mapping; the live half executes the real `.ts` tool files
    /// through bun against a real fixture DB + the real `apg` binary
    /// (APG_BINARY), so a drifted flag or broken dispatch surfaces as a
    /// runtime failure. Skipped (not failed) when bun is absent — the tools
    /// only run inside opencode, which is bun-based.
    #[test]
    fn suite_tools_invariant_add_and_invariants_smoke() {
        // Hermetic: both suite tools are embedded in SUITE_TOOLS, and their
        // arg→CLI mapping matches the flags `apg invariant add` /
        // `apg invariants` actually accept.
        let names: Vec<&str> = crate::SUITE_TOOLS.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"apg_invariant_add.ts"),
            "apg_invariant_add.ts must be registered in SUITE_TOOLS"
        );
        assert!(
            names.contains(&"apg_invariants.ts"),
            "apg_invariants.ts must be registered in SUITE_TOOLS"
        );
        let add_src = crate::SUITE_TOOLS
            .iter()
            .find(|(n, _)| *n == "apg_invariant_add.ts")
            .unwrap()
            .1;
        let list_src = crate::SUITE_TOOLS
            .iter()
            .find(|(n, _)| *n == "apg_invariants.ts")
            .unwrap()
            .1;
        assert!(add_src.contains("\"invariant\", \"add\""));
        assert!(add_src.contains("--title") && add_src.contains("--category"));
        assert!(add_src.contains("--guard"));
        assert!(list_src.contains("\"invariants\""));
        assert!(list_src.contains("--scope") && list_src.contains("--project"));

        // Live: run the real tool files through bun against a real fixture DB
        // and the real apg binary, exactly as opencode would invoke them.
        let (_apg_root, dir) = fixture_layout("suite-smoke");
        let bunver = std::process::Command::new("bun")
            .arg("--version")
            .output();
        let Ok(bunver) = bunver else {
            eprintln!("skipping live suite-tool smoke: bun not on PATH");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        if !bunver.status.success() {
            eprintln!("skipping live suite-tool smoke: bun unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        // The CLI binary of the profile the test runs under: tests are built
        // as <profile>/deps/apg-<hash>, the CLI as <profile>/apg.
        let exe = std::env::current_exe().unwrap();
        let profile = exe.parent().and_then(|d| d.parent()).unwrap();
        let apg_bin = profile.join("apg");
        if !apg_bin.exists() {
            let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
            let mut cmd = std::process::Command::new("cargo");
            cmd.arg("build").current_dir(manifest);
            if profile.file_name().map(|n| n == "release").unwrap_or(false) {
                cmd.arg("--release");
            }
            let out = cmd.output().expect("cargo build spawn");
            assert!(
                out.status.success(),
                "cargo build failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(apg_bin.exists(), "apg binary missing at {}", apg_bin.display());

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let tool_add = manifest.join(".opencode").join("tools").join("apg_invariant_add.ts");
        let tool_list = manifest.join(".opencode").join("tools").join("apg_invariants.ts");
        let entry = dir.join("suite_smoke.ts");
        std::fs::write(
            &entry,
            format!(
                r#"
import addTool from "{}"
import listTool from "{}"
const ctx = {{ directory: "{}", worktree: "{}" }}
const addOut = await addTool.execute({{ name: "task-kind", title: "Task kind", category: "process", scope: "plan", guard: ["github.com/x/y.Store"] }}, ctx)
console.log("ADD_BEGIN")
console.log(addOut)
console.log("ADD_END")
const listOut = await listTool.execute({{ scope: "plan" }}, ctx)
console.log("LIST_BEGIN")
console.log(listOut)
console.log("LIST_END")
"#,
                tool_add.display(),
                tool_list.display(),
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();

        let out = std::process::Command::new("bun")
            .arg(&entry)
            .env("APG_BINARY", &apg_bin)
            .output()
            .expect("bun spawn");
        assert!(
            out.status.success(),
            "bun failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let add = stdout
            .split("ADD_BEGIN")
            .nth(1)
            .and_then(|s| s.split("ADD_END").next())
            .unwrap_or_default();
        let list = stdout
            .split("LIST_BEGIN")
            .nth(1)
            .and_then(|s| s.split("LIST_END").next())
            .unwrap_or_default();
        assert!(
            add.contains("Added invariant task-kind"),
            "add tool output: {add}"
        );
        assert!(
            list.contains("invariant/task-kind"),
            "list tool output: {list}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}