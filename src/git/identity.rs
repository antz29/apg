use std::path::{Path, PathBuf};

use crate::specs;

use super::state::{canonical, discover_repo, head_sha};

// ---------------------------------------------------------------------------
// Repo identity (R7): the walk-up stays the layout resolver; git2 is identity.
// ---------------------------------------------------------------------------

/// The identity of the checkout that contains a walked-up `apg/` layout root.
#[derive(Debug, Clone)]
pub struct RepoIdentity {
    /// Root of the **main** checkout (the repo's original working tree; a
    /// linked worktree's `commondir` parent).
    pub main_root: PathBuf,
    /// Root of the checkout that actually contains the layout — equal to
    /// `main_root` unless the layout lives inside a linked project worktree.
    pub checkout_root: PathBuf,
    /// True when the containing checkout is a linked worktree (≠ main).
    pub is_worktree: bool,
    /// The current branch's shorthand name (`None` when detached or unborn).
    pub branch: Option<String>,
    /// The symbolic HEAD of the **main** checkout — the repo's DEFAULT branch
    /// (R1: project branches come off it; `None` when the main checkout is
    /// detached or unborn).
    pub default_branch: Option<String>,
    /// The current HEAD commit sha (`None` when unborn).
    pub head_sha: Option<String>,
}

/// Resolves the identity of the git checkout containing the walked-up layout
/// root `apg_root` (R7). Verifies the invariant cheaply: the checkout root
/// must contain the walked-up `apg/` — an `apg_root` that git does not see as
/// inside the checkout is a divergence and an error.
pub fn repo_identity(apg_root: &Path) -> anyhow::Result<RepoIdentity> {
    let repo = discover_repo(apg_root)?;
    if repo.is_bare() {
        anyhow::bail!(
            "{} is inside a bare git repository — a working checkout is required",
            apg_root.display()
        );
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("repository has no working directory"))?;
    let checkout_root = canonical(workdir);

    // Main checkout root: a linked worktree's gitdir is
    // `<main>/.git/worktrees/<name>`; the main checkout's gitdir is
    // `<main>/.git`. Deriving from the gitdir keeps the worktree case exact.
    let gitdir = canonical(repo.path());
    let gitdir_parent = gitdir.parent().unwrap_or(&gitdir);
    let main_root = if gitdir_parent.file_name().is_some_and(|n| n == "worktrees") {
        // <main>/.git/worktrees → <main>/.git → <main>
        canonical(
            gitdir_parent
                .parent()
                .and_then(|g| g.parent())
                .unwrap_or(&gitdir),
        )
    } else {
        checkout_root.clone()
    };

    // R7 invariant: the walked-up apg/ must sit inside the checkout.
    let layout = canonical(apg_root);
    if !layout.starts_with(&checkout_root) {
        anyhow::bail!(
            "layout divergence: the walked-up apg/ ({}) is not inside the git checkout ({}) — move the apg/ layout under the checkout root",
            layout.display(),
            checkout_root.display()
        );
    }

    let head = repo.head().ok();
    let detached = repo.head_detached().unwrap_or(true);
    let branch = match &head {
        Some(h) if !detached => h.shorthand().map(str::to_string),
        _ => None,
    };
    let head_sha = head_sha(&repo);

    // The DEFAULT branch. Resolved origin-first: `refs/remotes/origin/HEAD` is
    // the repo's true default and stays authoritative even while the main
    // checkout itself holds a project branch (the R20 bootstrap state — the
    // branch-without-worktree case git cannot host anywhere else). Without an
    // origin default, the main checkout's own symbolic HEAD is the fallback.
    let default_branch =
        origin_default_branch(&main_root).or_else(|| main_checkout_head(&main_root));

    Ok(RepoIdentity {
        is_worktree: checkout_root != main_root,
        main_root,
        checkout_root,
        branch,
        default_branch,
        head_sha,
    })
}

/// The repo's true default branch: the symbolic target of
/// `refs/remotes/origin/HEAD` (`refs/heads/main` or `refs/remotes/origin/main`).
fn origin_default_branch(main_root: &Path) -> Option<String> {
    let repo = git2::Repository::open(main_root).ok()?;
    let head = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    let target = head.symbolic_target()?;
    target
        .strip_prefix("refs/heads/")
        .or_else(|| target.strip_prefix("refs/remotes/origin/"))
        .map(str::to_string)
}

/// The main checkout's own symbolic HEAD branch (its checked-out branch).
fn main_checkout_head(main_root: &Path) -> Option<String> {
    let repo = git2::Repository::open(main_root).ok()?;
    let head = repo.head().ok()?;
    if repo.head_detached().unwrap_or(true) {
        return None;
    }
    head.shorthand().map(str::to_string)
}

// ---------------------------------------------------------------------------
// Membership (R3/R4): writes only happen in the project's worktree on the
// project's branch. Reads are always unguarded.
// ---------------------------------------------------------------------------

/// The gitignored directory under the layout that hosts project worktrees.
pub const WORKTREES: &str = ".worktrees";

/// The canonical project worktree location: `<main>/apg/.worktrees/<project>`.
pub fn project_worktree_dir(main_root: &Path, project: &str) -> PathBuf {
    main_root.join(specs::LAYOUT).join(WORKTREES).join(project)
}

// ---------------------------------------------------------------------------
// Lifecycle self-cleanup (merge self-cleanup / delete subcommand): the
// shared git2 helpers the lifecycle flows call. Both tolerate "already gone"
// states — the merge success path cleans up only after a fully successful
// verify → merge → rebuild; a partially-gone project must not turn a
// successful merge into a failure.
// ---------------------------------------------------------------------------

/// Removes the project worktree `project` of the repo containing `main_root`:
/// its registration (the admin gitdir under `<main>/.git/worktrees/<name>`)
/// and its working directory at `<main>/apg/.worktrees/<project>`. Wired to
/// libgit2's `git_worktree_prune` with `prune.valid` set (a merged worktree is
/// valid — the branch still exists until [`delete_branch`] runs) and
/// `prune.working_tree` set (remove the working directory). Tolerates missing,
/// unregistered, or already-pruned worktrees as safe no-ops: a project whose
/// worktree is already gone must still merge cleanly.
///
/// Never touches the main checkout's own worktree — this removes the **linked**
/// worktree named `project`, and the name is validated to be a single-segment
/// local branch name first (callers additionally refuse the default branch
/// before this ever runs).
pub fn remove_worktree(main_root: &Path, project: &str) -> anyhow::Result<()> {
    if !git2::Reference::is_valid_name(&format!("refs/heads/{project}"))
        || project.contains('/')
        || project == "HEAD"
    {
        anyhow::bail!(
            "refused: `{project}` is not a valid project (branch) name — nothing was removed. Fix: use the name `apg project start` accepted."
        );
    }
    let repo = git2::Repository::open(main_root)?;
    let Some(wt) = repo.find_worktree(project).ok() else {
        // No registration to remove; the working dir (if any) is untracked
        // leftovers at the fixed location.
        return Ok(());
    };
    let mut opts = git2::WorktreePruneOptions::new();
    opts.valid(true).working_tree(true);
    if let Err(e) = wt.prune(Some(&mut opts)) {
        // A locked or in-use worktree blocks pruning; a merged-then-failed
        // cleanup must not look like a merge failure.
        anyhow::bail!(
            "cleanup failed: could not remove worktree `{project}` at {} ({e}). Fix: remove it manually (`git worktree remove {project}` or delete {}), then re-run `apg project merge {project}` to finish the cleanup.",
            wt.path().display(),
            project_worktree_dir(main_root, project).display()
        );
    }
    Ok(())
}

/// Deletes the local project branch `refs/heads/<name>` in the repo containing
/// `main_root`. Hard-refuses when `name` equals the repo's default branch —
/// the never-touch-default-branch law: `apg project merge`'s self-cleanup (and
/// `apg project delete`) may never delete `refs/heads/<default>`. Tolerates an
/// already-deleted branch as a safe no-op (a merged branch's ref may already
/// be gone). Callers must remove the project's worktree first — libgit2
/// refuses to delete a branch that is still a linked worktree's HEAD (mirrors
/// `git branch -d`).
pub fn delete_branch(main_root: &Path, name: &str) -> anyhow::Result<()> {
    let identity = repo_identity(main_root)?;
    if identity.default_branch.as_deref() == Some(name) {
        anyhow::bail!(
            "refused: `{name}` is the repo's default branch and is never deleted by lifecycle cleanup. Fix: merge it manually (`git checkout {name} && git merge main`) instead."
        );
    }
    let repo = git2::Repository::open(main_root)?;
    let mut branch = match repo.find_branch(name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    branch.delete()?;
    Ok(())
}

/// The two membership halves (R3):
///
/// - **half 1 (branch)**: the current branch is `project`; and `project` is
///   not the repo's default branch (the default branch — main — is never a
///   mutation place, delivered or not);
/// - **half 2 (worktree)**: the current checkout is the project's worktree at
///   `<main>/apg/.worktrees/<project>` — **or** the bootstrap carve-out (R20):
///   the project branch is the main checkout's own HEAD (the
///   branch-without-worktree state this change-set bootstraps in; git cannot
///   host that branch in any other checkout while the main checkout holds
///   it). The default branch is excluded from the carve-out by half 1.
fn membership_holds(identity: &RepoIdentity, project: &str) -> bool {
    identity.branch.as_deref() == Some(project)
        && identity.default_branch.as_deref() != Some(project)
        && (identity.checkout_root
            == canonical(&project_worktree_dir(&identity.main_root, project))
            || !identity.is_worktree)
}

/// The project membership guard (R3): `project` mutations may only run from
/// the project's worktree on the project's branch. Errors name which
/// membership half failed plus one fix line (exit 1 at the CLI).
pub fn require_membership(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    let identity = repo_identity(apg_root)?;
    if membership_holds(&identity, project) {
        return Ok(());
    }
    Err(membership_refusal(&identity, project))
}

/// Universal-scope mutations (e.g. the shared `_invariants.jsonl` ledger) have
/// no project argument: R3 — universal artifacts are authored from inside a
/// project like any mutation; scope is orthogonal to mutation context. Any
/// project context satisfies the guard; the default branch still never does.
pub fn require_project_context(apg_root: &Path) -> anyhow::Result<()> {
    let identity = repo_identity(apg_root)?;
    let Some(branch) = identity.branch.clone() else {
        anyhow::bail!(
            "mutation refused — no project context: the checkout is not on a branch (detached or unborn HEAD). Fix: run `apg project start <name>` from the main checkout and work inside its worktree."
        );
    };
    if membership_holds(&identity, &branch) {
        return Ok(());
    }
    Err(membership_refusal(&identity, &branch))
}

/// The two-part refusal message (R3 error UX): which membership half failed
/// plus one actionable fix line.
fn membership_refusal(identity: &RepoIdentity, project: &str) -> anyhow::Error {
    let current = identity.branch.as_deref().unwrap_or("<detached>");
    let expected = project_worktree_dir(&identity.main_root, project);
    if current != project {
        // Half 1 (branch). The default-branch flavor gets its own message:
        // main is never a mutation place.
        if identity.default_branch.as_deref() == Some(current) {
            return anyhow::anyhow!(
                "mutation refused — membership half 1 (branch) failed: you are on branch `{current}` (the repo's default branch), and the default branch is never a mutation place. Fix: run `apg project start {project}` from the main checkout and work inside its worktree."
            );
        }
        return anyhow::anyhow!(
            "mutation refused — membership half 1 (branch) failed: on branch `{current}`, but `{project}` mutations require branch `{project}`. Fix: run `apg project start {project}` from the main checkout and work inside its worktree."
        );
    }
    if identity.default_branch.as_deref() == Some(project) {
        return anyhow::anyhow!(
            "mutation refused — membership half 1 (branch) failed: `{project}` is the repo's default branch, and the default branch is never a mutation place. Fix: start a change-set project with a different name (`apg project start <name>`) from the main checkout."
        );
    }
    // Half 2 (worktree): on the right branch, but this checkout is not the
    // project's worktree (and the bootstrap carve-out does not apply — the
    // carve-out only covers the main checkout holding the project branch).
    anyhow::anyhow!(
        "mutation refused — membership half 2 (worktree) failed: {} is not the project's worktree (expected {}). Fix: work inside the project worktree, or run `apg project start {project}` from the main checkout to create it.",
        identity.checkout_root.display(),
        expected.display()
    )
}

// ---------------------------------------------------------------------------
// Identity base
// ---------------------------------------------------------------------------

/// The canonical repository-relative identity base of the checkout containing
/// `root`: the git **toplevel** (the workdir, canonicalized) when `root` is
/// inside a git repository, else `root` itself (the scan-root fallback for a
/// non-git tree). This is the single base every identity-rendering module
/// renders against — the ingestor's File/module identities and the fact cache's
/// cross-checkout re-basing both consume it. A linked worktree resolves to its
/// OWN root, so two checkouts at the same commit mint the same identities.
///
/// The base is canonicalized so a `/var` → `/private/var` symlink cannot make
/// two spellings of one checkout diverge.
pub fn repo_rel(root: &Path) -> PathBuf {
    match git2::Repository::discover(root) {
        Ok(repo) => repo
            .workdir()
            .map(canonical)
            .unwrap_or_else(|| canonical(root)),
        Err(_) => canonical(root),
    }
}
