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
/// checkout that is not ignored would dirty the main checkout forever; the
/// ingestor also uses it to keep gitignored build trees out of the graph).
///
/// `path` may be handed in any spelling a scan produces: an absolute scanner
/// path (possibly through a `/var` → `/private/var` symlink), or a
/// repo-relative identity. libgit2's ignore lookup wants a workdir-relative
/// path, so an absolute input is made relative to the canonicalized workdir
/// first (a path outside the checkout is not ignored); a relative input is
/// taken as already workdir-relative.
///
/// A **tracked** path is never reported ignored: Git's ignore rules apply to
/// untracked content only, and the scan's walk emits every tracked file, so
/// treating a tracked file that happens to match an ignore glob as ignored
/// would silently drop it from the graph (the walk's claim and the ingestor's
/// blacklist must agree — `requirements.constraint.structural-file-graphing-scope`:
/// every Git-tracked file is graphed). The checkout-identity use — an untracked
/// worktree-location probe — is unaffected.
pub fn path_is_ignored(main_root: &Path, path: &Path) -> bool {
    let Ok(repo) = discover_repo(main_root) else {
        return false;
    };
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    let rel = if path.is_absolute() {
        match canonical_allow_missing(path).strip_prefix(canonical_allow_missing(workdir)) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => return false,
        }
    } else {
        path.to_path_buf()
    };
    if rel.as_os_str().is_empty() {
        return false;
    }
    // Index-blind rule lookup: decline a tracked path before consulting the
    // rules. A tracked path that is staged as deleted is absent from the index
    // and falls through to the rules, which is correct — it is being removed.
    if repo
        .index()
        .ok()
        .and_then(|index| index.get_path(&rel, 0).map(|_| ()))
        .is_some()
    {
        return false;
    }
    repo.status_should_ignore(&rel).unwrap_or(false)
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
pub struct RecordedScan {
    pub sha: String,
    pub clean: bool,
    /// The content-identity key; `None` on a pre-hardening record (freshness
    /// cannot be verified then).
    pub content_key: Option<String>,
}

/// The recorded git state of the scan that built the live DB, read from the
/// `scan_meta` control record on line 1 of `graph.jsonl`. `None` when there is
/// no graph.jsonl, its first line is not a `scan_meta` record with both git
/// fields (a pre-hardening export, a non-git scan, or a corrupted line).
/// `content_key` carries the phase-01 content-identity key when present.
pub fn recorded_scan(apg_root: &Path) -> Option<RecordedScan> {
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
mod tests;
