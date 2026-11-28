//! The `apg` library: the root crate's production module tree and its
//! crate-internal API. The binary (`src/main.rs`) is a thin wrapper that
//! dispatches into the library's entry point; the inline white-box tests live
//! here with the modules they exercise.
//!
//! Every module is public so integration crates and the test tiers can reach
//! the internals they exercise. `testutil` is the shared test harness and
//! compiles unconditionally as a public module.

pub mod artifacts;
pub mod cache;
pub mod classify;
pub mod cleanup;
pub mod cobol;
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
pub mod progress;
pub mod project_cmd;
pub mod ps;
pub mod query;
pub mod registry;
pub mod review_cmd;
pub mod scan;
pub mod schema;
pub mod session;
pub mod spec_lint;
pub mod specs;
pub mod splice;
pub mod testutil;
pub mod timing;
pub mod version_gate;
pub mod warm;

// The cohesive submodules above are re-exported here so the crate-root surface
// every integration crate and test tier reaches (`apg::<name>`) stays at the
// root.
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

// The root test tier (`src/tests.rs`) reaches `BTreeSet` through
// `use super::*;`; gate it to the test build so the non-test library has no
// unused import.
#[cfg(test)]
pub(crate) use std::collections::BTreeSet;

/// The `apg --help` text. Split from [`print_help`] so the strict add|update|rm
/// surface and the unchanged `apg review` line can be asserted directly by the
/// help/dispatch acceptance test (no stdout capture).
pub fn help_text() -> String {
    "apg — program graph scanner + LadybugDB query CLI for opencode

USAGE:
  apg init [dir]              Set up apg/ (config.json carrying the binary
                              version + .trans/ + .worktrees/ + the
                              changelog-sliced migration-guide.md), scaffold the
                              repo .gitignore for the apg layout entries,
                              install/update the opencode apg tool suite + nine
                              distributed agents + the upgrade guide in
                              ~/.config/opencode/, and warn loudly about project
                              .opencode/ files that duplicate the installed
                              suite (never deletes)
  apg scan [dir] [options]    Scan a project; writes apg/.trans/db.lbug
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
                              refuses unsafe states, never the default branch);
                              the main checkout's registry
                              apg/.trans/projects.db maps name → worktree;
                              start claims a row, merge/delete remove it;
                              resolve <name> — print the registered worktree
                              path; reconstruct — rebuild the registry from
                              git worktree list
  apg review <sub> …          Writer↔reviewer feedback cycle:
                              add/action/resolve/reject/list
  apg spec <sub> …            Deterministic, read-only durable-spec lint:
                              lint — report the R3/R4 rule violations and
                              the shared structural validator set as errors,
                              plus the non-blocking tier-1-3 wording advisories
  apg node <sub> …            Durable node-file model mutations:
                              add/update/rm (type-as-argument, writes apg/layers;
                              the name is identity and is never updatable)
  apg edge <sub> …            Durable node-file model edge mutations:
                              add/update/rm (kind/from/to; update is
                              properties-only)
                              Each invocation opens a read-only session; a
                              durable apg node/edge write upgrades under the
                              worktree's blocking lock for one critical
                              section — it writes its node files, commits
                              once and projects into db.lbug — then releases
                              back to read-only. Reads never take the lock.
  apg --version               Print version
  apg --help                  Show this help

SCAN OPTIONS:
  --language <lang>            Scanner language(s): java, go, cpp, rust, ts,
                               csharp, py, cobol, sql, ps (comma-separated or
                               repeated; auto-detected for every language
                               present if omitted)
  --exclude-path <glob>       Exclude path patterns (repeatable)
  --module <dir>              Restrict the scan to this directory and its
                               subtree (repeatable): a relative value resolves
                               against the scan root, an absolute value passes
                               through unchanged, and a missing directory or a
                               non-directory fails the scan with a named error.
                               Honoured by every frontend apg ships — the code
                               languages (java, go, cpp, rust, ts/js, csharp,
                               py, cobol, sql, ps), the agent-definition
                               frontend and every bundled structural stream
                               (md, sh, yaml, json, toml, xml, dockerfile,
                               makefile, ini, sln, misc). TypeScript selects
                               whole npm packages: a value at or above a
                               package root selects that package's subtree, a
                               value inside a package (e.g. packages/app/src)
                               selects nothing.
  --std <dialect>             COBOL dialect: cobol85 (default), cobol2002,
                               cobol2014 or ibm (overrides cobol.dialect in
                               apg/config.json; cobol2023 is refused)
  --copy-path <dir>           COBOL copybook search directory (repeatable)
  --no-build-scripts          Rust only: skip cargo build scripts and the
                              proc-macro server (hermetic scans)
  --no-progress               Progress is on by default when stderr is a
                              terminal; this flag turns off the start line,
                              the per-node symbols and the summary
  --force                     Rebuild from scratch: discard db.lbug, then
                              re-run every frontend rather than assembling
                              from the shared fact cache; apg/layers and
                              source files are untouched
  <blacklist...>              FQN prefixes to exclude from the graph"
        .to_string()
}

fn print_help() {
    println!("{}", help_text());
}

/// The `apg` entry point: dispatches the CLI subcommands (`init`, `query`,
/// `scan`, `plan`, `review`, `spec`, `project`, `node`, `edge`), prints help
/// for `--help`/no args, and turns a returned error into a non-zero exit. The
/// binary embeds the apg opencode suite — the tool set (`SUITE_TOOLS`) and the
/// nine distributed agents (`AGENTS`) delivered by `apg init` — whose prompts
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
        "spec" => spec_lint::cmd_spec(&raw[2..]),
        "project" => project_cmd::cmd_project(&raw[2..]),
        "node" => node_cmd::cmd_node(&raw[2..]),
        "edge" => node_cmd::cmd_edge(&raw[2..]),
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
