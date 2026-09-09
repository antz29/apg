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

use std::io::BufRead;
use std::path::{Path, PathBuf};

use lbug::{Connection, Database, SystemConfig};

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
        };
    };
    match head_sha(&repo) {
        Some(sha) => GitState {
            sha: Some(sha),
            clean: repo_is_clean(&repo),
        },
        None => GitState {
            sha: None,
            clean: false,
        },
    }
}

/// The recorded git state of the scan that built the live DB, read from the
/// `scan_meta` control record on line 1 of `graph.jsonl`. `None` when there is
/// no graph.jsonl, its first line is not a `scan_meta` record with both git
/// fields (a pre-hardening export, a non-git scan, or a corrupted line).
fn recorded_scan(apg_root: &Path) -> Option<(String, bool)> {
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
            ..
        }) => Some((sha, clean)),
        _ => None,
    }
}

/// The refuse-on-stale predicate:
///
/// - No DB (no `apg/.trans/db.lbug`) → **false** (N/A — nothing to be stale).
/// - Not a git repo → **false** (N/A — no recorded state to compare).
/// - DB exists in a git repo → **stale iff** `(current_sha, current_clean) !=
///   (recorded_sha, recorded_clean)` — both fields, deliberately not sha-only:
///   a dirty tree at the same sha has content the scan did not see. When no
///   recorded scan_meta exists, freshness cannot be verified → **stale**.
pub fn is_stale(apg_root: &Path) -> bool {
    if !db_path(apg_root).exists() {
        return false;
    }
    let current = git_state(apg_root);
    let Some(cur_sha) = current.sha.as_deref() else {
        return false;
    };
    match recorded_scan(apg_root) {
        None => true,
        Some((rec_sha, rec_clean)) => rec_sha != cur_sha || rec_clean != current.clean,
    }
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
        Some((sha, clean)) => state_str(Some(&sha), clean),
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
        Some((rec_sha, rec_clean)) => {
            let rec = state_str(Some(&rec_sha), rec_clean);
            let verdict = if rec_sha == cur_sha && rec_clean == current.clean {
                "FRESH"
            } else {
                "STALE"
            };
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

/// Commits the single file `path` on the current branch of the checkout
/// containing `apg_root` (git2 only — the git CLI is never shelled out to).
///
/// Returns `Ok(Some(sha))` with the new HEAD sha when a commit was created,
/// or `Ok(None)` when the file's content already matches HEAD (nothing to
/// commit — e.g. an idempotent re-write). Errors when the file sits outside
/// the checkout or git refuses the commit.
pub fn auto_commit(apg_root: &Path, path: &Path) -> anyhow::Result<Option<String>> {
    let repo = discover_repo(apg_root)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("repository has no working directory"))?;
    // Compare canonical paths: git2's workdir is canonicalized, the mutation
    // path may be lexical (e.g. through a /var → /private/var symlink).
    let path = canonical(path);
    let workdir = canonical(workdir);
    let rel = path.strip_prefix(&workdir).map_err(|_| {
        anyhow::anyhow!(
            "cannot auto-commit {}: it is outside the checkout {}",
            path.display(),
            workdir.display()
        )
    })?;
    let head = repo.head().map_err(|e| {
        anyhow::anyhow!(
            "cannot auto-commit {}: no HEAD to commit on ({e})",
            path.display()
        )
    })?;
    let head_commit = head
        .peel_to_commit()
        .map_err(|e| anyhow::anyhow!("cannot auto-commit: {e}"))?;

    // Stage the file and compare trees: an unchanged tree means nothing to
    // commit (an idempotent mutation re-wrote identical content).
    let mut index = repo.index()?;
    index.add_path(rel)?;
    index.write()?;
    let tree_id = index.write_tree()?;
    if tree_id == head_commit.tree_id() {
        return Ok(None);
    }
    let tree = repo.find_tree(tree_id)?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("apg", "apg@localhost"))?;
    let msg = format!("apg: graph mutation ({})", rel.display());
    let oid = repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &[&head_commit])?;
    Ok(Some(oid.to_string()))
}

/// Re-anchors the staleness gate's recorded scan_meta after an auto-commit:
/// rewrites the `scan_meta` control record on line 1 of `graph.jsonl` (the
/// record `is_stale` compares against) and, when a DB exists, updates the DB's
/// `Scan` node to match. The code graph itself is untouched — an auto-commit
/// carries exactly the mutated JSONL, so the scan's code content is still
/// exactly what the DB holds (R8: DB and tree in sync by construction).
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
        scanned_at,
    })?;
    std::fs::write(&path, format!("{new}{rest}"))?;

    // Mirror the new state into the DB's Scan node when a DB exists (the Scan
    // table stores git_clean as the STRING "true"/"false" — load.rs).
    let dbp = db_path(apg_root);
    if !dbp.exists() {
        return Ok(());
    }
    let Some(sha) = state.sha.as_deref() else {
        return Ok(());
    };
    let db = Database::new(&dbp, SystemConfig::default())?;
    let conn = Connection::new(&db)?;
    let clean = if state.clean { "true" } else { "false" };
    conn.query(&format!(
        "MATCH (n:Scan {{fqn: 'scan/HEAD'}}) SET n.git_sha = '{}', n.git_clean = '{}'",
        sha.replace('\'', "\\'"),
        clean
    ))?;
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

    // ------------------------------------------------------------------
    // Staleness (git2-based reads; pure-git2 fixtures — no git CLI)
    // ------------------------------------------------------------------

    #[test]
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
        assert!(refusal_message(&apg).is_none());
        assert_eq!(
            staleness_line(&apg, &git_state(&apg)),
            "Git state: N/A (not a git repo)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_db_is_never_stale() {
        let repo = fixture_repo("nodb");
        let apg = repo.apg_root();
        // Even a recorded mismatch is irrelevant when there is no DB to guard.
        testutil::write_scan_meta(&apg, Some("stale-sha"), false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        assert_eq!(
            staleness_line(&apg, &git_state(&apg)),
            format!("Git state: no scan yet (current {}@true)", repo.head_sha())
        );
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // Identity (R7): worktree vs main, default branch, the layout invariant
    // ------------------------------------------------------------------

    #[test]
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
    fn membership_passes_in_project_worktree_on_project_branch() {
        let repo = fixture_repo("member-ok");
        repo.start_project("foo");
        let apg = repo.project_apg_root("foo");
        require_membership(&apg, "foo").unwrap();
        testutil::remove(&repo);
    }

    #[test]
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
    // Auto-commit (R8)
    // ------------------------------------------------------------------

    #[test]
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

    #[test]
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
        // The recorded state now matches the new state → fresh.
        assert_eq!(recorded_scan(&apg), Some((new_sha.to_string(), false)));
        testutil::remove(&repo);
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

    // ------------------------------------------------------------------
    // R6 VI: the git CLI is never shelled out to anywhere in the apg binary
    // (git2, default-features = false; push/tag remain human acts).
    // ------------------------------------------------------------------

    #[test]
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
