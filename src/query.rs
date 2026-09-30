//! The `apg query` surface: open `db.lbug` read-only, render CSV/JSON rows.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — byte-identical output for
//! both the direct path and the session coordinator's routed-read branch.

use lbug::{Connection, Database, SystemConfig};

use crate::find_apg_root;
use crate::session;
use crate::specs;

/// `apg query "<cypher>"`: open `apg/.trans/db.lbug` (found by walking up from
/// cwd) read-only and print the result as CSV with a header row.
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

    let query = if query.trim_end().ends_with(';') {
        query
    } else {
        format!("{query};")
    };

    // Read routing (phase-03): while a session owns db.lbug, route the read
    // through it so a separate reader sees the current state with no lock error
    // and without waiting for the session to end. With no live session the DB
    // file is not held and a normal (read-only) direct open serves the read —
    // that is the read-your-writes guarantee.
    if session::live_session(&apg_root) {
        let output = session::forward_query(&apg_root, &query, json)?;
        println!("{output}");
        return Ok(());
    }

    // Crash recovery (phase-02 task-15): a session socket that is present but
    // fails a connect + `Ping` is an UNCLEAN EXIT — the killed process left the
    // file behind while its unsaved buffer's phantom projections remain in the
    // derived `db.lbug`. Never serve that stale index: reclaim the stale socket
    // and force the full scan that discards and rebuilds `db.lbug` from the
    // durable `apg/layers/**` node files (the same recovery `abort` drives,
    // reusing the existing scan entry point), then fall through to the
    // read-only open below. The guard runs BEFORE that open, so the read can
    // never touch the phantom index. A live session (above) still routes, and
    // an absent socket is the ordinary direct path.
    let socket = session::socket_path(&apg_root);
    if socket.exists() && !session::live_session_at(&socket) {
        session::Coordinator::reclaim_stale_socket(&apg_root)?;
    }

    let db_path = apg_root.join(specs::TRANS).join("db.lbug");
    if !db_path.exists() {
        anyhow::bail!(
            "{} does not exist — run `apg scan` first",
            db_path.display()
        );
    }
    let db = Database::new(&db_path, SystemConfig::default().read_only(true))?;
    println!("{}", render_query(&db, &query, json)?);
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
