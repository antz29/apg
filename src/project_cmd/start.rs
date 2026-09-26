//! `apg project start` — branch + worktree + branch DB in one command.

use std::path::{Path, PathBuf};

use crate::artifacts::parse_args;
use crate::git;
use crate::specs;
use crate::version_gate;

use super::{require_apg_root, validate_project_name, worktree_hosting};

/// `apg project start <name>` — the one-command project context creation.
pub(crate) fn project_start(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(name) = p.positional.first() else {
        anyhow::bail!("usage: apg project start <name>");
    };
    let apg_root = require_apg_root()?;
    let path = project_start_at(&apg_root, name)?;
    println!("Project `{name}` started at {}", path.display());
    Ok(())
}

/// The default rebuild: a real `apg scan` of the main checkout (production).
/// The optional `ScanFn` override lets tests drive a hermetic rebuild.
pub(crate) fn real_scan(dir: &Path) -> anyhow::Result<()> {
    crate::cmd_scan(&[dir.display().to_string()])
}

/// The rebuild injection seam (`project merge`'s main rebuild).
pub(crate) type ScanFn = dyn Fn(&Path) -> anyhow::Result<()>;

/// Core of `project start` (split from the CLI wrapper so tests drive it
/// against a fixture root). Refuses when the main checkout's scan is stale or
/// missing, else creates the branch + worktree and copies main's `apg/.trans`
/// into the branch — no frontend scan.
pub fn project_start_at(apg_root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    // Identity first: non-git dirs refuse here (R2: suggest apg init).
    let identity = git::repo_identity(apg_root)?;
    let wt_dir = git::project_worktree_dir(&identity.main_root, name);

    // Idempotent no-op: already inside this project's worktree, on its branch.
    if identity.is_worktree {
        if identity.branch.as_deref() == Some(name)
            && identity.checkout_root
                == std::fs::canonicalize(&wt_dir).unwrap_or_else(|_| wt_dir.clone())
        {
            println!("Project `{name}` is already active at {}", wt_dir.display());
            return Ok(wt_dir);
        }
        anyhow::bail!(
            "refused: `apg project start` is a main-checkout operation — you work one project at a time (this checkout is worktree `{}` on branch `{}`). Fix: run `apg project start {name}` from the main checkout.",
            identity.checkout_root.display(),
            identity.branch.as_deref().unwrap_or("<none>")
        );
    }

    // Unborn / detached main checkout.
    if identity.branch.is_none() {
        if identity.head_sha.is_none() {
            anyhow::bail!(
                "refused: the main checkout has no commits yet (unborn HEAD). Fix: commit an initial state first (`git add … && git commit`), then re-run `apg project start {name}`."
            );
        }
        anyhow::bail!(
            "refused: the main checkout is on a detached HEAD — a project branches off the DEFAULT branch (symbolic HEAD). Fix: check out the default branch (`git checkout {default}`), then re-run `apg project start {name}`.",
            default = identity.default_branch.as_deref().unwrap_or("<default>")
        );
    }

    // R10 version gate: `project start` is a layout-touching op (it
    // self-heals + writes under `<main>/apg/`), so the main checkout's
    // layout must be versioned for this binary's major.minor — a missing
    // version (pre-versioning layout) or a mismatch in either direction
    // blocks with upgrade guidance; `apg init` is the upgrade act.
    version_gate::require_layout_version(apg_root, &format!("re-run `apg project start {name}`"))?;

    refuse_start(&identity, name)?;

    // Freshness gate: start seeds the branch DB by copying main's scan, so
    // main must have a CURRENT scan for main's HEAD. `!git::is_fresh` (never
    // `is_stale`) — a missing `db.lbug`/`graph.jsonl` is "not fresh" too, which
    // `is_stale` alone cannot see. This sits AFTER `refuse_start` (dirty/
    // collision/in-worktree refusals keep precedence) and BEFORE the gitignore
    // self-heal, so a refusal never mutates the main checkout.
    if !git::is_fresh(apg_root) {
        anyhow::bail!(
            "refused: the main checkout's scan is stale or missing — `apg project start` copies main's scan into the branch, so main must have a current scan. Fix: run `apg scan` in the main checkout, then re-run `apg project start {name}`."
        );
    }

    // Self-heal (R9): when the worktree location is not gitignored — the
    // repo .gitignore dropped the `apg/.worktrees/` entry, or the repo was
    // cloned before it existed — scaffold the apg layout entries and commit
    // the scaffold on the current branch. The dirty-main refusal above
    // guarantees the tree was clean at entry, and a dropped entry must not
    // leave the main checkout dirty (a later start would refuse it). If the
    // location still is not ignored after the scaffold (e.g. an explicit
    // negation shadowing the entries), refuse naming the actual state. The
    // `.worktrees/` dir itself is created below (libgit2's worktree add does
    // not create intermediate dirs; `apg init` scaffolds it, start heals it).
    let probe = git::project_worktree_dir(&identity.main_root, name);
    if !git::path_is_ignored(&identity.main_root, &probe) {
        if crate::scaffold_gitignore(&identity.main_root)? {
            git::commit_file(
                &identity.main_root,
                &identity.main_root.join(".gitignore"),
                "apg: scaffold .gitignore entries for the apg layout",
            )?;
        }
        if !git::path_is_ignored(&identity.main_root, &probe) {
            anyhow::bail!(
                "refused: {} is still not gitignored after scaffolding the apg layout entries — an explicit .gitignore negation must be shadowing them. Fix: remove the negation (or append `apg/.worktrees/` at the end of the .gitignore), then re-run `apg project start {name}`.",
                probe.display()
            );
        }
    }

    // Create branch + worktree (git2 only; the worktree's own HEAD is set to
    // the new branch and the branch tree is checked out into it). libgit2's
    // worktree add creates the branch itself (like `git worktree add <path>`
    // branches off the last path component) at the main checkout's HEAD —
    // refuse_start already guaranteed the branch does not exist.
    let main_repo = git2::Repository::open(&identity.main_root)?;
    if let Some(parent) = wt_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    main_repo.worktree(name, &wt_dir, None)?;
    let wt_repo = git2::Repository::open(&wt_dir)?;
    wt_repo.set_head(&format!("refs/heads/{name}"))?;
    wt_repo.checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))?;

    // Seed the worktree's own `apg/.trans/` by copying the main checkout's
    // scan verbatim: the graph stores repo-relative paths (`path`/File FQNs)
    // and the worktree's tree equals main's HEAD, so main's DB is valid as-is
    // in the branch — no scan, no frontend spawns. Copying (not scanning) also
    // guarantees the fresh `apg/.trans` marker exists, which is what
    // disambiguates this worktree's layout root from the main checkout's
    // during walk-up discovery.
    let wt_apg = wt_dir.join(specs::LAYOUT);
    copy_trans_artifacts(&apg_root.join(specs::TRANS), &wt_apg.join(specs::TRANS))?;
    Ok(wt_dir)
}

/// Recursively copies the main checkout's `apg/.trans` scan into the new
/// worktree's `apg/.trans` (files and nested directories). Reads only the
/// `.trans` tree — never the source tree — creates `dst`, and is a no-op when
/// `src` and `dst` are the same directory.
fn copy_trans_artifacts(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if src == dst {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_trans_artifacts(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Every hard-refusal case of `project start` (R2) — each error names the
/// actual state and one fix command. Callers resolve identity first; this
/// function is the pure refusal matrix over a main-checkout identity.
fn refuse_start(identity: &git::RepoIdentity, name: &str) -> anyhow::Result<()> {
    // Name validity (R2: never sanitized — an invalid name is refused whole).
    validate_project_name(name)?;

    // Dirty main checkout (ignored paths — .trans, .worktrees — never count).
    if !git::checkout_clean(&identity.main_root) {
        anyhow::bail!(
            "refused: the main checkout is dirty — a project must branch off a clean default. Fix: commit or stash your changes (`git status`), then re-run `apg project start {name}`."
        );
    }

    // Every collision is a case-specific "project already exists" (R2).
    let probe = git::project_worktree_dir(&identity.main_root, name);
    let repo = git2::Repository::open(&identity.main_root)?;
    let branch = repo.find_branch(name, git2::BranchType::Local).ok();
    let registered = repo.find_worktree(name).ok();
    if let Some(branch) = branch {
        if branch.is_head() {
            anyhow::bail!(
                "project `{name}` already exists: branch `{name}` is checked out in the main checkout (the branch-without-worktree state) — there is no second project context to start. Fix: work on the branch where it is (mutations are allowed there), and when it is done merge it and delete the branch (`git branch -d {name}`)."
            );
        }
        // Checked out in a linked worktree? Name it.
        if let Some((wt_name, wt_path)) = worktree_hosting(&repo, name) {
            anyhow::bail!(
                "project `{name}` already exists: branch `{name}` is checked out in worktree `{wt_name}` at {path}. Fix: work inside {path} (and remove the worktree with `git worktree remove` once the project is merged).",
                path = wt_path.display()
            );
        }
        anyhow::bail!(
            "project `{name}` already exists: branch `{name}` exists but no worktree hosts it (a leftover branch). Fix: check it out into the project worktree (`git worktree add {} {name}`) or delete it (`git branch -D {name}`), then re-run `apg project start {name}`.",
            probe.display()
        );
    }
    if registered.is_some() {
        anyhow::bail!(
            "project `{name}` already exists: a registered worktree exists at {} (its branch is gone). Fix: prune it (`git worktree prune`), then re-run `apg project start {name}`.",
            probe.display()
        );
    }
    if probe.exists() {
        anyhow::bail!(
            "refused: {} exists but is not a project worktree. Fix: move it aside or remove it, then re-run `apg project start {name}`.",
            probe.display()
        );
    }
    Ok(())
}
