//! The `apg query` surface: open `db.lbug` read-only, render CSV/JSON rows.
//!
//! A cohesive single-responsibility module: byte-identical output for both the
//! direct path and the session coordinator's routed-read branch.

use lbug::{Connection, Database};

use crate::find_apg_root;
use crate::session;
use crate::specs;

/// `apg query "<cypher>"`: hold a read-only per-invocation session on
/// `apg/.trans/db.lbug` and print the result as CSV with a header row. When
/// `APG_PROJECT` binds a registry row the read targets that project's worktree
/// database; otherwise it is the database found by walking up from cwd.
pub(crate) fn cmd_query(args: &[String]) -> anyhow::Result<()> {
    let json = args.first().is_some_and(|a| a == "--json");
    let query = if json {
        args[1..].join(" ")
    } else {
        args.join(" ")
    };
    if query.trim().is_empty() {
        anyhow::bail!("usage: apg query [--json] \"<cypher>\"");
    }
    let start = std::env::current_dir()?;
    let apg_root = find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))?;

    // The env-gated registry binding: with `APG_PROJECT` present the
    // read targets the registered project's own worktree and nothing else, so a
    // missing database there is refused rather than served from another
    // checkout; the seam's refusals (lost registry, unregistered, stale,
    // unauthorised) propagate unchanged.
    let bound = crate::registry::require_project(&apg_root)?;
    let read_apg = match &bound {
        Some(row) => row.worktree.join(specs::LAYOUT),
        None => apg_root,
    };

    let query = if query.trim_end().ends_with(';') {
        query
    } else {
        format!("{query};")
    };

    // A bound read targets the registered project's own worktree and nothing
    // else, so a missing database there is refused rather than served from
    // another checkout. The pre-check mirrors `open_project_db`'s wording, the
    // refusal its project-bound callers have always reported.
    if let Some(row) = &bound {
        let db_path = read_apg.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            anyhow::bail!(
                "refused: project `{}` at worktree `{}` has no database `{}`. Fix: run `apg scan` inside `{}`.",
                row.name,
                row.worktree.display(),
                db_path.display(),
                row.worktree.display()
            );
        }
    }

    // The read runs on the invocation's own read-only handle: no session
    // daemon, no liveness probe and no routed forward, and the read-only class
    // takes no exclusive lock a concurrent writer needs.
    let session = session::Session::open(&read_apg).map_err(|err| match &bound {
        Some(row) => anyhow::anyhow!(
            "project `{}` at worktree `{}`: opening `{}`: {err}",
            row.name,
            row.worktree.display(),
            read_apg.join(specs::TRANS).join("db.lbug").display()
        ),
        None => err,
    })?;
    println!("{}", render_query(session.db(), &query, json)?);
    Ok(())
}

/// Render a query result exactly as `apg query` prints it: JSON rows for
/// `--json`, otherwise CSV with a header row (no trailing newline). Shared by
/// the direct path and the session coordinator's routed-read branch, so both
/// produce byte-identical output.
pub(crate) fn render_query(db: &Database, query: &str, json: bool) -> anyhow::Result<String> {
    let conn = Connection::new(db)?;
    let result = conn.query(query)?;
    if json {
        return Ok(emit_json_rows(result));
    }
    let names = result.get_column_names();
    let mut out = names
        .iter()
        .map(|n| csv_escape(n))
        .collect::<Vec<_>>()
        .join(",");
    for row in result {
        out.push('\n');
        out.push_str(
            &row.iter()
                .map(|v| csv_escape(&v.to_string()))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    Ok(out)
}

/// Renders a query result as a JSON array of objects, one per row, keyed by
/// column name with string-typed values (matching CSV's cell semantics).
pub fn emit_json_rows(result: lbug::QueryResult<'_>) -> String {
    let names = result.get_column_names();
    let rows: Vec<serde_json::Value> = result
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = row.get(i).map(|v| v.to_string()).unwrap_or_default();
                obj.insert(name.clone(), serde_json::Value::String(v));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(rows)).unwrap()
}

pub fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}
