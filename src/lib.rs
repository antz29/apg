//! The `apg` library: the root crate's production module tree and its
//! crate-internal API. The binary (`src/main.rs`) is a thin wrapper that
//! dispatches into the library's entry point; the inline white-box tests live
//! here with the modules they exercise.
//!
//! Every module is public so integration crates (and the relocated test tiers)
//! can reach the internals they exercise. `testutil` is the shared test
//! harness and compiles unconditionally as a public module.

pub mod artifacts;
pub mod cache;
pub mod classify;
pub mod cleanup;
pub mod delta;
pub mod frontends;
pub mod git;
pub mod graph;
pub mod impact;
pub mod incremental;
pub mod ingest;
pub mod install;
pub mod layers;
pub mod load;
pub mod logging;
pub mod node_cmd;
pub mod pipeline;
pub mod plan_cmd;
pub mod project_cmd;
pub mod query;
pub mod review_cmd;
pub mod scan;
pub mod schema;
pub mod session;
pub mod specs;
pub mod splice;
pub mod testutil;
pub mod timing;
pub mod version_gate;
pub mod warm;

// The cohesive submodules above were split out of this file (phase-04
// decomposition). Re-export their public items so the crate-root surface every
// integration crate and test tier reaches (`apg::<name>`) is unchanged.
pub use frontends::*;
pub use install::*;
pub use logging::*;
pub use pipeline::*;
pub use query::*;
pub use scan::*;
pub use warm::*;

// The genuinely crate-only entry points, re-exported at crate visibility (they
// are reached by `src/main.rs`'s dispatch and by sibling modules as
// `crate::<name>`, never by an integration crate).
pub(crate) use install::cmd_init;
pub(crate) use pipeline::run_pipeline;
pub(crate) use query::{cmd_query, render_query};
pub(crate) use scan::cmd_scan;

use std::path::{Path, PathBuf};

// The relocated root test tier (`src/tests.rs`) reaches `BTreeSet` through
// `use super::*;`; gate it to the test build so the non-test library has no
// unused import.
#[cfg(test)]
pub(crate) use std::collections::BTreeSet;

/// The `apg --help` text. Split from [`print_help`] so the strict add|update|rm
/// surface and the unchanged `apg review` line can be asserted directly by the
/// Phase-7 help/dispatch acceptance test (no stdout capture).
pub fn help_text() -> String {
    "apg — program graph scanner + LadybugDB query CLI for opencode

USAGE:
  apg init [dir]              Set up apg/ (config.json carrying the binary
                              version + .trans/ + .worktrees/), scaffold the
                              repo .gitignore for the apg layout entries,
                              install/update the opencode apg tool suite + seven
                              distributed agents + the upgrade guide in
                              ~/.opencode/, and warn loudly about project
                              .opencode/ files that duplicate the installed
                              suite (never deletes)
  apg scan [dir] [options]    Scan a project; writes apg/.trans/db.lbug and
                              apg/.trans/graph.jsonl
  apg query [--json] \"<cypher>\"  Run a read-only Cypher query against
                               apg/.trans/db.lbug (found by walking up from
                               cwd); CSV by default, --json for JSON rows
  apg plan <sub> …            The phased execution plan (transient, branch-local):
                              add/update/rm/done/undone/note/complete/render/verify
                              (add <project> creates the plan, then authors
                              phases, tasks, and planned Implementation nodes —
                              module/file/struct/function marked planned at the
                              FQN where the code lands; update edits in place;
                              rm refuses while dependents exist unless --force;
                              verify is the pre-merge coherence gate)
  apg project <sub> …         Project contexts (worktrees, git2-operated):
                              start <name> — worktree + branch + branch DB off
                              the default branch (apg/.worktrees/<name>);
                              merge <name> — verify gate → merge → main rebuild;
                              delete <name> — abandon a project: remove its
                              worktree + delete its branch (commits discarded;
                              refuses unsafe states, never the default branch)
  apg review <sub> …          Writer↔reviewer feedback cycle:
                              add/action/resolve/reject/list
  apg node <sub> …            Durable node-file model mutations:
                              add/update/rm (type-as-argument, writes apg/layers;
                              the name is identity and is never updatable)
  apg edge <sub> …            Durable node-file model edge mutations:
                              add/update/rm (kind/from/to; update is
                              properties-only)
  apg session <sub> …         Session-scoped single-writer coordinator
                              (apg/.trans/session.sock):
                              start — own db.lbug exclusively, perform routed
                              node/edge mutations and serve routed reads in
                              receive order (write-through, no buffered flush);
                              end — signal the live session to release the
                              DB/socket and exit
  apg --version               Print version
  apg --help                  Show this help

SCAN OPTIONS:
  --language <lang>            Scanner language(s): java, go, cpp, rust, ts,
                               csharp, py (comma-separated or repeated;
                               auto-detected for every language present if
                               omitted)
  --exclude-path <glob>       Exclude path patterns (repeatable)
  --module <dir>              Restrict scanning to a module (Go/C++/Rust/TS/C#,
                               repeatable)
  --no-build-scripts          Rust only: skip cargo build scripts and the
                              proc-macro server (hermetic scans)
  <blacklist...>              FQN prefixes to exclude from the graph"
        .to_string()
}

fn print_help() {
    println!("{}", help_text());
}

/// `apg session <start|end>` (phase-03): `start` launches the session-scoped
/// single-writer coordinator (bind socket, own `db.lbug`, serve routed
/// mutations/reads); `end` signals the running session — the server-side
/// shutdown releases the DB/socket and `serve` exits. Every mutation's
/// projection delta was already applied write-through, so `end` performs no
/// flush.
fn session_cmd(args: &[String]) -> anyhow::Result<()> {
    let apg_root = session::require_apg_root()?;
    match args.first().map(|s| s.as_str()) {
        Some("start") => session::Coordinator::start(&apg_root),
        Some("end") => session::Coordinator::signal_end(&apg_root),
        other => anyhow::bail!(
            "usage: apg session <start|end> (got `{}`)",
            other.unwrap_or("<none>")
        ),
    }
}

/// The `apg` entry point: dispatches the CLI subcommands (`init`, `query`,
/// `scan`, `plan`, `review`, `project`, `node`, `edge`, `session`), prints help
/// for `--help`/no args, and turns a returned error into a non-zero exit. The
/// binary embeds the apg opencode suite — the tool set (`SUITE_TOOLS`) and the
/// seven distributed agents (`AGENTS`) delivered by `apg init` — whose prompts
/// carry the coordinator-mediated feedback cycle: the owning writer returns an
/// ACTIONED/WONT-FIX claim and the coordinator performs the shallow
/// claim-vs-change consistency check and then actions the item.
pub fn main() {
    let raw: Vec<String> = std::env::args().collect();
    if raw.len() < 2 {
        print_help();
        std::process::exit(2);
    }
    let status = match raw[1].as_str() {
        "init" => cmd_init(&raw[2..]),
        "query" => cmd_query(&raw[2..]),
        "scan" => cmd_scan(&raw[2..]),
        "plan" => plan_cmd::cmd_plan(&raw[2..]),
        "review" => review_cmd::cmd_review(&raw[2..]),
        "project" => project_cmd::cmd_project(&raw[2..]),
        "node" => node_cmd::cmd_node(&raw[2..]),
        "edge" => node_cmd::cmd_edge(&raw[2..]),
        "session" => session_cmd(&raw[2..]),
        "--version" | "-V" => {
            println!("apg {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("apg: unknown subcommand: {other}");
            print_help();
            Err(anyhow::anyhow!("unknown subcommand: {other}"))
        }
    };
    if let Err(e) = status {
        eprintln!("apg: {e}");
        std::process::exit(1);
    }
}

/// Walks up from `start` looking for the committed `apg/` layout root.
fn find_apg_root(start: &Path) -> Option<PathBuf> {
    specs::find_apg_root(start)
}

/// Finds the project's `apg/` layout root (walking up from the scanned dir) or
/// creates one at `<dir>/apg` (with `.trans/`) if none exists.
fn find_or_create_apg_root(dir: &Path) -> PathBuf {
    specs::find_or_create_apg_root(dir)
}

#[cfg(test)]
mod tests;
