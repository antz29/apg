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
//!   push/tag remain human acts.
//!
//! - `apg project delete <name>` — the explicit abandon act for projects
//!   that are NOT merged. Every refusal names the actual state + one fix
//!   command (the refuse_start pattern); the branch's commits are discarded —
//!   delete does NOT require the branch to be merged (delete-refuses-unsafe).
//!   On the success path the project's worktree at
//!   `<main>/apg/.worktrees/<name>` is removed and its branch deleted via the
//!   shared phase-1 helpers; the default branch and the main checkout are
//!   never touched (never-touch-default-branch).

use std::path::{Path, PathBuf};

use crate::artifacts::parse_args;
use crate::git;
use crate::plan_cmd;
use crate::specs;
use crate::version_gate;

/// `apg project <start|merge|delete> …`.
pub fn cmd_project(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg project <start|merge|delete> …");
    };
    match sub {
        "start" => project_start(&args[1..]),
        "merge" => project_merge(&args[1..]),
        "delete" => project_delete(&args[1..]),
        other => anyhow::bail!("unknown apg project subcommand: {other}"),
    }
}

/// The layout root of the checkout the process runs in (walk-up discovery).
fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

// ---------------------------------------------------------------------------
// project start
// ---------------------------------------------------------------------------

/// `apg project start <name>` — the one-command project context creation.
fn project_start(args: &[String]) -> anyhow::Result<()> {
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
fn real_scan(dir: &Path) -> anyhow::Result<()> {
    crate::cmd_scan(&[dir.display().to_string()])
}

/// The rebuild injection seam (`project merge`'s main rebuild).
type ScanFn = dyn Fn(&Path) -> anyhow::Result<()>;

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

/// The worktree (name, path) that has `branch` checked out, if any.
fn worktree_hosting(repo: &git2::Repository, branch: &str) -> Option<(String, PathBuf)> {
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
fn validate_project_name(name: &str) -> anyhow::Result<()> {
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

// ---------------------------------------------------------------------------
// project merge
// ---------------------------------------------------------------------------

/// `apg project merge <name>` — verify gate → merge → main rebuild.
fn project_merge(args: &[String]) -> anyhow::Result<()> {
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
    // Lifecycle exclusivity (phase-03): a live session owns the branch DB, so
    // merging (which removes the worktree/branch on success) must not race it.
    // Refuse BEFORE the merge/rebuild replaces any projected DB.
    if crate::session::live_session(&wt_apg) {
        anyhow::bail!(
            "refused: a live `apg session` owns the branch DB at {} — end it first (`apg session end` inside the project worktree) before merging",
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

// ---------------------------------------------------------------------------
// project delete
// ---------------------------------------------------------------------------

/// `apg project delete <name>` — the explicit abandon act for a project that
/// is NOT merged. Refuses (naming the actual state + one fix command) unless
/// deleting the branch is safe; on success removes the project's worktree at
/// `<main>/apg/.worktrees/<name>` and deletes `refs/heads/<name>` via the
/// shared phase-1 helpers. Discarding the branch's commits is the point —
/// delete does NOT require the branch to be merged.
fn project_delete(args: &[String]) -> anyhow::Result<()> {
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
