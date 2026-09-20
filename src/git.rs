//! Git-state capture, repo identity, and the project membership guard — the
//! git2 plumbing of the projects model (apg-projects R1–R8).
//!
//! Everything here is libgit2 (`git2`, `default-features = false` — no
//! https/ssh, no OpenSSL). **The git CLI is never shelled out to** (R6);
//! push/tag remain human acts.
//!
//! Three concerns live in this module:
//!
//! 1. **Scan staleness** (agent-loop hardening, pre-projects model): every
//!    scan records the git state it ran under — HEAD sha plus tree cleanliness
//!    — as a `scan_meta` control record on line 1 of
//!    `apg/.trans/graph.jsonl` and as the DB's `Scan` node. A later
//!    spec/plan/review mutation that would re-ingest into a stale DB is
//!    refused *before* any JSONL write. All git reads here are git2-based.
//!
//! 2. **Repo identity + membership** (R3/R4/R7): mutations may only happen
//!    inside a project context — the project's worktree at
//!    `<main>/apg/.worktrees/<project>`, on the project's branch. Identity
//!    (branch, checkout path, main path, worktree existence) comes from git2;
//!    the walk-up `apg/` discovery stays the layout resolver (correct in
//!    worktrees by construction). The invariant — the git checkout root
//!    contains the walked-up `apg/` — is verified cheaply in
//!    [`repo_identity`], erroring on divergence (R7).
//!
//! 3. **Auto-commit + scan_meta re-anchor** (R8): after a graph mutation the
//!    funnel commits the touched file on the project branch via git2 — one
//!    commit per mutation, single-file diffs — and re-anchors the recorded
//!    scan_meta to the new state so consecutive mutations do not each demand
//!    a rescan. Plan mutations never commit: `apg/.trans` is gitignored and
//!    transient by design.
//!
//! 4. **Lifecycle self-cleanup** (merge self-cleanup / delete subcommand):
//!    [`remove_worktree`] + [`delete_branch`] — the shared git2 primitives
//!    `apg project merge`'s success path and `apg project delete` use to
//!    remove a project's worktree and delete its branch. The
//!    never-touch-default-branch law lives in [`delete_branch`].

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::schema::Record;
use crate::specs;

/// The git state of a directory at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    /// The full HEAD commit sha of the enclosing repo, or `None` when `dir` is
    /// not inside a git repo (or the repo has no commits yet).
    pub sha: Option<String>,
    /// True when `git status --porcelain` is empty. Meaningful only when
    /// `sha` is `Some`.
    pub clean: bool,
    /// The content-identity key of the tree (win A): a digest over the HEAD
    /// sha plus the working-tree + index + untracked file **content**, never
    /// mtime. `None` when there is no sha (not a git repo / unborn HEAD).
    pub content_key: Option<String>,
}

fn db_path(apg_root: &Path) -> PathBuf {
    apg_root.join(specs::TRANS).join("db.lbug")
}

fn graph_jsonl_path(apg_root: &Path) -> PathBuf {
    apg_root.join(specs::TRANS).join("graph.jsonl")
}

// ---------------------------------------------------------------------------
// git2 plumbing helpers
// ---------------------------------------------------------------------------

/// Discovers the repo enclosing `dir` (walking up, like the git CLI).
fn discover_repo(dir: &Path) -> anyhow::Result<git2::Repository> {
    git2::Repository::discover(dir).map_err(|e| {
        anyhow::anyhow!(
            "{} is not inside a git repository ({e}) — a project context requires git; run `git init`, commit an initial state, then `apg init`",
            dir.display()
        )
    })
}

/// True when `git status --porcelain` would be empty: no tracked changes and
/// no untracked non-ignored files (ignored content — `apg/.trans/`,
/// `apg/.worktrees/` — never counts).
fn repo_is_clean(repo: &git2::Repository) -> bool {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    repo.statuses(Some(&mut opts))
        .map(|ss| ss.is_empty())
        .unwrap_or(false)
}

/// Canonicalizes a path for comparison, falling back to the lexical path.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Canonicalize a path whose leaf may no longer exist (a staged deletion):
/// canonicalize the deepest existing ancestor and re-append the missing
/// components. Falls back to the lexical path when nothing resolves. Needed
/// because `std::fs::canonicalize` follows symlinks only through existing
/// components — a removed file would otherwise compare un-canonicalized
/// against the canonical workdir (the `/var` → `/private/var` macOS alias).
fn canonical_allow_missing(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(base) = std::fs::canonicalize(&cur) {
            let mut out = base;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out;
        }
        let Some(name) = cur.file_name().map(|n| n.to_os_string()) else {
            return path.to_path_buf();
        };
        tail.push(name);
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent.to_path_buf(),
            _ => return path.to_path_buf(),
        }
    }
}

/// The head commit sha of the repo, or `None` when HEAD is unborn or missing.
fn head_sha(repo: &git2::Repository) -> Option<String> {
    let head = repo.head().ok()?;
    head.peel_to_commit().ok().map(|c| c.id().to_string())
}

/// True when the checkout containing `dir` is clean (`git status --porcelain`
/// empty, ignoring gitignored content). Used by `project start`/`project
/// merge` to refuse operating on a dirty main checkout (R2).
pub fn checkout_clean(dir: &Path) -> bool {
    match discover_repo(dir) {
        Ok(repo) => repo_is_clean(&repo),
        Err(_) => false,
    }
}

/// True when the ignore rules of the checkout containing `main_root` cover
/// `path` (the project worktree location must be gitignored — a nested
/// checkout that is not ignored would dirty the main checkout forever).
pub fn path_is_ignored(main_root: &Path, path: &Path) -> bool {
    match discover_repo(main_root) {
        Ok(repo) => repo.status_should_ignore(path).unwrap_or(false),
        Err(_) => false,
    }
}

/// Captures the current git state of the repo containing `dir`: HEAD sha plus
/// tree cleanliness. The repo is resolved by walking up, so passing a
/// subdirectory (a scanned module dir, or the `apg/` layout root) observes the
/// whole repo's state.
pub fn git_state(dir: &Path) -> GitState {
    let Ok(repo) = git2::Repository::discover(dir) else {
        return GitState {
            sha: None,
            clean: false,
            content_key: None,
        };
    };
    match head_sha(&repo) {
        Some(sha) => GitState {
            sha: Some(sha),
            clean: repo_is_clean(&repo),
            content_key: Some(content_key(&repo)),
        },
        None => GitState {
            sha: None,
            clean: false,
            content_key: None,
        },
    }
}

/// FNV-1a 64-bit over `bytes`, folded into `h` (a small, dependency-free,
/// deterministic digest — equal bytes always give equal digests across
/// processes; it is only ever compared within the same binary's rule).
fn fnv1a(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// The content-identity key of a repo's tree (win A): a digest over the HEAD
/// sha, the staged (index) content, and the working-tree/untracked file
/// **content**. mtime is never consulted — touching a file without changing a
/// byte yields the same key; any byte edit changes it. Ignored content
/// (`apg/.trans/`, `apg/.worktrees/`) never counts, so a scan's own writes do
/// not invalidate the key.
fn content_key(repo: &git2::Repository) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    fnv1a(&mut h, head_sha(repo).unwrap_or_default().as_bytes());

    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return format!("{h:016x}");
    };
    // Deterministic order: the status collection's iteration order is not
    // specified, so sort before folding (the key must be stable).
    let mut changed: Vec<(String, u32)> = statuses
        .iter()
        .map(|e| (e.path().unwrap_or_default().to_string(), e.status().bits()))
        .collect();
    changed.sort();

    let workdir = repo.workdir().map(Path::to_path_buf);
    let index = repo.index().ok();
    for (path, bits) in changed {
        fnv1a(&mut h, path.as_bytes());
        fnv1a(&mut h, &bits.to_le_bytes());
        // Staged content identity: the index blob oid (content-addressed).
        if let Some(ie) = index.as_ref().and_then(|i| i.get_path(Path::new(&path), 0)) {
            fnv1a(&mut h, ie.id.to_string().as_bytes());
        }
        // Working-tree content identity: hash the file bytes when it exists.
        if let Some(wd) = &workdir {
            let full = wd.join(&path);
            if full.is_file()
                && let Ok(bytes) = std::fs::read(&full)
            {
                fnv1a(&mut h, &bytes);
            }
        }
    }
    format!("{h:016x}")
}

/// The recorded state of the scan that built the live DB (`recorded_scan`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedScan {
    sha: String,
    clean: bool,
    /// The content-identity key; `None` on a pre-hardening record (freshness
    /// cannot be verified then).
    content_key: Option<String>,
}

/// The recorded git state of the scan that built the live DB, read from the
/// `scan_meta` control record on line 1 of `graph.jsonl`. `None` when there is
/// no graph.jsonl, its first line is not a `scan_meta` record with both git
/// fields (a pre-hardening export, a non-git scan, or a corrupted line).
/// `content_key` carries the phase-01 content-identity key when present.
fn recorded_scan(apg_root: &Path) -> Option<RecordedScan> {
    let f = std::fs::File::open(graph_jsonl_path(apg_root)).ok()?;
    let line = std::io::BufReader::new(f).lines().next()?.ok()?;
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    match serde_json::from_str::<Record>(line) {
        Ok(Record::ScanMeta {
            git_sha: Some(sha),
            git_clean: Some(clean),
            content_key,
            ..
        }) => Some(RecordedScan {
            sha,
            clean,
            content_key,
        }),
        _ => None,
    }
}

/// The content-identity freshness predicate (win A): the live DB is reusable
/// as-is iff it exists AND the current tree's content identity matches the
/// recorded scan exactly — recorded HEAD sha, cleanliness and content-identity
/// key all equal the current values. mtime is never consulted: a touch without
/// a byte change stays fresh, a byte edit is stale.
///
/// The DB-existence precondition comes FIRST. The fast-path's action is "reuse
/// the existing DB", so with no `db.lbug` at all (or a `graph.jsonl` without
/// its DB) there is nothing to reuse → NOT fresh. A missing/pre-hardening
/// recorded key means freshness cannot be verified → NOT fresh.
///
/// This is deliberately not `!is_stale`: `is_stale` is N/A (false) when there
/// is no DB or the dir is not a git repo, so `is_stale != !is_fresh` there.
pub fn is_fresh(apg_root: &Path) -> bool {
    if !db_path(apg_root).exists() {
        return false;
    }
    let Ok(repo) = git2::Repository::discover(apg_root) else {
        return false;
    };
    let Some(cur_sha) = head_sha(&repo) else {
        return false;
    };
    let Some(rec) = recorded_scan(apg_root) else {
        return false;
    };
    let Some(rec_key) = rec.content_key.as_deref() else {
        return false; // pre-hardening: freshness cannot be verified
    };
    rec.sha == cur_sha && rec.clean == repo_is_clean(&repo) && rec_key == content_key(&repo)
}

/// The refuse-on-stale predicate:
///
/// - No DB (no `apg/.trans/db.lbug`) → **false** (N/A — nothing to be stale).
/// - Not a git repo (or an unborn HEAD) → **false** (N/A — no recorded state
///   to compare). These two N/A short-circuits are why this is not simply
///   `!is_fresh`.
/// - DB exists in a git repo → **stale iff** `!is_fresh` — one shared
///   content-identity rule with the scan fast-path. A missing/pre-hardening
///   recorded key is stale (freshness cannot be verified).
pub fn is_stale(apg_root: &Path) -> bool {
    if !db_path(apg_root).exists() {
        return false;
    }
    let Ok(repo) = git2::Repository::discover(apg_root) else {
        return false;
    };
    if head_sha(&repo).is_none() {
        return false;
    }
    !is_fresh(apg_root)
}

/// `<sha>@<clean>` for a git state, or `-@-` when there is no sha to display.
fn state_str(sha: Option<&str>, clean: bool) -> String {
    match sha {
        Some(s) => format!("{s}@{clean}"),
        None => "-@-".to_string(),
    }
}

/// The refusal message for a stale DB, or `None` when the DB is fresh (or N/A).
/// Only ever called by the mutation gate once `is_stale` is true.
pub fn refusal_message(apg_root: &Path) -> Option<String> {
    if !is_stale(apg_root) {
        return None;
    }
    let current = git_state(apg_root);
    let recorded = match recorded_scan(apg_root) {
        Some(rec) => state_str(Some(&rec.sha), rec.clean),
        None => state_str(None, false),
    };
    let cur = state_str(current.sha.as_deref(), current.clean);
    Some(format!(
        "graph is stale (recorded {recorded}, current {cur}) — run `apg scan` before mutating"
    ))
}

/// The one-line staleness summary `apg scan` prints (requirement 5): recorded
/// `<sha>@<clean>` vs current `<sha>@<clean>` → `STALE`/`FRESH`, evaluated
/// against the *pre-scan* DB before the new scan overwrites it. N/A when the
/// scan is not in a git repo, or when there is no prior scan to be stale.
///
/// The verdict is the SAME content-identity rule [`is_fresh`] uses, so the
/// printed line and the fast-path decision can never disagree (a recorded-dirty
/// tree whose content digest changed at the same sha prints STALE, never FRESH
/// followed by a full pipeline run).
pub fn staleness_line(apg_root: &Path, current: &GitState) -> String {
    let Some(cur_sha) = current.sha.as_deref() else {
        return "Git state: N/A (not a git repo)".to_string();
    };
    if !db_path(apg_root).exists() {
        return format!(
            "Git state: no scan yet (current {})",
            state_str(Some(cur_sha), current.clean)
        );
    }
    let cur = state_str(Some(cur_sha), current.clean);
    match recorded_scan(apg_root) {
        None => format!(
            "Git state: recorded {} vs current {cur} → STALE",
            state_str(None, false)
        ),
        Some(rec) => {
            let rec = state_str(Some(&rec.sha), rec.clean);
            let verdict = if is_fresh(apg_root) { "FRESH" } else { "STALE" };
            format!("Git state: recorded {rec} vs current {cur} → {verdict}")
        }
    }
}

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
// Auto-commit (R8): one commit per mutation, single-file diffs; the stale
// gate's recorded scan_meta is re-anchored after each auto-commit so DB and
// tree stay in sync by construction.
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

/// Commits all `writes` (created/modified paths) and `deletes` (removed
/// paths) in one commit on the current branch of the checkout containing
/// `apg_root` with a caller-supplied message (git2 only — the git CLI is never
/// shelled out to). The multi-file generalization of [`commit_file`]: writes
/// are staged with `index.add_path`, deletes with `index.remove_path`
/// (`add_path` stats the file and cannot stage a deletion — a removed path
/// fails with a libgit2 `NotFound`), the trees are compared, and a single
/// commit is created when anything changed.
///
/// The index write (`repo.index()` / `index.write()`, i.e. `.git/index.lock`)
/// is **already inside the caller's extended whole-durable-sequence flock**:
/// `cmd_node`/`cmd_edge` hold [`crate::artifacts::acquire_spec_lock`] across
/// validate → write → commit → projection, so no internal acquire is needed and
/// a parallel burst never contends on `.git/index.lock`. Deletion-capable
/// staging (`add_path` for writes, `remove_path` for deletes) is preserved.
///
/// Returns `Ok(Some(sha))` with the new HEAD sha when a commit was created, or
/// `Ok(None)` when the staged tree already matches HEAD (nothing to commit —
/// e.g. an idempotent re-write). Errors when any path sits outside the
/// checkout or git refuses the commit.
pub fn commit_files(
    apg_root: &Path,
    writes: &[&Path],
    deletes: &[&Path],
    msg: &str,
) -> anyhow::Result<Option<String>> {
    let repo = discover_repo(apg_root)?;
    let base = repo_rel(apg_root);
    // The mutation paths may be lexical (e.g. through a `/var` →
    // `/private/var` symlink), while the base is canonical: canonicalize the
    // deepest existing ancestor before stripping.
    let rel = |p: &Path| -> anyhow::Result<PathBuf> {
        let path = canonical_allow_missing(p);
        path.strip_prefix(&base)
            .map(|r| r.to_path_buf())
            .map_err(|_| {
                anyhow::anyhow!(
                    "cannot commit {}: it is outside the checkout {}",
                    path.display(),
                    base.display()
                )
            })
    };
    let write_rels: Vec<PathBuf> = writes
        .iter()
        .map(|p| rel(p))
        .collect::<anyhow::Result<_>>()?;
    let delete_rels: Vec<PathBuf> = deletes
        .iter()
        .map(|p| rel(p))
        .collect::<anyhow::Result<_>>()?;
    let head = repo
        .head()
        .map_err(|e| anyhow::anyhow!("cannot commit: no HEAD to commit on ({e})"))?;
    let head_commit = head
        .peel_to_commit()
        .map_err(|e| anyhow::anyhow!("cannot commit: {e}"))?;

    // Stage every write and delete and compare trees: an unchanged tree means
    // nothing to commit (an idempotent mutation re-wrote identical content).
    let mut index = repo.index()?;
    for rel in &write_rels {
        index.add_path(rel)?;
    }
    for rel in &delete_rels {
        // Drop the removed path from the index so `write_tree` records the
        // deletion. A path that is not in the index has nothing to stage
        // (e.g. an untracked file already gone from disk) — tolerate it.
        let _ = index.remove_path(rel);
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    if tree_id == head_commit.tree_id() {
        return Ok(None);
    }
    let tree = repo.find_tree(tree_id)?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("apg", "apg@localhost"))?;
    let oid = repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head_commit])?;
    Ok(Some(oid.to_string()))
}

/// `commit_files` for a single file — the single-file commit path.
pub fn commit_file(apg_root: &Path, path: &Path, msg: &str) -> anyhow::Result<Option<String>> {
    commit_files(apg_root, &[path], &[], msg)
}

/// The standard graph-mutation commit message for a set of touched paths:
/// [`auto_commit`]'s single-path format, joined for a multi-file mutation —
/// `apg: graph mutation (rel1, rel2, ...)`. Paths render checkout-relative
/// when possible (the same form [`auto_commit`] uses).
pub fn graph_mutation_message(apg_root: &Path, paths: &[&Path]) -> String {
    let base = repo_rel(apg_root);
    let rels: Vec<String> = paths
        .iter()
        .map(|p| {
            canonical_allow_missing(p)
                .strip_prefix(&base)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string())
        })
        .collect();
    format!("apg: graph mutation ({})", rels.join(", "))
}

/// `commit_file` with the standard graph-mutation message — the funnel's
/// one-commit-per-mutation commit (R8).
pub fn auto_commit(apg_root: &Path, path: &Path) -> anyhow::Result<Option<String>> {
    commit_file(apg_root, path, &graph_mutation_message(apg_root, &[path]))
}

/// True when `apg_root` is inside a git repository (discoverable via git2).
/// [`layers::write_through`] uses it to skip the commit outside a repo — no
/// repo → no commit; the mutation still lands on disk.
// (Unused until write_project, phase-3 task-16, wires write_through.)
#[allow(dead_code)]
pub fn in_repo(apg_root: &Path) -> bool {
    discover_repo(apg_root).is_ok()
}

/// Re-anchors the staleness gate's recorded scan_meta after an auto-commit:
/// rewrites the `scan_meta` control record on line 1 of `graph.jsonl` (the
/// record `is_stale` compares against). The code graph itself is untouched —
/// an auto-commit carries exactly the mutated node/JSONL content, so the scan's
/// code content is still exactly what the projection holds (R8: DB and tree in
/// sync by construction).
///
/// `graph.jsonl` is the SOLE code-identity and freshness source (phase-02
/// decoupling): this NEVER opens `db.lbug` read-write, so no exclusive DB open
/// precedes the mutation's durable commit. The DB's derived `Scan` node is
/// reconciled by the write-through projection / the next scan, never here.
pub fn reanchor_scan_meta(apg_root: &Path, state: &GitState) -> anyhow::Result<()> {
    let path = graph_jsonl_path(apg_root);
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)?;
    let first_nl = text.find('\n').unwrap_or(text.len());
    let first = text[..first_nl].trim_end();
    let rest = &text[first_nl..];
    if first.is_empty() {
        return Ok(());
    }
    let Ok(Record::ScanMeta { scanned_at, .. }) = serde_json::from_str::<Record>(first) else {
        return Ok(()); // Not a scan_meta lead — nothing to re-anchor.
    };
    let new = serde_json::to_string(&Record::ScanMeta {
        git_sha: state.sha.clone(),
        git_clean: state.sha.as_ref().map(|_| state.clean),
        // The content-identity key of the post-mutation state, so the
        // re-anchored record keeps the fast-path's rule intact (the mutation's
        // auto-commit moved HEAD; the new state's key matches the new tree).
        content_key: state.content_key.clone(),
        scanned_at,
    })?;
    std::fs::write(&path, format!("{new}{rest}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// Current UTC time as an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM:SSZ`), the
/// `scanned_at` value of a scan_meta record. Pure `std` (no chrono dep); the
/// civil-date conversion is the classic days-to-civil algorithm.
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs() as i64;
    iso8601(secs)
}

/// Formats epoch seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
fn iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian
/// calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, Repo, git2_repo};

    fn fixture_repo(tag: &str) -> Repo {
        Repo::new(tag)
    }

    /// Commits a single file on the worktree's current branch.
    fn wt_commit(wt: &Path, path: &Path, msg: &str) -> String {
        let repo = git2::Repository::open(wt).unwrap();
        let rel = path.strip_prefix(wt).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(rel).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
            .unwrap()
            .to_string()
    }

    fn open_worktree_sha(wt: &Path) -> String {
        git2::Repository::open(wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string()
    }

    /// e2e tier -- real I/O: every test here creates scratch git repos/DBs
    /// under the temp dir, writes files, runs git2/libgit2 operations or spawns
    /// the `apg` binary. Each is `#[ignore]`d, so a plain `cargo test` never
    /// runs one; the only entry point is the named guard `cargo test-e2e`
    /// (= `cargo test tests::e2e:: -- --ignored`).
    mod e2e {
        use super::*;

        // ------------------------------------------------------------------
        // Staleness (git2-based reads; pure-git2 fixtures — no git CLI)
        // ------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn same_sha_and_clean_is_fresh() {
            let repo = fixture_repo("fresh");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
            assert!(
                !is_stale(&apg),
                "clean tree at the recorded sha must be fresh"
            );
            assert!(refusal_message(&apg).is_none());
            let cur = git_state(&apg);
            assert_eq!(
                staleness_line(&apg, &cur),
                format!("Git state: recorded {sha}@true vs current {sha}@true → FRESH")
            );
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn differing_sha_is_stale() {
            let repo = fixture_repo("sha");
            let sha0 = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha0), true, "2026-09-07T00:00:00Z");
            let sha1 = repo.commit_all("second");
            assert_ne!(sha0, sha1);
            assert!(is_stale(&apg), "a new commit must make the DB stale");
            let msg = refusal_message(&apg).unwrap();
            assert!(
                msg.contains(&format!("recorded {sha0}@true, current {sha1}@true")),
                "refusal message: {msg}"
            );
            assert!(msg.contains("run `apg scan` before mutating"), "{msg}");
            assert!(staleness_line(&apg, &git_state(&apg)).contains("→ STALE"));
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn dirty_tree_at_same_sha_is_stale() {
            let repo = fixture_repo("dirty");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
            assert!(!is_stale(&apg));
            // Same sha, but the tree moved: the scan did not see this content.
            repo.write(".gitignore", "apg/.trans/\n# dirty after scan\n");
            assert!(!git_state(&apg).clean);
            assert!(
                is_stale(&apg),
                "a dirty tree at the same sha must be stale (recorded clean=true)"
            );
            assert!(
                refusal_message(&apg)
                    .unwrap()
                    .contains(&format!("recorded {sha}@true")),
                "dirty-state message should name the recorded clean state"
            );
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn recorded_dirty_matching_dirty_tree_is_fresh() {
            // A scan over an already-dirty tree records clean=false; the same dirty
            // tree at the same sha is (by design) indistinguishable from it, so the
            // DB stays fresh until the tree changes again.
            let repo = fixture_repo("dirtyrec");
            repo.write("dirty.txt", "x");
            assert!(!git_state(&repo.root).clean);
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
            assert!(!is_stale(&apg));
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn no_recorded_scan_meta_is_stale() {
            let repo = fixture_repo("norec");
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            // DB exists in a git repo, but no graph.jsonl / no scan_meta record:
            // freshness cannot be verified.
            assert!(is_stale(&apg));
            let msg = refusal_message(&apg).unwrap();
            assert!(msg.contains("recorded -@-"), "{msg}");
            assert!(msg.contains("run `apg scan` before mutating"), "{msg}");
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn non_git_repo_is_never_stale() {
            // A DB whose scan was not in a git repo, plus no git repo now → N/A.
            let dir = std::env::temp_dir().join(format!("apg-nongit-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let apg = dir.join("apg").join(specs::TRANS);
            std::fs::create_dir_all(&apg).unwrap();
            let apg = dir.join("apg");
            std::fs::write(apg.join(specs::TRANS).join("db.lbug"), "").unwrap();
            testutil::write_scan_meta(&apg, None, false, "2026-09-07T00:00:00Z");
            assert!(!is_stale(&apg));
            // The gate is N/A, but the fast-path is NOT fresh: is_stale != !is_fresh.
            assert!(!is_fresh(&apg), "not a git repo → nothing to reuse");
            assert!(refusal_message(&apg).is_none());
            assert_eq!(
                staleness_line(&apg, &git_state(&apg)),
                "Git state: N/A (not a git repo)"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn no_db_is_never_stale() {
            let repo = fixture_repo("nodb");
            let apg = repo.apg_root();
            // Even a recorded mismatch is irrelevant when there is no DB to guard.
            testutil::write_scan_meta(&apg, Some("stale-sha"), false, "2026-09-07T00:00:00Z");
            assert!(!is_stale(&apg));
            // No DB → the fast-path is NOT fresh (nothing to reuse): is_stale !=
            // !is_fresh for this N/A gate case.
            assert!(!is_fresh(&apg));
            assert_eq!(
                staleness_line(&apg, &git_state(&apg)),
                format!("Git state: no scan yet (current {}@true)", repo.head_sha())
            );
            testutil::remove(&repo);
        }

        // ------------------------------------------------------------------
        // Phase-01 task-9: content-identity freshness (win A)
        // ------------------------------------------------------------------

        /// The DB-existence precondition: with no `db.lbug`, or a `graph.jsonl`
        /// without its DB, the fast-path is NOT fresh (there is nothing to reuse) —
        /// even when the recorded scan_meta matches the tree.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn is_fresh_requires_the_live_db() {
            let repo = fixture_repo("freshdb");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
            assert!(!is_fresh(&apg), "no db.lbug → not fresh");
            assert!(
                !is_stale(&apg),
                "the gate is N/A without a DB (is_stale != !is_fresh)"
            );
            // The DB appearing makes the matching recorded state fresh.
            testutil::touch_db(&apg);
            assert!(is_fresh(&apg), "matching recorded state + DB → fresh");
            assert!(!is_stale(&apg));
            // A DB but no recorded scan_meta (graph.jsonl removed) → not fresh.
            std::fs::remove_file(apg.join(specs::TRANS).join("graph.jsonl")).unwrap();
            assert!(!is_fresh(&apg), "no recorded scan → not fresh");
            assert!(is_stale(&apg), "DB in a repo with no recorded scan → stale");
            testutil::remove(&repo);
        }

        /// A pre-hardening scan_meta (no content-identity key) cannot be verified:
        /// it is NOT fresh and the gate treats it as stale.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn pre_hardening_scan_meta_is_not_fresh() {
            let repo = fixture_repo("prehard");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta_keyed(&apg, Some(&sha), true, "2026-09-07T00:00:00Z", None);
            assert!(
                !is_fresh(&apg),
                "missing key → freshness cannot be verified"
            );
            assert!(is_stale(&apg));
            testutil::remove(&repo);
        }

        /// mtime is never consulted: touching a tracked file forward without
        /// changing a byte keeps the DB fresh.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn touch_without_byte_change_stays_fresh() {
            let repo = fixture_repo("touch");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
            assert!(is_fresh(&apg));
            // Bump the mtime far into the future — bytes unchanged.
            let p = repo.root.join("apg/config.json");
            let before = std::fs::read(&p).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
                .unwrap();
            drop(f);
            assert_eq!(
                std::fs::read(&p).unwrap(),
                before,
                "bytes must be unchanged"
            );
            assert!(
                is_fresh(&apg),
                "a touch without a byte change must stay fresh"
            );
            assert!(!is_stale(&apg));
            testutil::remove(&repo);
        }

        /// A byte edit at the same sha changes the content identity: the DB is not
        /// fresh and the gate is stale.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn content_identity_byte_edit_invalidates() {
            let repo = fixture_repo("byteedit");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
            assert!(is_fresh(&apg));
            repo.write(
                "apg/config.json",
                "{\n  \"default\": \"edited\",\n  \"types\": []\n}\n",
            );
            assert!(!is_fresh(&apg), "a byte edit must invalidate freshness");
            assert!(is_stale(&apg));
            assert!(staleness_line(&apg, &git_state(&apg)).contains("→ STALE"));
            testutil::remove(&repo);
        }

        /// A recorded DIRTY tree matched by content stays fresh; a content change
        /// of that same dirty tree at the same sha invalidates it (recorded
        /// `clean=false` alone is not enough — the digest must match too).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn recorded_dirty_content_change_invalidates() {
            let repo = fixture_repo("dirtykey");
            repo.write("scratch.txt", "one");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            testutil::write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
            assert!(
                is_fresh(&apg),
                "same dirty content at the same sha is fresh"
            );
            // Same sha, still dirty — but the dirty content changed.
            repo.write("scratch.txt", "two");
            assert!(!is_fresh(&apg), "changed dirty content must invalidate");
            assert!(is_stale(&apg));
            testutil::remove(&repo);
        }

        /// `staleness_line` prints the SAME verdict `is_fresh` returns — in both
        /// directions. A recorded-dirty tree whose content digest changed at the
        /// same sha prints STALE from BOTH (never FRESH followed by a full run).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn staleness_line_agrees_with_is_fresh() {
            let repo = fixture_repo("verdict");
            let sha = repo.head_sha();
            let apg = repo.apg_root();
            testutil::touch_db(&apg);
            // FRESH: a matching clean tree.
            testutil::write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
            assert_eq!(
                is_fresh(&apg),
                staleness_line(&apg, &git_state(&apg)).contains("→ FRESH")
            );
            // Recorded DIRTY tree, then its dirty content changes at the same sha.
            repo.write("scratch.txt", "one");
            testutil::write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
            assert!(is_fresh(&apg));
            repo.write("scratch.txt", "two");
            let line = staleness_line(&apg, &git_state(&apg));
            assert!(!is_fresh(&apg), "content digest changed at the same sha");
            assert!(line.contains("→ STALE"), "printed line: {line}");
            assert!(!line.contains("→ FRESH"), "printed line: {line}");
            assert_eq!(is_fresh(&apg), line.contains("→ FRESH"));
            testutil::remove(&repo);
        }

        // ------------------------------------------------------------------
        // Identity (R7): worktree vs main, default branch, the layout invariant
        // ------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn identity_in_main_checkout() {
            let repo = fixture_repo("ident-main");
            let apg = repo.apg_root();
            let id = repo_identity(&apg).unwrap();
            assert_eq!(id.main_root, id.checkout_root);
            assert!(!id.is_worktree);
            assert_eq!(id.branch.as_deref(), Some("main"));
            assert_eq!(id.default_branch.as_deref(), Some("main"));
            assert_eq!(id.head_sha.as_deref(), Some(repo.head_sha().as_str()));
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn identity_in_project_worktree() {
            let repo = fixture_repo("ident-wt");
            let wt = repo.start_project("foo");
            let apg = wt.join(specs::LAYOUT);
            let id = repo_identity(&apg).unwrap();
            assert_eq!(
                id.checkout_root,
                repo.project_worktree_dir("foo").canonicalize().unwrap()
            );
            assert_eq!(id.main_root, repo.root.canonicalize().unwrap());
            assert!(id.is_worktree);
            assert_eq!(id.branch.as_deref(), Some("foo"));
            // The DEFAULT branch is the repo's default (origin/HEAD when present,
            // else the main checkout's symbolic HEAD) — never the worktree's
            // branch. The fixture has no origin, so the fallback applies: main.
            assert_eq!(id.default_branch.as_deref(), Some("main"));
            assert!(id.head_sha.is_some());
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn identity_divergence_errors_when_layout_outside_checkout() {
            // An apg/ layout that git does not contain: the invariant errors.
            let dir = std::env::temp_dir().join(format!("apg-divergence-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let repo = fixture_repo("divergence");
            let outside = dir.join("apg");
            std::fs::create_dir_all(outside.join(specs::TRANS)).unwrap();
            let err = repo_identity(&outside).unwrap_err();
            assert!(
                format!("{err:#}").contains("divergence"),
                "divergence message: {err:#}"
            );
            let _ = std::fs::remove_dir_all(&dir);
            testutil::remove(&repo);
        }

        // ------------------------------------------------------------------
        // Membership (R3/R4)
        // ------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn membership_passes_in_project_worktree_on_project_branch() {
            let repo = fixture_repo("member-ok");
            repo.start_project("foo");
            let apg = repo.project_apg_root("foo");
            require_membership(&apg, "foo").unwrap();
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn membership_refuses_on_default_branch_naming_branch_half() {
            let repo = fixture_repo("member-main");
            let apg = repo.apg_root();
            let err = require_membership(&apg, "foo").unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("half 1 (branch)"), "{msg}");
            assert!(msg.contains("branch `main`"), "{msg}");
            assert!(msg.contains("default branch"), "{msg}");
            assert!(msg.contains("apg project start foo"), "{msg}");
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn membership_refuses_wrong_project_from_another_worktree() {
            let repo = fixture_repo("member-wrong");
            repo.start_project("foo");
            repo.start_project("bar");
            let apg_bar = repo.project_apg_root("bar");
            let err = require_membership(&apg_bar, "foo").unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("half 1 (branch)"), "{msg}");
            assert!(msg.contains("branch `bar`"), "{msg}");
            assert!(msg.contains("branch `foo`"), "{msg}");
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn bootstrap_branch_in_main_checkout_is_a_project_context() {
            // R20 bootstrap carve-out: a project branch that IS the main
            // checkout's HEAD (branch-without-worktree — git cannot host the
            // branch anywhere else) is a valid mutation context. The default
            // branch is not — and the default is resolved from origin/HEAD, so it
            // stays `main` even while the main checkout holds the project branch.
            let repo = fixture_repo("member-bootstrap");
            let main_repo = git2_repo(&repo);
            let head = main_repo.head().unwrap().peel_to_commit().unwrap();
            // origin/HEAD → main (like a real remote-backed repo).
            main_repo
                .reference("refs/remotes/origin/main", head.id(), true, "origin main")
                .unwrap();
            main_repo
                .reference_symbolic(
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                    true,
                    "origin head",
                )
                .unwrap();
            // The main checkout switches to the project branch (bootstrap state).
            main_repo.branch("apg-feature", &head, false).unwrap();
            main_repo.set_head("refs/heads/apg-feature").unwrap();
            assert_eq!(
                repo_identity(&repo.apg_root())
                    .unwrap()
                    .default_branch
                    .as_deref(),
                Some("main")
            );
            // The bootstrap project context passes membership...
            require_membership(&repo.apg_root(), "apg-feature").unwrap();
            // ...and the default branch itself never does, even under its own name.
            main_repo.set_head("refs/heads/main").unwrap();
            let err = require_membership(&repo.apg_root(), "main").unwrap_err();
            assert!(
                format!("{err:#}").contains("default branch is never a mutation place"),
                "{err:#}"
            );
            // Back on main, the project branch refuses via the branch half.
            let err = require_membership(&repo.apg_root(), "apg-feature").unwrap_err();
            assert!(format!("{err:#}").contains("half 1 (branch)"), "{err:#}");
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn universal_context_refuses_outside_any_project() {
            let repo = fixture_repo("member-universal");
            // Main checkout on main: no project context.
            let err = require_project_context(&repo.apg_root()).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("refused"), "{msg}");
            assert!(msg.contains("default branch"), "{msg}");
            // Inside a project worktree: universal mutations are fine.
            repo.start_project("foo");
            require_project_context(&repo.project_apg_root("foo")).unwrap();
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn non_git_dir_refuses_membership_with_git_guidance() {
            let dir = std::env::temp_dir().join(format!("apg-nongit-m-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
            let err = require_membership(&dir.join("apg"), "foo").unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("not inside a git repository"), "{msg}");
            assert!(msg.contains("apg init"), "{msg}");
            let _ = std::fs::remove_dir_all(&dir);
        }

        // ------------------------------------------------------------------
        // Lifecycle cleanup helpers (merge self-cleanup / delete): the shared
        // git2 worktree-removal + branch-deletion primitives.
        // ------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn remove_worktree_unregisters_and_removes_working_dir() {
            let repo = fixture_repo("rm-wt");
            let wt = repo.start_project("foo");
            assert!(wt.is_dir());
            let main_repo = git2_repo(&repo);
            assert!(main_repo.find_worktree("foo").is_ok());

            remove_worktree(&repo.root, "foo").unwrap();

            assert!(!wt.exists(), "the worktree working dir must be removed");
            assert!(
                main_repo.find_worktree("foo").is_err(),
                "the worktree registration must be pruned"
            );
            assert!(
                main_repo
                    .find_branch("foo", git2::BranchType::Local)
                    .is_ok(),
                "remove_worktree must leave the branch alone"
            );
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn remove_worktree_tolerates_missing_or_unregistered_worktrees() {
            let repo = fixture_repo("rm-wt-gone");
            repo.start_project("foo");
            let main_repo = git2_repo(&repo);
            // Simulate an already-pruned worktree: registration gone, leftover dir
            // (or no dir at all) — both must be safe no-ops. (`valid` is required
            // to prune a still-valid worktree; `working_tree` off keeps the dir.)
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).working_tree(false);
            main_repo
                .find_worktree("foo")
                .unwrap()
                .prune(Some(&mut opts))
                .unwrap();
            assert!(main_repo.find_worktree("foo").is_err());
            remove_worktree(&repo.root, "foo").unwrap();

            // A name that never existed is also a no-op.
            remove_worktree(&repo.root, "never-existed").unwrap();
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn delete_branch_removes_local_project_branch() {
            let repo = fixture_repo("del-br");
            repo.start_project("foo");
            let main_repo = git2_repo(&repo);
            assert!(
                main_repo
                    .find_branch("foo", git2::BranchType::Local)
                    .is_ok()
            );

            // The lifecycle ordering: the worktree is removed FIRST — a branch
            // that is still a linked worktree's HEAD cannot be deleted (libgit2
            // mirrors `git branch -d`) — then the branch.
            remove_worktree(&repo.root, "foo").unwrap();
            delete_branch(&repo.root, "foo").unwrap();

            assert!(
                main_repo
                    .find_branch("foo", git2::BranchType::Local)
                    .is_err(),
                "the local project branch must be deleted"
            );
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn delete_branch_never_touches_the_default_branch() {
            // The never-touch-default-branch law: deleting <default> is a hard
            // refusal, and the main checkout (default branch checked out) must
            // survive untouched.
            let repo = fixture_repo("del-default");
            let err = delete_branch(&repo.root, "main").unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("refused"), "{msg}");
            assert!(msg.contains("default branch"), "{msg}");
            assert!(
                git2_repo(&repo)
                    .find_branch("main", git2::BranchType::Local)
                    .is_ok(),
                "the default branch must survive"
            );
            assert!(repo.root.is_dir(), "the main checkout must survive");
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn delete_branch_tolerates_an_already_deleted_branch() {
            let repo = fixture_repo("del-br-gone");
            repo.start_project("foo");
            let main_repo = git2_repo(&repo);
            // Lifecycle ordering again: remove the worktree first so the branch is
            // deletable, then delete it out from under the helper.
            remove_worktree(&repo.root, "foo").unwrap();
            let mut branch = main_repo
                .find_branch("foo", git2::BranchType::Local)
                .unwrap();
            branch.delete().unwrap();
            assert!(
                main_repo
                    .find_branch("foo", git2::BranchType::Local)
                    .is_err()
            );
            delete_branch(&repo.root, "foo").unwrap();
            testutil::remove(&repo);
        }

        // ------------------------------------------------------------------
        // Auto-commit (R8)
        // ------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn auto_commit_makes_one_commit_with_single_file_diff() {
            let repo = fixture_repo("autocommit");
            repo.start_project("foo");
            let wt = repo.project_worktree_dir("foo");
            // A mutation-style file change inside the worktree (spec JSONL).
            let rel = "apg/specs/foo.jsonl";
            let path = wt.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{\"type\":\"spec\"}\n").unwrap();
            let sha0 = open_worktree_sha(&wt);
            let Some(new_sha) = auto_commit(&wt.join("apg"), &path).unwrap() else {
                panic!("expected a commit");
            };
            assert_ne!(sha0, new_sha);
            // Exactly one commit ahead of the main checkout's branch tip...
            let main_repo = git2_repo(&repo);
            let branch = main_repo
                .find_branch("foo", git2::BranchType::Local)
                .unwrap();
            let branch_commit = branch.get().peel_to_commit().unwrap();
            assert_eq!(branch_commit.id().to_string(), new_sha);
            // ...whose tree diff is exactly the one file (single-file diff).
            let parent = branch_commit.parent(0).unwrap();
            let diff = main_repo
                .diff_tree_to_tree(
                    Some(&parent.tree().unwrap()),
                    Some(&branch_commit.tree().unwrap()),
                    None,
                )
                .unwrap();
            assert_eq!(
                diff.deltas().len(),
                1,
                "auto-commit must be a single-file diff"
            );
            let delta = diff.deltas().next().unwrap();
            assert_eq!(delta.new_file().path().unwrap(), Path::new(rel));
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn auto_commit_is_a_noop_when_content_matches_head() {
            let repo = fixture_repo("autocommit-noop");
            repo.start_project("foo");
            let wt = repo.project_worktree_dir("foo");
            let path = wt.join("apg/specs/foo.jsonl");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x\n").unwrap();
            // Commit the file on the worktree's branch (git2 — the same mechanics
            // auto_commit uses), so HEAD already carries this content.
            wt_commit(&wt, &path, "seed");
            assert_eq!(auto_commit(&wt.join("apg"), &path).unwrap(), None);
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn commit_file_commits_with_custom_message() {
            let repo = fixture_repo("commitfile");
            repo.start_project("foo");
            let wt = repo.project_worktree_dir("foo");
            // The start-flow self-heal uses commit_file for its .gitignore
            // scaffold commit — same mechanics, caller-supplied message.
            let path = wt.join(".gitignore");
            std::fs::write(&path, "apg/.trans/\napg/.worktrees/\n# extra\n").unwrap();
            let sha0 = open_worktree_sha(&wt);
            let Some(new_sha) =
                commit_file(&wt.join("apg"), &path, "apg: scaffold .gitignore entries").unwrap()
            else {
                panic!("expected a commit");
            };
            assert_ne!(sha0, new_sha);
            let wt_repo = git2::Repository::open(&wt).unwrap();
            let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
            assert_eq!(
                head.message().unwrap(),
                "apg: scaffold .gitignore entries",
                "commit_file must use the caller's message"
            );
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn commit_files_commits_all_paths_in_one_commit() {
            let repo = fixture_repo("commitfiles");
            repo.start_project("foo");
            let wt = repo.project_worktree_dir("foo");
            let a = wt.join("apg/layers/requirements/requirement/a.json");
            let b = wt.join("apg/layers/domain/entity/b.json");
            std::fs::create_dir_all(a.parent().unwrap()).unwrap();
            std::fs::create_dir_all(b.parent().unwrap()).unwrap();
            std::fs::write(&a, "a\n").unwrap();
            std::fs::write(&b, "b\n").unwrap();
            let sha0 = open_worktree_sha(&wt);
            let Some(new_sha) = commit_files(
                &wt.join("apg"),
                &[a.as_path(), b.as_path()],
                &[],
                "apg: graph mutation (apg/layers/...)",
            )
            .unwrap() else {
                panic!("expected a commit");
            };
            assert_ne!(sha0, new_sha);
            // Exactly one commit ahead of the branch tip, whose tree diff is
            // exactly the two files — one commit carrying all affected paths.
            let main_repo = git2_repo(&repo);
            let branch = main_repo
                .find_branch("foo", git2::BranchType::Local)
                .unwrap();
            let branch_commit = branch.get().peel_to_commit().unwrap();
            assert_eq!(branch_commit.id().to_string(), new_sha);
            let parent = branch_commit.parent(0).unwrap();
            let diff = main_repo
                .diff_tree_to_tree(
                    Some(&parent.tree().unwrap()),
                    Some(&branch_commit.tree().unwrap()),
                    None,
                )
                .unwrap();
            assert_eq!(
                diff.deltas().len(),
                2,
                "commit_files must commit all paths in one commit"
            );
            testutil::remove(&repo);
        }

        /// `commit_files` stages a removed path as a deletion (`remove_path`), not
        /// an `add_path` — the latter stats the gone file and fails with a libgit2
        /// NotFound, which is why a `node rm` used to roll back inside a real repo.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn commit_files_stages_a_deletion() {
            let repo = fixture_repo("commitfiles-delete");
            repo.start_project("foo");
            let wt = repo.project_worktree_dir("foo");
            let rel = "apg/layers/requirements/requirement/gone.json";
            let path = wt.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "gone\n").unwrap();
            let sha0 = wt_commit(&wt, &path, "seed");
            // Remove it from disk and commit the deletion.
            std::fs::remove_file(&path).unwrap();
            let Some(new_sha) =
                commit_files(&wt.join("apg"), &[], &[path.as_path()], "apg: rm").unwrap()
            else {
                panic!("expected a deletion commit");
            };
            assert_ne!(sha0, new_sha);
            let wt_repo = git2::Repository::open(&wt).unwrap();
            let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
            let parent = head.parent(0).unwrap();
            let diff = wt_repo
                .diff_tree_to_tree(
                    Some(&parent.tree().unwrap()),
                    Some(&head.tree().unwrap()),
                    None,
                )
                .unwrap();
            let deleted: Vec<&str> = diff
                .deltas()
                .filter(|d| d.status() == git2::Delta::Deleted)
                .map(|d| d.old_file().path().unwrap().to_str().unwrap())
                .collect();
            assert_eq!(
                deleted,
                vec![rel],
                "the removed path must be staged as a deletion"
            );
            testutil::remove(&repo);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn reanchor_rewrites_graph_jsonl_line_one() {
            let repo = fixture_repo("reanchor");
            repo.start_project("foo");
            let apg = repo.project_apg_root("foo");
            let sha0 = repo.head_sha();
            testutil::write_scan_meta(&apg, Some(&sha0), true, "2026-09-07T00:00:00Z");
            let new_sha = "abcd1234";
            reanchor_scan_meta(
                &apg,
                &GitState {
                    sha: Some(new_sha.to_string()),
                    clean: false,
                    content_key: Some("reanchored-key".to_string()),
                },
            )
            .unwrap();
            let first = std::fs::read_to_string(apg.join(specs::TRANS).join("graph.jsonl"))
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .to_string();
            assert!(first.contains(new_sha), "line 1: {first}");
            assert!(first.contains("\"git_clean\":false"), "line 1: {first}");
            assert!(
                first.contains("\"scanned_at\":\"2026-09-07T00:00:00Z\""),
                "line 1: {first}"
            );
            // The content-identity key is preserved across the re-anchor.
            assert!(
                first.contains("\"content_key\":\"reanchored-key\""),
                "line 1: {first}"
            );
            // The recorded state now matches the new state → fresh.
            assert_eq!(
                recorded_scan(&apg),
                Some(RecordedScan {
                    sha: new_sha.to_string(),
                    clean: false,
                    content_key: Some("reanchored-key".to_string()),
                })
            );
            testutil::remove(&repo);
        }

        /// Phase-04 task-5 (acceptance): one logical node/edge mutation produces
        /// exactly ONE commit, and that commit stages durable files only — never
        /// `apg/.trans` (the gitignored transient store).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn acceptance_one_logical_mutation_is_exactly_one_durable_commit() {
            let (repo, wt, _wt_apg) = testutil::project_with_db("accept-one-commit");
            let home = repo.root.join("home");
            std::fs::create_dir_all(&home).unwrap();
            let run = |args: &[&str]| {
                let out = testutil::ApgCommand::new(args)
                    .cwd(&wt)
                    .env("HOME", home.to_str().unwrap())
                    .output();
                assert!(
                    out.status.success(),
                    "{args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            };
            // The paths the HEAD commit's tree diff touched.
            let head_diff_paths = || -> Vec<String> {
                let r = git2::Repository::open(&wt).unwrap();
                let head = r.head().unwrap().peel_to_commit().unwrap();
                let parent = head.parent(0).unwrap();
                let diff = r
                    .diff_tree_to_tree(
                        Some(&parent.tree().unwrap()),
                        Some(&head.tree().unwrap()),
                        None,
                    )
                    .unwrap();
                diff.deltas()
                    .map(|d| d.new_file().path().unwrap().to_string_lossy().into_owned())
                    .collect()
            };

            let base = testutil::commit_count(&wt);

            // Logical mutation 1 + 2: each `apg node add` is exactly one commit.
            run(&["node", "add", "requirements", "requirement", "one-a"]);
            assert_eq!(
                testutil::commit_count(&wt),
                base + 1,
                "node add one-a must be exactly one commit"
            );
            run(&["node", "add", "requirements", "requirement", "one-b"]);
            assert_eq!(
                testutil::commit_count(&wt),
                base + 2,
                "node add one-b must be exactly one commit"
            );

            // Logical mutation 3: the edge add rewrites BOTH endpoint files in ONE
            // commit whose diff is the two durable layer files.
            run(&[
                "edge",
                "add",
                "depends-on",
                "requirements.requirement.one-a",
                "requirements.requirement.one-b",
            ]);
            assert_eq!(
                testutil::commit_count(&wt),
                base + 3,
                "the edge add must be exactly one commit"
            );
            let edge_paths = head_diff_paths();
            assert_eq!(
                edge_paths.len(),
                2,
                "the edge commit stages both endpoint files: {edge_paths:?}"
            );
            for p in &edge_paths {
                assert!(p.starts_with("apg/layers/"), "durable only: {p}");
                assert!(!p.starts_with("apg/.trans/"), "never .trans: {p}");
            }

            // Logical mutation 4: `node rm` rewrites the referring file and deletes
            // the node in ONE commit, still durable-only.
            run(&["node", "rm", "requirements", "requirement", "one-b"]);
            assert_eq!(
                testutil::commit_count(&wt),
                base + 4,
                "node rm must be exactly one commit"
            );
            for p in head_diff_paths() {
                assert!(p.starts_with("apg/layers/"), "durable only: {p}");
                assert!(!p.starts_with("apg/.trans/"), "never .trans: {p}");
            }

            testutil::remove(&repo);
        }

        // ------------------------------------------------------------------
        // R6 VI: the git CLI is never shelled out to anywhere in the apg binary
        // (git2, default-features = false; push/tag remain human acts).
        // ------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/fs/git/process); run via cargo test-e2e"]
        fn no_git_cli_shellouts_remain_in_src() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let src = root.join("src");
            let mut checked = 0usize;
            for entry in std::fs::read_dir(&src).unwrap().flatten() {
                let path = entry.path();
                // Only the apg binary's own flat top-level src/*.rs files
                // (read_dir yields direct children, so the vendored frontend
                // projects under src/*lib/ are never included).
                if !path.is_file() || path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let content = std::fs::read_to_string(&path).unwrap();
                assert!(
                    !content.contains("Command::new(\"git\")"),
                    "{} shells out to the git CLI (R6: git2 only; push/tag remain human acts)",
                    path.display()
                );
                checked += 1;
            }
            assert!(
                checked >= 10,
                "expected the flat src/*.rs set, checked {checked}"
            );
        }
    }

    /// unit tier -- pure in-memory: no filesystem, database, git or process.
    mod unit {
        use super::*;

        #[test]
        fn iso8601_formats_known_epochs() {
            assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
            assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
            // The live formatter produces the same shape.
            let now = now_iso8601();
            assert_eq!(now.len(), "YYYY-MM-DDTHH:MM:SSZ".len());
            assert!(now.ends_with('Z'));
            assert!(now.as_bytes()[10] == b'T');
        }
    }
}
