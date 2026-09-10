//! `apg project` — the projects model commands (apg-projects R1/R2/R5):
//!
//! - `apg project start <name>` — one command creates the project context:
//!   a git worktree + branch off the repo's DEFAULT branch (the main
//!   checkout's symbolic HEAD, not literal main), at the fixed location
//!   `<main>/apg/.worktrees/<name>`, plus an auto-scan of the new worktree
//!   (worktree + branch + branch DB). Always a branch — no escape hatch.
//!   Idempotent only when `<name>` already IS the current project context
//!   (re-run inside the project's worktree → no-op, prints the path);
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
//!   plain unguarded scan. Binary-operated via git2 from the main checkout;
//!   the git CLI is never shelled out to (R6); push/tag remain human acts.

use std::path::{Path, PathBuf};

use crate::artifacts::parse_args;
use crate::git;
use crate::plan_cmd;
use crate::specs;
use crate::version_gate;

/// `apg project <start|merge> …`.
pub fn cmd_project(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg project <start|merge> …");
    };
    match sub {
        "start" => project_start(&args[1..]),
        "merge" => project_merge(&args[1..]),
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
    let path = project_start_at(&apg_root, name, None)?;
    println!("Project `{name}` started at {}", path.display());
    Ok(())
}

/// The default auto-scan: a real `apg scan` of the worktree (production). The
/// optional `scan` override lets tests drive a hermetic scan.
fn real_scan(dir: &Path) -> anyhow::Result<()> {
    crate::cmd_scan(&[dir.display().to_string()])
}

/// The scan/rebuild injection seam (`project start`'s auto-scan and
/// `project merge`'s main rebuild).
type ScanFn = dyn Fn(&Path) -> anyhow::Result<()>;

/// Core of `project start` (split from the CLI wrapper so tests drive it
/// against a fixture root). `scan: None` runs the real frontend scan;
/// a test scan closure replaces it.
fn project_start_at(apg_root: &Path, name: &str, scan: Option<&ScanFn>) -> anyhow::Result<PathBuf> {
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

    // Seed the worktree's own `apg/.trans/` before the auto-scan: the walk-up
    // layout discovery must resolve to THIS worktree's layout root (a
    // gitignored `.trans` marker is what disambiguates a layout root from an
    // unrelated `apg/` dir — without it, a scan of the worktree would walk
    // past it up into the main checkout's layout).
    let wt_apg = wt_dir.join(specs::LAYOUT);
    std::fs::create_dir_all(wt_apg.join(specs::TRANS))?;

    // Auto-scan: worktree + branch + branch DB (R1 — one command yields all
    // three).
    let scan = scan.unwrap_or(&real_scan);
    scan(&wt_dir)?;
    Ok(wt_dir)
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
/// unguarded `apg scan` of the main checkout; tests override it.
fn project_merge_at(
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

    // Main rebuild: a plain unguarded scan on main.
    let rebuild = rebuild.unwrap_or(&real_scan);
    rebuild(&identity.main_root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts;
    use crate::schema::Record;
    use crate::testutil::{self, Repo};

    /// The module/fqn namespace the payload fixtures use.
    const MOD: &str = "fixture.mod";
    const FILE: &str = "/abs/store.go";

    fn start_scan(dir: &Path) -> anyhow::Result<()> {
        testutil::scan_checkout(dir)
    }

    /// Rewrites the fixture's committed `apg/config.json` (the whole file)
    /// and commits it, so the layout declares `version` (`None` → the
    /// unversioned, pre-versioning shape).
    fn set_layout_version(repo: &Repo, version: Option<&str>) {
        let json = match version {
            Some(v) => format!(
                "{{\n  \"default\": \"src\",\n  \"types\": [],\n  \"version\": \"{v}\"\n}}\n"
            ),
            None => "{ \"default\": \"src\", \"types\": [] }\n".to_string(),
        };
        repo.write("apg/config.json", &json);
        repo.commit_all("set layout version");
    }

    /// The binary's version with a patch bump — same major.minor, so the R10
    /// gate must proceed (patch differences never block).
    fn patch_shifted_version() -> String {
        let v: Vec<u64> = env!("CARGO_PKG_VERSION")
            .split('.')
            .map(|p| p.parse().unwrap())
            .collect();
        format!("{}.{}.{}", v[0], v[1], v[2] + 1)
    }

    /// A version whose major.minor is guaranteed older than the binary's
    /// (e.g. 0.9.x for a 0.10.4 binary).
    fn older_minor_version() -> String {
        let v: Vec<u64> = env!("CARGO_PKG_VERSION")
            .split('.')
            .map(|p| p.parse().unwrap())
            .collect();
        if v[1] > 0 {
            format!("{}.{}.0", v[0], v[1] - 1)
        } else {
            format!("{}.99.0", v[0].saturating_sub(1))
        }
    }

    /// A version whose major.minor is guaranteed newer than the binary's
    /// (e.g. 0.11.x / 1.x for a 0.10.4 binary).
    fn newer_minor_version() -> String {
        let v: Vec<u64> = env!("CARGO_PKG_VERSION")
            .split('.')
            .map(|p| p.parse().unwrap())
            .collect();
        format!("{}.{}.0", v[0], v[1] + 1)
    }

    /// Commits a single file on the worktree's branch (git2 — the same
    /// mechanics auto_commit uses).
    fn wt_commit(wt: &Path, rel: &str, msg: &str) -> String {
        let repo = git2::Repository::open(wt).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(rel)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
            .unwrap()
            .to_string()
    }

    // ------------------------------------------------------------------
    // task-4 AC (int): one command yields worktree + branch + branch DB off
    // the DEFAULT branch; in-context re-run no-ops and prints the path;
    // identity correct in worktree vs main.
    // ------------------------------------------------------------------

    #[test]
    fn start_creates_worktree_branch_and_branch_db() {
        let repo = Repo::new("start-ac");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        let main_sha = repo.commit_all("seed code");

        let wt = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();
        assert_eq!(wt, repo.project_worktree_dir("foo").canonicalize().unwrap());
        assert!(wt.is_dir());

        // The branch exists, off the default branch (main), at main's tip.
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        assert!(!branch.is_head());
        let branch_commit = branch.get().peel_to_commit().unwrap();
        assert_eq!(branch_commit.id().to_string(), main_sha);

        // The worktree's HEAD is the project branch.
        let wt_repo = git2::Repository::open(&wt).unwrap();
        assert_eq!(wt_repo.head().unwrap().shorthand(), Some("foo"));

        // The branch DB exists after start (AC-1).
        assert!(wt.join("apg").join(specs::TRANS).join("db.lbug").exists());

        // Identity correct in worktree vs main (R7).
        let id_wt = git::repo_identity(&wt.join(specs::LAYOUT)).unwrap();
        assert!(id_wt.is_worktree);
        assert_eq!(id_wt.branch.as_deref(), Some("foo"));
        assert_eq!(id_wt.default_branch.as_deref(), Some("main"));
        let id_main = git::repo_identity(&repo.apg_root()).unwrap();
        assert!(!id_main.is_worktree);
        assert_eq!(id_main.branch.as_deref(), Some("main"));

        // In-context re-run: no-op, prints the path, still Ok.
        let again = project_start_at(&wt.join(specs::LAYOUT), "foo", Some(&start_scan)).unwrap();
        assert_eq!(again, wt);

        // From the main checkout the same name is a hard collision.
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        testutil::remove(&repo);
    }

    #[test]
    fn start_branches_off_the_repo_default_not_literal_main() {
        // The default branch is the main checkout's symbolic HEAD (fallback:
        // main). The project branch must sit exactly on it.
        let repo = Repo::new("start-default");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");
        // Rename the default to `trunk` (like a repo whose default is trunk).
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let mut branch = main_repo
            .find_branch("main", git2::BranchType::Local)
            .unwrap();
        branch.rename("trunk", true).unwrap();
        main_repo.set_head("refs/heads/trunk").unwrap();

        let _wt = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        let branch = main_repo
            .find_branch("foo", git2::BranchType::Local)
            .unwrap();
        assert_eq!(
            branch.get().peel_to_commit().unwrap().id(),
            head.id(),
            "project branch must sit on the default branch tip"
        );
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // task-6 AC (unit): every refusal exits 1 naming the actual state and
    // one actionable fix command; run-from-worktree hard-fails.
    // ------------------------------------------------------------------

    #[test]
    fn start_refuses_non_git_with_apg_init_suggestion() {
        let dir = std::env::temp_dir().join(format!("apg-start-nongit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        let err = project_start_at(&dir.join("apg"), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("git repository"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_refuses_unborn_head() {
        let root = std::env::temp_dir().join(format!("apg-start-unborn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("refs/heads/main");
        git2::Repository::init_opts(&root, &mut opts).unwrap();
        std::fs::create_dir_all(root.join("apg").join(specs::TRANS)).unwrap();
        let err = project_start_at(&root.join("apg"), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unborn HEAD"), "{msg}");
        assert!(msg.contains("commit an initial state"), "{msg}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn start_refuses_inside_a_worktree() {
        let repo = Repo::new("start-wt-escape");
        repo.start_project("foo");
        let wt_apg = repo.project_apg_root("foo");
        let err = project_start_at(&wt_apg, "bar", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("one project at a time"), "{msg}");
        assert!(msg.contains("main checkout"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    fn start_refuses_dirty_main() {
        let repo = Repo::new("start-dirty");
        repo.write("junk.txt", "untracked junk");
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("dirty"), "{msg}");
        assert!(msg.contains("git status"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    fn start_refuses_invalid_names_never_sanitized() {
        let repo = Repo::new("start-names");
        for bad in ["", "a/b", "bad name", "bad~name", "..", "HEAD", ".lock"] {
            let err = project_start_at(&repo.apg_root(), bad, Some(&start_scan)).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("refused") && (msg.contains("not a valid") || msg.contains("empty")),
                "name `{bad}` must refuse with a validity message: {msg}"
            );
        }
        testutil::remove(&repo);
    }

    #[test]
    fn start_refuses_branch_without_worktree_collision() {
        let repo = Repo::new("start-collision-branch");
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        main_repo.branch("foo", &head, false).unwrap();
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("no worktree hosts it"), "{msg}");
        assert!(msg.contains("git branch -D foo"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    fn start_refuses_branch_checked_out_in_main_checkout() {
        // Bootstrap-style: the branch is the main checkout's HEAD.
        let repo = Repo::new("start-collision-head");
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let head = main_repo.head().unwrap().peel_to_commit().unwrap();
        main_repo.branch("foo", &head, false).unwrap();
        main_repo.set_head("refs/heads/foo").unwrap();
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("checked out in the main checkout"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    fn start_refuses_branch_checked_out_in_another_worktree() {
        let repo = Repo::new("start-collision-wt");
        repo.start_project("foo");
        // From the main checkout, starting `foo` again: checked out in the
        // project's worktree.
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("checked out in worktree"), "{msg}");
        assert!(
            msg.contains(
                repo.project_worktree_dir("foo")
                    .display()
                    .to_string()
                    .as_str()
            ),
            "{msg}"
        );
        testutil::remove(&repo);
    }

    #[test]
    fn start_refuses_dir_that_is_not_a_worktree() {
        let repo = Repo::new("start-collision-dir");
        // A plain directory at the worktree location (no branch, no worktree).
        std::fs::create_dir_all(repo.project_worktree_dir("foo")).unwrap();
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a project worktree"), "{msg}");
        testutil::remove(&repo);
    }

    #[test]
    fn start_self_heals_missing_worktrees_gitignore_entry() {
        // A repo whose .gitignore dropped the worktrees entry (or was
        // cloned before it existed): start must scaffold it + commit the
        // scaffold (the main checkout stays clean), then proceed.
        let repo = Repo::new("start-selfheal");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.write(".gitignore", "apg/.trans/\n");
        repo.commit_all("drop worktrees ignore");
        let wt = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();
        assert!(wt.is_dir());
        // The entry is back in the main checkout's .gitignore — committed,
        // so the main checkout is clean again (a later start would refuse a
        // dirty main).
        let ignore = std::fs::read_to_string(repo.root.join(".gitignore")).unwrap();
        assert!(ignore.contains("apg/.worktrees/"), "{ignore}");
        assert!(repo.is_clean(), "self-heal must commit the scaffold");
        // The worktree (checked out from the scaffolded HEAD) carries it too.
        let wt_ignore = std::fs::read_to_string(wt.join(".gitignore")).unwrap();
        assert!(wt_ignore.contains("apg/.worktrees/"), "{wt_ignore}");
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // R10 version gate on start (task-3/task-5): blocks — never warns —
    // on missing version and on major/minor mismatch in either direction,
    // with upgrade guidance; a patch diff proceeds.
    // ------------------------------------------------------------------

    #[test]
    fn start_blocks_unversioned_layout_with_init_guidance() {
        let repo = Repo::new("start-gate-unversioned");
        set_layout_version(&repo, None);
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        for needle in [
            "no layout version",
            "apg init",
            "re-run `apg project start foo`",
            "apg-upgrade.md",
        ] {
            assert!(msg.contains(needle), "{msg}");
        }
        // Nothing was created.
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    #[test]
    fn start_blocks_older_layout_with_upgrade_guidance() {
        let repo = Repo::new("start-gate-older");
        set_layout_version(&repo, Some(&older_minor_version()));
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("predates"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        assert!(msg.contains("apg-upgrade.md"), "{msg}");
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    #[test]
    fn start_blocks_newer_layout_with_upgrade_guidance() {
        let repo = Repo::new("start-gate-newer");
        set_layout_version(&repo, Some(&newer_minor_version()));
        let err = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("NEWER apg"), "{msg}");
        assert!(msg.contains("upgrade apg"), "{msg}");
        assert!(msg.contains("apg init"), "{msg}");
        assert!(msg.contains("apg-upgrade.md"), "{msg}");
        assert!(!repo.project_worktree_dir("foo").exists());
        testutil::remove(&repo);
    }

    #[test]
    fn start_proceeds_on_patch_diff_layout() {
        let repo = Repo::new("start-gate-patch");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        set_layout_version(&repo, Some(&patch_shifted_version()));
        let wt = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();
        assert!(wt.is_dir(), "same major.minor must proceed");
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // task-12 (e2e): start on main -> mutate -> verify -> merge -> main
    // rebuild; verify rejects unrealized planned nodes, dangling targets,
    // and unresolved feedback.
    // ------------------------------------------------------------------

    /// Authors the durable spec as a node file under `apg/layers/` (the
    /// new-model store) rather than the retired `apg/specs/` JSONL: one
    /// requirement node, written through the node-file mutation funnel (guard →
    /// validate → atomic write → commit → re-merge).
    fn write_spec_node(wt_apg: &Path) {
        let node = crate::layers::NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: "timer".to_string(),
            body: "A workitem can be started".to_string(),
            properties: std::collections::BTreeMap::from([("id".to_string(), "R1".to_string())]),
            out: Vec::new(),
            in_edges: Vec::new(),
        };
        crate::layers::write_project(wt_apg, &[node], &[]).unwrap();
    }

    /// A plan whose single task plans `fixture.mod.Widget` (not yet real
    /// code) and carries one open feedback on the task.
    fn plan_records(with_feedback: bool) -> Vec<Record> {
        let mut r: Vec<Record> = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "Foo plan".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "T".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::PlannedNode {
                fqn: format!("{MOD}.Widget"),
                kind: "struct".into(),
                name: "Widget".into(),
                parent: MOD.into(),
            },
        ];
        if with_feedback {
            r.push(Record::Feedback {
                fqn: "foo/feedback-1".into(),
                body: "unresolved".into(),
                status: "open".into(),
                disposition: String::new(),
            });
            r.push(Record::Reviews {
                from: "foo/feedback-1".into(),
                to: "foo/plan.phase-01.task-1".into(),
            });
        }
        r
    }

    #[test]
    fn merge_round_trip_start_mutate_verify_merge_rebuild() {
        let repo = Repo::new("merge-e2e");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");

        // start -> worktree + branch + branch DB (payload has only Store).
        let wt = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();
        let wt_apg = wt.join(specs::LAYOUT);

        // mutate: author the spec (a durable node file -> auto-commits on the
        // branch) and the plan (transient -> never commits), both through the
        // funnel.
        write_spec_node(&wt_apg);
        let plan_path = wt_apg.join(specs::TRANS).join("plans").join("foo.jsonl");
        artifacts::write_jsonl_and_reingest(&wt_apg, &plan_path, "foo", &plan_records(true))
            .unwrap();
        // The spec mutation auto-committed; the plan mutation committed
        // nothing (R8 — .trans never commits).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let foo_tip = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            foo_tip
                .tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/requirement/timer.json"))
                .is_ok(),
            "spec node file must be committed on the project branch"
        );
        assert!(
            foo_tip
                .tree()
                .unwrap()
                .get_path(Path::new("apg/.trans/plans/foo.jsonl"))
                .is_err(),
            "plan JSONL must never be committed (.trans is transient)"
        );

        // verify rejects: unrealized planned node (dangling — no code at its
        // FQN yet) AND unresolved feedback, in one refusal listing both.
        let err = plan_cmd::plan_verify_at(&wt_apg, "foo").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Widget"), "unrealized planned node: {msg}");
        assert!(msg.contains("unresolved review feedback"), "{msg}");

        // resolve the feedback (writer action + reviewer resolve, via the
        // funnel on the transient plan).
        let mut recs = plan_records(false);
        recs.push(Record::Feedback {
            fqn: "foo/feedback-1".into(),
            body: "unresolved".into(),
            status: "resolved".into(),
            disposition: "fixed".into(),
        });
        recs.push(Record::Reviews {
            from: "foo/feedback-1".into(),
            to: "foo/plan.phase-01.task-1".into(),
        });
        artifacts::write_jsonl_and_reingest(&wt_apg, &plan_path, "foo", &recs).unwrap();

        // realize the planned node: the implementer's code lands on the
        // branch (payload gains Widget), is committed, and the branch DB is
        // rebuilt with a scan.
        let payload = testutil::code_payload(MOD, FILE, &["Store", "Widget"]);
        std::fs::write(wt.join("code/seed.scan.jsonl"), payload).unwrap();
        wt_commit(&wt, "code/seed.scan.jsonl", "implement Widget");
        start_scan(&wt).unwrap();
        let foo_tip = git2::Repository::open(&wt)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();

        // verify passes: planned node realized, all feedback resolved.
        plan_cmd::plan_verify_at(&wt_apg, "foo").unwrap();

        // merge from the main checkout: verify gate -> ff merge -> main
        // rebuild (the rebuild is a plain unguarded scan on main).
        project_merge_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();

        // The default branch now holds the project's tip.
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        let main_tip = main_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            main_tip.id(),
            foo_tip,
            "main must fast-forward to the project tip"
        );
        // The main checkout carries the merged content (the spec node file).
        assert!(
            repo.root
                .join("apg/layers/requirements/requirement/timer.json")
                .exists()
        );
        assert!(repo.is_clean(), "merged main must be clean");

        // Main rebuild: the main DB has the code (Store + Widget) and the
        // merged spec; the transient plan did not cross the merge.
        let main_apg = repo.apg_root();
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        assert!(db.has_node("requirements.requirement.timer"));
        assert!(db.has_node(format!("{MOD}.Store").as_str()));
        assert!(db.has_node(format!("{MOD}.Widget").as_str()));
        assert!(!db.has_node("foo/plan"), "transient plans never reach main");
        drop(db);
        // The main DB is fresh (scan_meta re-anchored by the rebuild scan).
        assert!(!git::is_stale(&main_apg));
        testutil::remove(&repo);
    }
}
