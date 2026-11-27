//! `apg project` — the projects model commands (apg-projects R1/R2/R5):
//!
//! - `apg project start <name>` — one command creates the project context:
//!   a git worktree + branch off the repo's DEFAULT branch (the main
//!   checkout's symbolic HEAD, not literal main), at the fixed location
//!   `<main>/apg/.worktrees/<name>`, seeded by copying the main checkout's
//!   `apg/.trans` scan into the branch (worktree + branch + branch DB; no
//!   frontend scan). Because the seed is main's scan, start refuses when the
//!   main checkout's scan is stale or missing for main's HEAD — run
//!   `apg scan` in the main checkout first. Always a branch — no escape
//!   hatch. Idempotent only when `<name>` already IS the current project
//!   context (re-run inside the project's worktree → no-op, prints the path);
//!   otherwise every refusal is a hard fail naming the actual state and one
//!   fix command (R2). Project names are never sanitized. Start is a
//!   layout-touching op: the R10 version gate blocks layouts whose
//!   `apg/config.json` version is missing or does not share the binary's
//!   major.minor (in either direction), and the R9 self-heal scaffolds the
//!   worktrees dir + the `.gitignore` entries when absent.
//!
//! - `apg project merge <name>` — the project's terminal lifecycle act:
//!   verify gate (plan verify against the branch graph: every planned node
//!   realized, no dangling targets, all feedback resolved) → merge the
//!   project branch into the default branch → rebuild main's graph with a
//!   plain unguarded scan → **self-cleanup**: on that success path only, the
//!   project's worktree at `<main>/apg/.worktrees/<name>` is removed and its
//!   branch deleted (never the default branch). Binary-operated via git2
//!   from the main checkout; the git CLI is never shelled out to (R6);
//!   push/tag remain human-approved acts.
//!
//! - `apg project delete <name>` — the explicit abandon act for projects
//!   that are NOT merged. Every refusal names the actual state + one fix
//!   command (the refuse_start pattern); the branch's commits are discarded —
//!   delete does NOT require the branch to be merged (delete-refuses-unsafe).
//!   On the success path the project's worktree at
//!   `<main>/apg/.worktrees/<name>` is removed and its branch deleted via the
//!   shared phase-1 helpers; the default branch and the main checkout are
//!   never touched (never-touch-default-branch).
//!
//! - `apg project resolve <name>` — print the registered worktree path for
//!   `<name>` on stdout, reading the main checkout's registry. A missing or
//!   unregistered row is refused naming the project, the registry path and the
//!   registered project names.
//!
//! The cohesive groups live in submodules — the start path ([`start`]), the
//! merge path ([`merge`]), the abandon path ([`delete`]), and the start-time
//! toolchain-cache seeder ([`cache_copy`]) — all re-exported here so
//! `crate::project_cmd::<name>` keeps resolving.

use std::path::PathBuf;

use crate::git;
use crate::specs;

pub mod cache_copy;
pub mod delete;
pub mod merge;
pub mod start;

#[cfg(test)]
mod tests;

pub use cache_copy::*;
pub use delete::*;
pub use merge::*;
pub use start::*;

// The crate-internal CLI wrappers cannot travel through a `pub use …::*` glob
// (their visibility is narrower than `pub`); re-export them explicitly so
// `cmd_project` reaches them through `crate::project_cmd::<name>`.
pub(crate) use delete::project_delete;
pub(crate) use merge::project_merge;
pub(crate) use start::project_start;

/// `apg project <start|merge|delete|resolve> …`.
pub fn cmd_project(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg project <start|merge|delete|resolve> …");
    };
    match sub {
        "start" => project_start(&args[1..]),
        "merge" => project_merge(&args[1..]),
        "delete" => project_delete(&args[1..]),
        "resolve" => project_resolve(&args[1..]),
        other => anyhow::bail!("unknown apg project subcommand: {other}"),
    }
}

/// `apg project resolve <name>` — print the registered worktree path on stdout,
/// and nothing else. Reads the **main** checkout's registry, so it works from
/// the main checkout and from inside any project worktree. Read-only: a missing
/// registry file is an empty registry and is never created here.
pub(crate) fn project_resolve(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.first() else {
        anyhow::bail!("usage: apg project resolve <name>");
    };
    let apg_root = require_apg_root()?;
    let identity = git::repo_identity(&apg_root)?;
    let registry_path = crate::registry::registry_path(&identity.main_root);
    let registry = if registry_path.exists() {
        Some(crate::registry::Registry::open(&registry_path)?)
    } else {
        None
    };
    let entry = match &registry {
        Some(registry) => registry.get(name)?,
        None => None,
    };
    let Some(entry) = entry else {
        let mut names: Vec<String> = match &registry {
            Some(registry) => registry
                .entries()?
                .into_iter()
                .map(|entry| entry.name)
                .collect(),
            None => Vec::new(),
        };
        names.sort();
        let registered = if names.is_empty() {
            "<none>".to_string()
        } else {
            names.join(", ")
        };
        anyhow::bail!(
            "refused: project `{name}` is not registered in the project registry `{reg}` (registered projects: {registered}). Fix: start it with `apg project start {name}`, or rebuild the registry from the worktree list with `apg project reconstruct`.",
            reg = registry_path.display()
        );
    };
    println!("{}", entry.worktree.display());
    Ok(())
}

/// The layout root of the checkout the process runs in (walk-up discovery).
pub(crate) fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

/// The worktree (name, path) that has `branch` checked out, if any.
pub(crate) fn worktree_hosting(repo: &git2::Repository, branch: &str) -> Option<(String, PathBuf)> {
    let names = repo.worktrees().ok()?;
    for name in names.iter().flatten() {
        let wt = repo.find_worktree(name).ok()?;
        let path = wt.path().to_path_buf();
        let wt_repo = git2::Repository::open(&path).ok()?;
        let head = wt_repo.head().ok()?;
        if head.shorthand() == Some(branch) {
            return Some((name.to_string(), path));
        }
    }
    None
}

/// Project-name validation (R2): names inherit git refname constraints and are
/// never sanitized — an invalid name is refused as-is. Project names are a
/// single path segment on top of that: the worktree location
/// `<main>/apg/.worktrees/<project>` and the spec/plan FQN space are
/// single-segment namespaces.
pub(crate) fn validate_project_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!(
            "refused: project name is empty. Fix: `apg project start <name>` with a valid branch name."
        );
    }
    if name.contains('/') {
        anyhow::bail!(
            "refused: `{name}` is not a valid project name — project names are a single path segment (no '/'). Fix: choose a single-segment name (`apg project start <name>`)."
        );
    }
    if name == "HEAD" {
        anyhow::bail!(
            "refused: `{name}` is not a valid project (branch) name — `HEAD` is reserved. Fix: choose a valid name and re-run `apg project start <name>`."
        );
    }
    if !git2::Reference::is_valid_name(&format!("refs/heads/{name}")) {
        anyhow::bail!(
            "refused: `{name}` is not a valid project (branch) name — project names inherit git refname constraints, nothing is sanitized. Fix: choose a valid name and re-run `apg project start <name>`."
        );
    }
    Ok(())
}
