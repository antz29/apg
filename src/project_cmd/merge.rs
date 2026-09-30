//! `apg project merge` — verify gate → merge → main rebuild → self-cleanup.

use std::path::Path;

use crate::artifacts::parse_args;
use crate::git;
use crate::plan_cmd;
use crate::specs;

use super::require_apg_root;
use super::start::{ScanFn, real_scan};

/// `apg project merge <name>` — verify gate → merge → main rebuild.
pub(crate) fn project_merge(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(name) = p.positional.first() else {
        anyhow::bail!("usage: apg project merge <name>");
    };
    let apg_root = require_apg_root()?;
    project_merge_at(&apg_root, name, None)?;
    println!("Merged project `{name}` into the default branch.");
    Ok(())
}

/// Core of `project merge` (split for tests). The default `rebuild` is a real
/// unguarded `apg scan` of the main checkout; tests override it. After the
/// verify gate → merge → rebuild succeed, the merged project cleans up after
/// itself: its worktree is removed and its branch deleted (self-cleanup;
/// never the default branch). Any refusal or failure before the success path
/// leaves both untouched.
pub fn project_merge_at(
    main_apg_root: &Path,
    name: &str,
    rebuild: Option<&ScanFn>,
) -> anyhow::Result<()> {
    let identity = git::repo_identity(main_apg_root)?;
    if identity.is_worktree {
        anyhow::bail!(
            "refused: `apg project merge` is a main-checkout operation (binary-operated from the main checkout). Fix: run `apg project merge {name}` from the main checkout."
        );
    }
    let Some(head_branch) = identity.branch.as_deref() else {
        anyhow::bail!(
            "refused: the main checkout is not on a branch (unborn or detached HEAD). Fix: check out the default branch first."
        );
    };
    if head_branch == name {
        anyhow::bail!(
            "refused: project `{name}` cannot be merged from here — branch `{name}` is the main checkout's current branch (the bootstrap state; there is nothing to merge into from this checkout). Fix: merge it with git once it is ready (`git checkout {default} && git merge {name}`), or run `apg project merge {name}` after the main checkout holds the default branch.",
            default = identity.default_branch.as_deref().unwrap_or("<default>")
        );
    }
    let Some(default) = identity.default_branch.as_deref() else {
        anyhow::bail!(
            "refused: the repo has no default branch (no origin/HEAD and the main checkout is detached). Fix: check out the default branch first."
        );
    };
    if head_branch != default {
        anyhow::bail!(
            "refused: the main checkout must be on the default branch (`{default}`) to merge a project — it is on `{head_branch}`. Fix: `git checkout {default}`, then re-run `apg project merge {name}`."
        );
    }

    // The project must exist: branch + worktree at the fixed location, with
    // the branch checked out in it.
    let wt_dir = git::project_worktree_dir(&identity.main_root, name);
    let repo = git2::Repository::open(&identity.main_root)?;
    let branch = repo.find_branch(name, git2::BranchType::Local).map_err(|_| {
        anyhow::anyhow!(
            "no project `{name}`: branch `{name}` does not exist. Fix: `apg project start {name}` from the main checkout."
        )
    })?;
    if !wt_dir.is_dir() {
        anyhow::bail!(
            "no project `{name}`: expected worktree {} does not exist. Fix: `apg project start {name}` from the main checkout.",
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
            "refused: {} is not branch `{name}`'s worktree (it holds `{}`). Fix: `apg project start {name}` from the main checkout (after removing the stale directory), then re-run `apg project merge {name}`.",
            wt_dir.display(),
            on_branch.as_deref().unwrap_or("<none>")
        );
    }

    // The main checkout must be clean before the merge updates it.
    if !git::checkout_clean(&identity.main_root) {
        anyhow::bail!(
            "refused: the main checkout is dirty — the merge rewrites it. Fix: commit or stash your changes (`git status`), then re-run `apg project merge {name}`."
        );
    }

    // Verify gate: the branch graph must be fresh, and the coherence gate must
    // pass — no remaining planned/dangling nodes, all feedback resolved
    // (R5). `plan_verify_at` also refuses when the branch DB is stale (a
    // verdict is only meaningful against a fresh branch graph).
    let wt_apg = wt_dir.join(specs::LAYOUT);
    // Lifecycle exclusivity: a live session owns the branch DB, so merging
    // (which removes the worktree/branch on success) must not race it. Refuse
    // BEFORE the merge/rebuild replaces any projected DB — the caller must save
    // the buffered state (`apg session save`) and end the session
    // (`apg session end`) first.
    if crate::session::live_session(&wt_apg) {
        anyhow::bail!(
            "refused: a live `apg session` owns the branch DB at {} — save its buffered changes and end it first (`apg session save`, then `apg session end`, inside the project worktree) before merging",
            wt_apg.join(specs::TRANS).join("db.lbug").display()
        );
    }
    if !wt_apg.join(specs::TRANS).join("db.lbug").exists() {
        anyhow::bail!(
            "refused: project `{name}` has no branch graph — run `apg scan` inside {} first (a verdict is only meaningful against the branch's graph).",
            wt_dir.display()
        );
    }
    plan_cmd::plan_verify_at(&wt_apg, name)?;

    // Merge the project branch into the default branch (git2, from the main
    // checkout): fast-forward when the default has not moved, otherwise a
    // merge commit. Conflicts are a hard refusal with a manual fix line.
    let head_commit = repo.head()?.peel_to_commit()?;
    let their = branch.get().peel_to_commit()?;
    if their.id() == head_commit.id() {
        anyhow::bail!(
            "nothing to merge: project `{name}` has no commits beyond the default branch. Fix: work inside {} and commit changes first.",
            wt_dir.display()
        );
    }
    let merge_base = repo.merge_base(head_commit.id(), their.id())?;
    let msg = format!("Merge project {name} (verify gate passed)");
    if merge_base == head_commit.id() {
        // Fast-forward: move the default branch ref and sync the checkout.
        let mut default_ref = repo.find_reference(&format!("refs/heads/{default}"))?;
        default_ref.set_target(their.id(), &msg)?;
        repo.checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))?;
    } else {
        // Merge commit.
        let mut index = repo.merge_commits(&head_commit, &their, None)?;
        if index.has_conflicts() {
            anyhow::bail!(
                "refused: merging `{name}` into `{default}` conflicts. Fix: resolve the conflicts manually (`git status` shows them), commit the merge, then re-run `apg project merge {name}` (or rebase `{name}` onto `{default}` first)."
            );
        }
        let tree_id = index.write_tree_to(&repo)?;
        let tree = repo.find_tree(tree_id)?;
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("apg", "apg@localhost"))?;
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &msg,
            &tree,
            &[&head_commit, &their],
        )?;
        repo.checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))?;
    }

    // Main rebuild: a plain unguarded scan on main. Cleanup runs ONLY on the
    // success path — any refusal or failure before this point leaves the
    // worktree and branch untouched. The worktree is unregistered + removed
    // and the project branch deleted (safe: the merged branch is by
    // definition merged into the default branch; the default branch itself is
    // hard-refused inside delete_branch).
    let rebuild = rebuild.unwrap_or(&real_scan);
    rebuild(&identity.main_root)?;
    git::remove_worktree(&identity.main_root, name)?;
    git::delete_branch(&identity.main_root, name)?;
    Ok(())
}
