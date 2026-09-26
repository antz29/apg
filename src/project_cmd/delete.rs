//! `apg project delete` — the explicit abandon act for an unmerged project.

use std::path::Path;

use crate::artifacts::parse_args;
use crate::git;

use super::{require_apg_root, validate_project_name};

/// `apg project delete <name>` — the explicit abandon act for a project that
/// is NOT merged. Refuses (naming the actual state + one fix command) unless
/// deleting the branch is safe; on success removes the project's worktree at
/// `<main>/apg/.worktrees/<name>` and deletes `refs/heads/<name>` via the
/// shared phase-1 helpers. Discarding the branch's commits is the point —
/// delete does NOT require the branch to be merged.
pub(crate) fn project_delete(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(name) = p.positional.first() else {
        anyhow::bail!("usage: apg project delete <name>");
    };
    let apg_root = require_apg_root()?;
    project_delete_at(&apg_root, name)?;
    println!(
        "Deleted project `{name}`: removed its worktree and deleted branch `{name}` (its commits were discarded)."
    );
    Ok(())
}

/// Core of `project delete` (split from the CLI wrapper so tests drive it
/// against a fixture root). The refusal preflight mirrors `refuse_start`: each
/// refusal names the actual state and one fix command (delete-refuses-unsafe).
/// Safe-to-delete means: valid project name; the branch is NOT the default
/// branch; the branch is NOT the main checkout's current branch (there is
/// nothing to delete from here); the branch IS hosted in the project's own
/// worktree at the fixed location `<main>/apg/.worktrees/<name>` (leftover
/// branch / mismatched worktree states are refused with a manual fix, never
/// guessed); and the worktree has no tracked uncommitted changes (commit or
/// stash first, mirroring merge's dirty refusal). The branch may be unmerged —
/// delete is the abandon path. On success `git::remove_worktree` +
/// `git::delete_branch` remove the project; the default branch and the main
/// checkout are never touched.
pub fn project_delete_at(main_apg_root: &Path, name: &str) -> anyhow::Result<()> {
    let identity = git::repo_identity(main_apg_root)?;
    if identity.is_worktree {
        anyhow::bail!(
            "refused: `apg project delete` is a main-checkout operation (binary-operated from the main checkout). Fix: run `apg project delete {name}` from the main checkout."
        );
    }

    // Name validity (delete-refuses-unsafe AC-a: refuses for an invalid name).
    validate_project_name(name)?;

    // Safety law: the default branch and the main checkout are never touched
    // (never-touch-default-branch). Deleting the default branch itself is a
    // hard refusal, before anything else looks at the project.
    let Some(default) = identity.default_branch.as_deref() else {
        anyhow::bail!(
            "refused: the repo has no default branch (no origin/HEAD and the main checkout is detached). Fix: check out the default branch first."
        );
    };
    if default == name {
        anyhow::bail!(
            "refused: `{name}` is the repo's default branch and is never deleted by lifecycle cleanup. Fix: work on it where it is (`git checkout {default}`), and abandon projects with `apg project delete <name>`."
        );
    }

    // The main checkout's current branch cannot be deleted (a checked-out
    // branch cannot be removed — delete-refuses-unsafe AC-b); there is
    // nothing to delete from here anyway.
    if identity.branch.as_deref() == Some(name) {
        anyhow::bail!(
            "refused: project `{name}` cannot be deleted from here — branch `{name}` is the main checkout's current branch (the branch-without-worktree state). Fix: `git checkout {default}`, then re-run `apg project delete {name}`.",
            default = default
        );
    }

    // The project must exist: the branch exists and is hosted in the project's
    // own worktree at the fixed location. A missing branch is a hard refusal
    // (AC-c — no project); a branch not hosted in the fixed worktree is a
    // leftover-branch / mismatched-worktree state, refused with a manual fix
    // (AC-d), never guessed.
    let wt_dir = git::project_worktree_dir(&identity.main_root, name);
    let repo = git2::Repository::open(&identity.main_root)?;
    if repo.find_branch(name, git2::BranchType::Local).is_err() {
        anyhow::bail!(
            "no project `{name}`: branch `{name}` does not exist. Fix: `apg project start {name}` from the main checkout."
        );
    }
    if !wt_dir.is_dir() {
        anyhow::bail!(
            "no project `{name}`: expected worktree {} does not exist. Fix: `apg project start {name}` from the main checkout (a project worktree lives at the fixed location).",
            wt_dir.display()
        );
    }
    let wt_repo = git2::Repository::open(&wt_dir)?;
    let on_branch = wt_repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_string));
    if on_branch.as_deref() != Some(name) {
        anyhow::bail!(
            "refused: {} is not branch `{name}`'s worktree (it holds `{}`) — a mismatched-worktree state is never guessed. Fix: decide manually (remove the stale directory or check out the branch into it: `git worktree remove {name}` / `git worktree add {} {name}`), then re-run `apg project delete {name}`.",
            wt_dir.display(),
            on_branch.as_deref().unwrap_or("<none>"),
            wt_dir.display()
        );
    }

    // The worktree must have no tracked uncommitted changes: delete discards
    // the branch's commits, and uncommitted work in the worktree would be
    // destroyed with them (delete-refuses-unsafe AC-e).
    if !git::checkout_clean(&wt_dir) {
        anyhow::bail!(
            "refused: the project worktree has tracked uncommitted changes — deleting the project would discard them (the branch's commits are already the abandon act; this would destroy work not in any commit). Fix: commit or stash them (`git status` inside {}), then re-run `apg project delete {name}`.",
            wt_dir.display()
        );
    }

    // The branch may be unmerged — delete is the explicit abandon path and
    // discarding the branch's commits is its purpose (delete-subcommand AC-c).
    // Worktree first, then the branch: libgit2 refuses to delete a branch
    // that is still a linked worktree's HEAD. delete_branch hard-refuses the
    // default branch again (the safety law's last line of defense).
    git::remove_worktree(&identity.main_root, name)?;
    git::delete_branch(&identity.main_root, name)?;
    Ok(())
}
