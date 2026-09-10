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
    use crate::layers::{self, InEdge, NodeFile, OutEdge};
    use crate::schema::Record;
    use crate::testutil::{self, Repo};
    use std::collections::BTreeMap;

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
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
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

    // ------------------------------------------------------------------
    // phase-5 task-1 (e2e): bootstrap dogfood — the apg-projects change-set
    // re-materializes in the layers model through the REAL project flow
    // (SPEC §6: "this spec ... will be re-materialized in the new model
    // later" — this is that re-materialization): start from the main
    // checkout (worktree + branch + auto-scan) -> author the tiers as node
    // files through the write_project funnel (the same funnel `apg node`/
    // `apg edge` use) -> the transient plan ingests alongside -> verify ->
    // merge -> main rebuild. Plus the SPEC §6 guard: the test worktree
    // `apg/.worktrees/test` stays until before shipping and cleanup deletes
    // no branch.
    // ------------------------------------------------------------------

    /// The code FQNs the solution tier's `implemented-by` edges claim — all
    /// resolve in the fixture's scanned graph, and the plan's tasks must touch
    /// every one of them for derived solution coverage (SPEC §5).
    const IMPL_FQNS: [&str; 5] = [
        "fixture.mod.ProjectStart",
        "fixture.mod.MutationGuard",
        "fixture.mod.LayersSerializer",
        "fixture.mod.PlanBridge",
        "fixture.mod.InitVersionGate",
    ];

    /// A bare node file (identity + prose + metadata; edges added by the
    /// pairing helpers below).
    fn nf(
        layer: &str,
        node_type: &str,
        name: &str,
        body: &str,
        props: &[(&str, &str)],
    ) -> NodeFile {
        NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: body.to_string(),
            properties: props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            out: Vec::new(),
            in_edges: Vec::new(),
        }
    }

    fn out_e(kind: &str, target: &str) -> OutEdge {
        OutEdge {
            kind: kind.to_string(),
            target: target.to_string(),
            properties: BTreeMap::new(),
        }
    }

    fn in_e(kind: &str, source: &str) -> InEdge {
        InEdge {
            kind: kind.to_string(),
            source: source.to_string(),
            properties: BTreeMap::new(),
        }
    }

    /// Pushes a node and records its FQN (`<layer>.<type>.<name>`) in `idx`.
    fn tier_push(nodes: &mut Vec<NodeFile>, idx: &mut BTreeMap<String, usize>, n: NodeFile) {
        let f = format!("{}.{}.{}", n.layer, n.node_type, n.name);
        idx.insert(f, nodes.len());
        nodes.push(n);
    }

    /// Adds one edge to BOTH endpoint files (SPEC §4.1 pairwise rule — out in
    /// the source's file, matching in in the target's).
    fn tier_edge(
        nodes: &mut [NodeFile],
        idx: &BTreeMap<String, usize>,
        kind: &str,
        from: &str,
        to: &str,
    ) {
        nodes[idx[from]].out.push(out_e(kind, to));
        nodes[idx[to]].in_edges.push(in_e(kind, from));
    }

    /// The re-materialized apg-projects tiers in the layers model: tier 1
    /// (stakeholder/user + the 20 requirements R1–R20 with their depends-on
    /// edges, AC constraints, background/design notes), tier 2 (the
    /// change-sets domain: group/entities/events/value + the four domain laws
    /// as constraints), tier 3 (the apg-cli C4 solution: system + five
    /// containers with `implemented-by` code refs), threaded through the
    /// strictly sequential spine (Requirement —Drives→ Domain —RealisedBy→
    /// Solution —ImplementedBy→ code). Bodies are condensed from
    /// plans/SPEC-apg-projects.md §1–6 and the old-model bootstrap
    /// apg/specs/apg-projects.jsonl — the lossy mapping is the spec-writer's
    /// judgement (SPEC §4.1 "re-materialize instead").
    fn apg_projects_tier_nodes() -> Vec<NodeFile> {
        let mut nodes: Vec<NodeFile> = Vec::new();
        let mut idx: BTreeMap<String, usize> = BTreeMap::new();

        // --- Tier 1: requirements -------------------------------------
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "stakeholder",
                "maintainer",
                "The apg maintainers — anyone with an interest in the projects model and its dogfooding (SPEC §1: a thing that has an opinion).",
                &[],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "user",
                "agent",
                "The codebase agents (navigator/implementer) — a thing that uses the system (SPEC §6 agent flow: the navigator runs `apg project start <name>` from the main checkout and operates with cwd inside the worktree).",
                &[],
            ),
        );
        // (id, feature, body) — condensed from the old-model bootstrap
        // requirements (apg/specs/apg-projects.jsonl), source cites kept.
        let reqs: &[(&str, &str, &str)] = &[
            (
                "R1",
                "project-start",
                "`apg project start <name>` creates the project context in one command: worktree + branch off the repo's DEFAULT branch (symbolic HEAD, not literal main) + auto-scan (worktree + branch + branch DB). Worktree location fixed at <main>/apg/.worktrees/<project>. Always a branch — no escape hatch. Idempotent ONLY when <name> matches the current project context; otherwise hard fail. AC: one command yields worktree + branch + branch DB. Source: SPEC-apg-projects.md §2.1.",
            ),
            (
                "R2",
                "project-start",
                "Hard refusals at project start: non-git dirs (suggest `apg init`); dirty main at start; unborn HEAD; invalid branch names NEVER sanitized (project names inherit git refname constraints); every collision is a case-specific \"project already exists\" naming the actual state and the fix command. `project start` is a main-checkout operation: run from inside a worktree → hard fail (\"you work one project at a time\"). AC: refusal exits 1 with a fix line. Source: SPEC-apg-projects.md §2.1.",
            ),
            (
                "R3",
                "mutation-guard",
                "The project membership guard: guards WRITES only — reads are always allowed; `project start` itself is unguarded (the entry point). Writes refuse outside a project context. Membership = \"the project's worktree, on the project's branch\": current branch == project name AND current checkout is the project's worktree; failure messages name which half failed. Main is never a mutation place — delivered or not. AC: a failure names which half failed. Source: SPEC-apg-projects.md §2.2.",
            ),
            (
                "R4",
                "mutation-guard",
                "The guard lives in the central mutation funnel — write_jsonl_and_reingest (src/artifacts.rs) — beside the existing staleness gate: one check covers all mutations. Consequence: non-git test fixtures gain a real project context (real git fixtures with branch + worktree). Source: SPEC-apg-projects.md §2.3.",
            ),
            (
                "R5",
                "mutation-guard",
                "`apg plan apply` is renamed `apg plan verify` — the binary applies nothing; verify is the pre-merge coherence gate (no remaining planned/dangling nodes + all feedback resolved), guarded (a verdict is only meaningful against the branch's graph). `apg project merge` = verify gate → merge → main rebuild: binary-operated via git2 from the main checkout — the project's terminal lifecycle act; the rebuild is a plain unguarded scan on main. Source: SPEC-apg-projects.md §2.2 + §2.3.",
            ),
            (
                "R6",
                "binary-plumbing",
                "git2 crate with default-features = false (no https/ssh → no OpenSSL). The git CLI is never shelled out to; push/tag remain human acts. VI: cargo check and cargo test pass green with git2 default-features=false. Source: SPEC-apg-projects.md §2.3.",
            ),
            (
                "R7",
                "binary-plumbing",
                "Root resolution split: keep the existing walk-up for checkout-local apg/ (correct in worktrees by construction); git2 for identity (branch, checkout path, main path, worktree existence). Invariant: the git checkout root contains the walked-up apg/ — verify cheaply, error on divergence. Source: SPEC-apg-projects.md §2.3.",
            ),
            (
                "R8",
                "binary-plumbing",
                "Graph mutations auto-commit on the project branch via git2: one commit per logical mutation (single-file diffs); after an auto-commit the staleness gate's recorded scan_meta is re-anchored (DB and tree in sync by construction). Plan mutations NEVER commit — .trans is gitignored and transient. Source: SPEC-apg-projects.md §2.3 + §4.2.",
            ),
            (
                "R9",
                "init-version-gate",
                "`apg init` scaffolds apg/.worktrees/ + its gitignore entry and writes the binary version into apg/config.json as a binary-managed `version` field (user code_type rules untouched). `project start` self-heals the dir/gitignore if absent. Source: SPEC-apg-projects.md §2.4.",
            ),
            (
                "R10",
                "init-version-gate",
                "The version gate BLOCKS, never warns: same major.minor → proceed (patch diff fine); missing `version` → block; major or minor mismatch in EITHER direction → block with upgrade guidance. Applies to `apg scan` and `apg project start` (the layout-touching ops); `apg init` is the upgrade act. Source: SPEC-apg-projects.md §2.4.",
            ),
            (
                "R11",
                "tier-model",
                "One model per layer (2.0 catalog): requirements = Stakeholder/User/Requirement/Note/Constraint with the requirement tree at all depths (theme → epic → feature → story is ONE node type; decompose until each requirement is atomic/testable); domain = Group/Entity/Value/Service/Note/Constraint; solution = System/Container/Component/Person/Note/Constraint; plans = the bridge, .trans-only; implementation = code — scanned, never serialized (Note/Constraint attach only); global = Constraint (the laws) + Notes. Source: SPEC-apg-projects.md §3.1.",
            ),
            (
                "R12",
                "tier-model",
                "Domain semantics use PLAIN names (DDD nomenclature is cryptic): Group hierarchical (groups in groups), attributes core/supporting/generic + optional root (aggregate-groups); BoundedContext/Subdomain/Aggregate/DomainRule collapse into it. Entity kind entity|event (events are ephemeral entities with motion, not a type). Value immutable. Service stateless behaviour. Container kind app/service/db/queue. Solution = C4 only; Person is the C4 view of User/Stakeholder. Stakeholder = anyone with an interest (\"a thing that has an opinion\"); User ⊂ Stakeholder (\"a thing that uses the system\"). Source: SPEC-apg-projects.md §3.1.",
            ),
            (
                "R13",
                "tier-model",
                "Spine strictly sequential — no tier skips (lint): Stakeholder ⊃ Requirement —Drives→ Domain —RealisedBy→ Solution —ImplementedBy→ code. Write-time edge-kind validation matrix (§3.3): contains/drives/realised-by/implemented-by/calls/publishes/subscribes/depends-on/uses/represents/details with their exact source/target shapes. Node rules: name allowlist [a-z0-9][a-z0-9-]* (refuse, never sanitize); type must exist in its layer; Entity requires kind; Group takes core/supporting/generic + optional root; Container takes app/service/db/queue; FQN = <layer>.<type>.<name>; names unique per (layer, type); contains/depends-on trees acyclic; dangling FQN references are write-time errors. Source: SPEC-apg-projects.md §3.2 + §3.3.",
            ),
            (
                "R14",
                "tier-model",
                "Coupling is DERIVED, never stored: A and B are coupled iff a service/event edge chain connects them. The DDD context-map flavors (direct/published/translated/shared/coevolving) are EDGE ATTRIBUTES on calls/publishes/subscribes — never node types, never Group→Group edges. Constraints are PROSE (\"X must hold\") over things that EXIST: the binary validates structure and references at write time; global constraints guard the whole graph; local constraints attach to any tier-1–3 node; SATISFACTION IS ASSESSED BY REVIEW, never executed. Source: SPEC-apg-projects.md §3.1 + §3.3.",
            ),
            (
                "R15",
                "serialization",
                "Layout + identity: the SIX-layer catalog with storage policy separate from it — plans serialize only under apg/.trans/plans/ (transient, per branch); implementation = the code (scanned, never serialized; attach-only note/constraint durable dir); the other four durable. apg/layers/ tree plus the complete .trans mirrors (all six tiers incl. global). One file per node; the file name IS the identity: FQN = <layer>.<type>.<name>, no project prefix. Node-file schema: layer/type/name/body/properties/in/out. Short ids may exist as metadata only, never as identity. Source: SPEC-apg-projects.md §4.1.",
            ),
            (
                "R16",
                "serialization",
                "Edge pairing + atomic write-throughs: BOTH in and out edges live in the node file; an in/out edge in one file without the matching out/in edge (same source, kind, target, AND properties) in the other endpoint's file is an ERROR caught at ingestion; outgoing edges are canonical. Transient-to-durable relationships stay ENTIRELY in .trans. Code endpoints are EXEMPT from the pairwise rule — implemented-by is recorded spec-side only, validated against the scanned graph: resolves → real; planned → pending, not an error; gone → error (spec drift). Renames/deletions are atomic write-throughs: one logical mutation updates ALL affected files and commits once; restore the previous state on failure. Source: SPEC-apg-projects.md §4.1.",
            ),
            (
                "R17",
                "serialization",
                "No migration: legacy apg/specs/*.jsonl and apg/notes/ are NOT read; the version gate blocks old layouts; re-materialize instead — the lossy mapping is the spec-writer's judgement, not a converter's. The old `apg spec` / `apg invariant` command surfaces and the old-model graph vocabulary are removed from the binary — node kinds are exactly the §3.1 catalog plus the code kinds and the transient plan/review kinds; edge kinds are exactly the §3.3 matrix plus the §5 plan/feedback edges. Source: SPEC-apg-projects.md §4.1.",
            ),
            (
                "R18",
                "plans-transient",
                "Plans are tier 4 — the bridge. Plan nodes (PlanPhase, Task, planned Implementation nodes) exist ONLY in apg/.trans/plans/ per branch, never durable. Plan edges: contains (Plan ⊃ PlanPhase ⊃ Task), gates (PlanPhase→PlanPhase), satisfies (PlanPhase→Requirement), and Task→Implementation verbs (creates/modifies/deletes/renames/moves — further verbs reveal themselves through dogfooding). Feedback is transient — branch-lifecycle data, never committed; .trans mirrors the layers structure; Reviews edges link feedback to durable nodes; review state dies with the branch, the reviewed nodes persist. Verification items are the plan's test tier (unit/int/e2e), not graph content. Source: SPEC-apg-projects.md §5.",
            ),
            (
                "R19",
                "plans-transient",
                "Coverage is derived and enforced: every solution node's implemented-by FQN must be touched by at least one plan task; the plan is the HOW for the whole solution; the bridge is complete iff coverage holds. Source: SPEC-apg-projects.md §5.",
            ),
            (
                "R20",
                "rollout",
                "Standalone change-set + dogfood operational flow: the apg repo dogfoods the model — THIS spec is the bootstrap dogfood (authored with the v0.10.4 binary in the old model, within its boundaries; branch/worktree created manually because `apg project start` does not exist yet; from the next feature onward the binary handles it). Agent flow: the navigator runs `apg project start <name>` from the main checkout; the binary prints the worktree path; the navigator operates with cwd inside the worktree; suite tools work unchanged — walk-up discovery finds the worktree's own apg/. Test worktree apg/.worktrees/test stays until before shipping; cleanup deletes no branch. Source: SPEC-apg-projects.md §6.",
            ),
        ];
        for (id, feature, body) in reqs {
            tier_push(
                &mut nodes,
                &mut idx,
                nf(
                    "requirements",
                    "requirement",
                    &id.to_lowercase(),
                    body,
                    &[("id", id), ("feature", feature)],
                ),
            );
        }
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "constraint",
                "ac-start-one-command",
                "R1 AC (§2.1): one `apg project start <name>` command yields the worktree + branch + branch DB — the branch DB exists after start.",
                &[(layers::PROP_ATTACHES_TO, "requirements.requirement.r1")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "constraint",
                "ac-refusals-name-fix",
                "R2 AC (§2.1): every hard-refusal case exits 1 with a fix line on stderr naming the actual state and the command that fixes it.",
                &[(layers::PROP_ATTACHES_TO, "requirements.requirement.r2")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "constraint",
                "ac-membership-names-half",
                "R3 AC (§2.2): a refused mutation's error names which membership half failed (branch half vs worktree half) plus one actionable fix line.",
                &[(layers::PROP_ATTACHES_TO, "requirements.requirement.r3")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "note",
                "bootstrap-dogfood",
                "This session is the bootstrap dogfood: branch/worktree created manually because `apg project start` does not exist yet; from the next feature onward the binary handles it. This change-set is standalone (SPEC §6).",
                &[("kind", "background")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "note",
                "worktree-safety",
                "The fixed worktree location <main>/apg/.worktrees/<project> is a gitignored path INSIDE the main checkout — tested and verified safe; the test worktree apg/.worktrees/test stays until before shipping; cleanup deletes no branch (SPEC §2.1/§6).",
                &[("kind", "background")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "requirements",
                "note",
                "write-through-regression",
                "Regression target (bootstrap note-18): a node rewrite must preserve/rewrite ALL incident edges — never drop them silently. The old-model body-upsert did exactly that; the 2.0 atomic write-through (R16, §4.1) is the fix.",
                &[("kind", "design")],
            ),
        );

        // --- Tier 2: domain -------------------------------------------
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "domain",
                "group",
                "change-sets",
                "The projects-model domain (SPEC §1): a project is a container for a change-set over the graph (and the code that underpins it) — a project ≠ a spec ≠ a plan, it CONTAINS those things. The worktree/branch context, the membership guard, the node-file serialization semantics, and the .trans plan bridge all express this one domain concept.",
                &[("attribute", "core")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "domain",
                "entity",
                "project",
                "The change-set container created by `apg project start <name>`: a git worktree at <main>/apg/.worktrees/<project> on a branch named after the project — branch name == project name is the membership mechanism — off the repo's DEFAULT branch (SPEC §2.1).",
                &[("kind", "entity")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "domain",
                "entity",
                "graph-node",
                "The universal graph node the serialization and pairwise-edge rules talk about: code nodes (scanned, never serialized) and tier-1–3 nodes (one file per node under the apg/layers/ tree) (SPEC §3.1/§5).",
                &[("kind", "entity")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "domain",
                "entity",
                "project-started",
                "The event a change-set comes into being: `apg project start <name>` creates the project context — worktree + branch + auto-scan branch DB in one command (SPEC §2.1).",
                &[("kind", "event")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "domain",
                "entity",
                "node-file-written",
                "The event a node file lands in the worktree: every node/edge mutation writes its file(s) and auto-commits on the project branch — one commit per logical mutation (SPEC §4.2).",
                &[("kind", "event")],
            ),
        );
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "domain",
                "value",
                "node-name",
                "A node's file name IS its identity — the name allowlist [a-z0-9][a-z0-9-]* (refuse, never sanitize); FQN = <layer>.<type>.<name>, no project prefix (SPEC §3.3/§4.1).",
                &[],
            ),
        );
        for (name, body) in [
            (
                "mutation-requires-project",
                "\"It is not possible to mutate the graph without a project — at all\" (SPEC §2.2): writes refuse outside a project context — membership = the project's worktree on the project's branch.",
            ),
            (
                "pairwise-edge-matching",
                "An edge appears in BOTH endpoint files (out in the source's, in in the target's); a match requires the same source, kind, target, AND properties; outgoing edges are canonical (SPEC §4.1).",
            ),
            (
                "plan-covers-solution",
                "Every solution node's implemented-by FQN must be touched by at least one plan task; the bridge is complete iff coverage holds (SPEC §5).",
            ),
            (
                "drift-is-error",
                "An implemented-by code FQN gone from the scanned graph is spec drift and an error — the scanned graph is the stronger check (resolves → real, planned → pending, gone → error) (SPEC §4.1).",
            ),
        ] {
            tier_push(
                &mut nodes,
                &mut idx,
                nf(
                    "domain",
                    "constraint",
                    name,
                    body,
                    &[(layers::PROP_ATTACHES_TO, "domain.group.change-sets")],
                ),
            );
        }

        // --- Tier 3: solution -----------------------------------------
        tier_push(
            &mut nodes,
            &mut idx,
            nf(
                "solution",
                "system",
                "apg-cli",
                "The apg binary itself — the single C4 system that hosts the project commands, the mutation guard, the layers serializer, the plan bridge and the init/version gate (SPEC §2.3/§6).",
                &[],
            ),
        );
        for (name, kind, body) in [
            (
                "project-commands",
                "app",
                "The `apg project` command surface: `apg project start <name>` (§2.1) and `apg project merge` = verify gate → merge → main rebuild, binary-operated via git2 from the main checkout (§2.3).",
            ),
            (
                "mutation-guard",
                "service",
                "The cross-cutting enforcement point: the project membership guard in the central mutation funnel write_jsonl_and_reingest beside the staleness gate (§2.2/§2.3), plus the pre-merge coherence gate behind the renamed `apg plan verify` (§2.2).",
            ),
            (
                "layers-serializer",
                "app",
                "The node-file serializer behind §3 (tier model, edge-kind validation matrix) and §4 (apg/layers/ layout, one file per node, pairwise edges, atomic write-throughs, auto-commit).",
            ),
            (
                "plan-bridge",
                "app",
                "The transient tier-4 bridge (§5): PlanPhase/Task/planned Implementation nodes live only in apg/.trans/plans/ per branch, never durable; Task→Implementation verbs; feedback transient in .trans mirrors.",
            ),
            (
                "init-version-gate",
                "app",
                "`apg init` scaffolding apg/.worktrees/ + gitignore entry + binary-managed `version` in apg/config.json (§2.4), and the version gate (blocks not warns; applies to apg scan + apg project start; init is the upgrade act).",
            ),
        ] {
            tier_push(
                &mut nodes,
                &mut idx,
                nf("solution", "container", name, body, &[("kind", kind)]),
            );
        }

        // --- The spine + trees (paired both halves, SPEC §3.2/§3.3) ----
        let req_fqns: Vec<String> = (1..=20)
            .map(|i| format!("requirements.requirement.r{i}"))
            .collect();
        // contains: User ⊃ every Requirement.
        for r in &req_fqns {
            tier_edge(&mut nodes, &idx, "contains", "requirements.user.agent", r);
        }
        // depends-on (SPEC §5 source ordering; the R8→R6/R7 pair mirrors the
        // bootstrap feedback-1 resolution).
        for (from, to) in [
            ("r2", "r1"),
            ("r3", "r1"),
            ("r4", "r3"),
            ("r5", "r4"),
            ("r7", "r6"),
            ("r8", "r6"),
            ("r8", "r7"),
            ("r10", "r9"),
            ("r15", "r10"),
            ("r16", "r15"),
            ("r17", "r10"),
            ("r19", "r18"),
        ] {
            tier_edge(
                &mut nodes,
                &idx,
                "depends-on",
                &format!("requirements.requirement.{from}"),
                &format!("requirements.requirement.{to}"),
            );
        }
        // drives: every Requirement → the change-sets domain group.
        for r in &req_fqns {
            tier_edge(&mut nodes, &idx, "drives", r, "domain.group.change-sets");
        }
        // contains: the change-sets group hosts its entities/events/value.
        for (name, node_type) in [
            ("project", "entity"),
            ("graph-node", "entity"),
            ("project-started", "entity"),
            ("node-file-written", "entity"),
            ("node-name", "value"),
        ] {
            tier_edge(
                &mut nodes,
                &idx,
                "contains",
                "domain.group.change-sets",
                &format!("domain.{node_type}.{name}"),
            );
        }
        // realised-by: Domain → Solution (the strictly sequential spine).
        tier_edge(
            &mut nodes,
            &idx,
            "realised-by",
            "domain.group.change-sets",
            "solution.system.apg-cli",
        );
        tier_edge(
            &mut nodes,
            &idx,
            "realised-by",
            "domain.entity.project",
            "solution.container.project-commands",
        );
        tier_edge(
            &mut nodes,
            &idx,
            "realised-by",
            "domain.entity.graph-node",
            "solution.container.layers-serializer",
        );
        // contains: System ⊃ the five Containers.
        for c in [
            "project-commands",
            "mutation-guard",
            "layers-serializer",
            "plan-bridge",
            "init-version-gate",
        ] {
            tier_edge(
                &mut nodes,
                &idx,
                "contains",
                "solution.system.apg-cli",
                &format!("solution.container.{c}"),
            );
        }
        // implemented-by: Solution → code FQNs (code-exempt — spec-side only,
        // validated against the scanned graph at write time and at scan).
        for (i, container) in [
            "project-commands",
            "mutation-guard",
            "layers-serializer",
            "plan-bridge",
            "init-version-gate",
        ]
        .iter()
        .enumerate()
        {
            nodes[idx[&format!("solution.container.{container}")]]
                .out
                .push(out_e("implemented-by", IMPL_FQNS[i]));
        }
        // details: notes attach to the nodes they explain.
        tier_edge(
            &mut nodes,
            &idx,
            "details",
            "requirements.note.bootstrap-dogfood",
            "requirements.requirement.r20",
        );
        tier_edge(
            &mut nodes,
            &idx,
            "details",
            "requirements.note.worktree-safety",
            "domain.entity.project",
        );
        tier_edge(
            &mut nodes,
            &idx,
            "details",
            "requirements.note.write-through-regression",
            "domain.constraint.pairwise-edge-matching",
        );

        nodes
    }

    /// The transient plan that pairs with the re-materialized tiers (SPEC §5):
    /// one phase satisfying the rollout requirement, and one `modifies` task
    /// per solution `implemented-by` FQN — derived coverage holds, no planned
    /// nodes, no feedback, so the verify gate passes green. FQNs are
    /// project-prefixed (`<project>/plan…`), matching the branch/project name
    /// the plan JSONL lives under (`.trans/plans/<project>.jsonl`).
    fn apg_projects_plan_records(project: &str) -> Vec<Record> {
        let mut r: Vec<Record> = vec![
            Record::Plan {
                fqn: format!("{project}/plan"),
                title: "apg-projects plan".to_string(),
                strategy:
                    "Bootstrap dogfood: re-materialize the change-set in the layers model (SPEC §6)"
                        .to_string(),
            },
            Record::PlanPhase {
                fqn: format!("{project}/plan.phase-01"),
                number: 1,
                title: "rollout".to_string(),
                deliverable: "re-materialized tiers".to_string(),
                status: "pending".to_string(),
            },
            Record::Contains {
                from: format!("{project}/plan"),
                to: format!("{project}/plan.phase-01"),
            },
            Record::Satisfies {
                from: format!("{project}/plan.phase-01"),
                to: "requirements.requirement.r20".to_string(),
            },
        ];
        for (i, code) in IMPL_FQNS.iter().enumerate() {
            let task = format!("{project}/plan.phase-01.task-{}", i + 1);
            r.push(Record::Task {
                fqn: task.clone(),
                title: format!("touch {code}"),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "modifies".to_string(),
                target: code.to_string(),
                new_fqn: String::new(),
            });
            r.push(Record::Contains {
                from: format!("{project}/plan.phase-01"),
                to: task,
            });
        }
        r
    }

    /// Every durable FQN the re-materialization authors — the e2e asserts the
    /// branch DB holds all of them (tier 1 + tier 2 + tier 3).
    fn expected_tier_fqns() -> Vec<String> {
        let mut fqns: Vec<String> = (1..=20)
            .map(|i| format!("requirements.requirement.r{i}"))
            .collect();
        fqns.extend([
            "requirements.stakeholder.maintainer".to_string(),
            "requirements.user.agent".to_string(),
            "requirements.constraint.ac-start-one-command".to_string(),
            "requirements.constraint.ac-refusals-name-fix".to_string(),
            "requirements.constraint.ac-membership-names-half".to_string(),
            "requirements.note.bootstrap-dogfood".to_string(),
            "requirements.note.worktree-safety".to_string(),
            "requirements.note.write-through-regression".to_string(),
            "domain.group.change-sets".to_string(),
            "domain.entity.project".to_string(),
            "domain.entity.graph-node".to_string(),
            "domain.entity.project-started".to_string(),
            "domain.entity.node-file-written".to_string(),
            "domain.value.node-name".to_string(),
            "domain.constraint.mutation-requires-project".to_string(),
            "domain.constraint.pairwise-edge-matching".to_string(),
            "domain.constraint.plan-covers-solution".to_string(),
            "domain.constraint.drift-is-error".to_string(),
            "solution.system.apg-cli".to_string(),
            "solution.container.project-commands".to_string(),
            "solution.container.mutation-guard".to_string(),
            "solution.container.layers-serializer".to_string(),
            "solution.container.plan-bridge".to_string(),
            "solution.container.init-version-gate".to_string(),
        ]);
        fqns
    }

    #[test]
    fn dogfood_round_trip_re_materializes_tiers_start_author_scan_verify_merge() {
        // A main checkout whose scanned code carries the structs the solution
        // tier's implemented-by edges claim (SPEC §4.1: resolves → real).
        let repo = Repo::new("dogfood-e2e");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(
                MOD,
                FILE,
                &[
                    "Store",
                    "ProjectStart",
                    "MutationGuard",
                    "LayersSerializer",
                    "PlanBridge",
                    "InitVersionGate",
                ],
            ),
        );
        repo.commit_all("seed code");

        // start: one command yields worktree + branch + branch DB, off the
        // default branch (the dogfood flow the next feature will run).
        let wt = project_start_at(&repo.apg_root(), "apg-projects", Some(&start_scan)).unwrap();
        let wt_apg = wt.join(specs::LAYOUT);

        // author: the re-materialized tiers land as node files through the
        // write_project funnel (the exact surface `apg node`/`apg edge`
        // use): membership guard → validate → atomic write → auto-commit →
        // DB re-merge.
        layers::write_project(&wt_apg, &apg_projects_tier_nodes(), &[]).unwrap();

        // author: the transient plan (SPEC §5) — never committed, but it must
        // ingest alongside the durable tiers.
        let plan_path = wt_apg
            .join(specs::TRANS)
            .join("plans")
            .join("apg-projects.jsonl");
        artifacts::write_jsonl_and_reingest(
            &wt_apg,
            &plan_path,
            "apg-projects",
            &apg_projects_plan_records("apg-projects"),
        )
        .unwrap();

        // The branch carries the node files (one auto-commit) and never the
        // plan (R8 — .trans is transient).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let tip = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            tip.tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/requirement/r1.json"))
                .is_ok(),
            "the tier node files must be committed on the project branch"
        );
        assert!(
            tip.tree()
                .unwrap()
                .get_path(Path::new("apg/.trans/plans/apg-projects.jsonl"))
                .is_err(),
            "the plan JSONL must never be committed (.trans is transient)"
        );

        // scan: the branch DB is rebuilt from code + layers + .trans plans —
        // the tiers appear and the transient plan ingests alongside them.
        start_scan(&wt).unwrap();
        let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        for f in expected_tier_fqns() {
            assert!(db.has_node(&f), "branch DB must hold tier node `{f}`");
        }
        for f in [
            "apg-projects/plan",
            "apg-projects/plan.phase-01",
            "apg-projects/plan.phase-01.task-1",
            "apg-projects/plan.phase-01.task-5",
        ] {
            assert!(
                db.has_node(f),
                "branch DB must hold transient plan node `{f}`"
            );
        }
        // The spine is real in the DB: 20 drives edges, the domain→solution
        // realised-by hop, the 5 implemented-by claims onto scanned structs,
        // and the User→Requirement contains tree.
        let count = |q: &str| -> i64 {
            db.q(q)
                .unwrap()
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0)
        };
        assert_eq!(
            count("MATCH (:Requirement)-[:Drives]->(:DomainGroup) RETURN count(*)"),
            20
        );
        assert_eq!(
            count("MATCH (:DomainGroup)-[:RealisedBy]->(:System) RETURN count(*)"),
            1
        );
        assert_eq!(
            count("MATCH (:Container)-[:SpecImplementedBy]->(:Struct) RETURN count(*)"),
            5
        );
        assert_eq!(
            count("MATCH (:User)-[:Contains]->(:Requirement) RETURN count(*)"),
            20
        );
        assert_eq!(
            count("MATCH (:Note)-[:Details]->(:Requirement) RETURN count(*)"),
            1
        );
        drop(db);

        // verify: the coherence gate passes — no planned nodes, no feedback,
        // and every implemented-by FQN is touched by a plan task.
        plan_cmd::plan_verify_at(&wt_apg, "apg-projects").unwrap();

        // merge: verify gate → fast-forward into the default branch → main
        // rebuild (a plain unguarded scan on main).
        project_merge_at(&repo.apg_root(), "apg-projects", Some(&start_scan)).unwrap();

        // The default branch now holds the project's tip with the node files.
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert_eq!(
            main_repo.head().unwrap().peel_to_commit().unwrap().id(),
            tip.id(),
            "main must fast-forward to the project tip"
        );
        assert!(
            repo.root
                .join("apg/layers/solution/container/project-commands.json")
                .exists(),
            "the merged main checkout carries the tier node files"
        );
        assert!(repo.is_clean(), "merged main must be clean");

        // Main rebuild: the main DB has the code + the re-materialized tiers;
        // the transient plan did not cross the merge.
        let main_apg = repo.apg_root();
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        for f in [
            "requirements.requirement.r1",
            "requirements.requirement.r20",
            "domain.group.change-sets",
            "solution.system.apg-cli",
            "fixture.mod.Store",
            "fixture.mod.ProjectStart",
        ] {
            assert!(db.has_node(f), "main DB must hold `{f}` after the rebuild");
        }
        assert!(
            !db.has_node("apg-projects/plan"),
            "transient plans never reach main"
        );
        drop(db);
        assert!(!git::is_stale(&main_apg));
        testutil::remove(&repo);
    }

    // ------------------------------------------------------------------
    // phase-5 task-4 (e2e): the FULL dogfood round trip — suite-tool
    // lookups and mutations with cwd inside the worktree (SPEC §6: walk-up
    // discovery finds the worktree's own apg/ + branch DB, even though the
    // worktree lives INSIDE the main checkout's apg/), verify, merge, main
    // rebuilds unguarded. The main checkout's apg/ is untouched by every
    // in-worktree operation.
    // ------------------------------------------------------------------

    #[test]
    fn full_dogfood_round_trip_suite_tool_ops_inside_the_worktree() {
        // A main checkout whose scanned code carries the structs the solution
        // tier's implemented-by edges claim (resolves -> real), plus one
        // function so the branch-DB lookups assert the
        // module/struct/function triple from the scanned payload.
        let repo = Repo::new("dogfood-full-e2e");
        let mut payload = testutil::code_payload(
            MOD,
            FILE,
            &[
                "Store",
                "ProjectStart",
                "MutationGuard",
                "LayersSerializer",
                "PlanBridge",
                "InitVersionGate",
            ],
        );
        payload.push_str(&testutil::function_line("n7", MOD, "Lookup", FILE));
        repo.write("code/seed.scan.jsonl", &payload);
        repo.commit_all("seed code");

        // A main-checkout scan first: the "untouched" assertions compare
        // against a real main DB (its db.lbug + graph.jsonl must not move
        // during in-worktree operation).
        start_scan(&repo.root).unwrap();
        let main_apg = repo.apg_root();
        let main_db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        assert!(main_db.has_node(format!("{MOD}.Store").as_str()));
        assert!(main_db.has_node(format!("{MOD}.Lookup").as_str()));
        drop(main_db);
        let main_db_bytes = std::fs::read(main_apg.join(specs::TRANS).join("db.lbug")).unwrap();
        let main_graph_bytes =
            std::fs::read(main_apg.join(specs::TRANS).join("graph.jsonl")).unwrap();
        let main_tip = repo.head_sha();

        // 1. start from main: one command yields worktree + branch + branch
        // DB. The worktree lives INSIDE main's apg/ (at
        // <main>/apg/.worktrees/round-trip), so walk-up discovery has a real
        // choice to make — the worktree's own apg/ vs main's apg/ above it.
        let wt = project_start_at(&repo.apg_root(), "round-trip", Some(&start_scan)).unwrap();
        let wt_apg = wt.join(specs::LAYOUT);
        assert!(wt.is_dir());
        assert!(wt_apg.join(specs::TRANS).join("db.lbug").exists());
        let id_wt = git::repo_identity(&wt_apg).unwrap();
        assert!(id_wt.is_worktree);
        assert_eq!(id_wt.branch.as_deref(), Some("round-trip"));
        let id_main = git::repo_identity(&main_apg).unwrap();
        assert!(!id_main.is_worktree);
        assert_eq!(id_main.branch.as_deref(), Some("main"));

        // 2. operate in-worktree: the suite tools shell out to `apg` with cwd
        // inside the worktree, and the binary resolves the layout root by
        // walking up from current_dir. Simulated here by passing a deep
        // in-worktree cwd to the same walk-up (tests never mutate the
        // process-global cwd outside the scan lock).
        let deep_cwd = wt.join("code").join("deep");
        std::fs::create_dir_all(&deep_cwd).unwrap();
        let resolved = specs::find_apg_root(&deep_cwd)
            .expect("walk-up from an in-worktree cwd must find a layout root");
        assert_eq!(
            resolved, wt_apg,
            "walk-up must find the worktree's OWN apg/, not the main checkout's (its parent)"
        );
        assert_ne!(resolved, main_apg);
        // Negative control: from a cwd deep inside MAIN, the same walk-up
        // finds main's apg/ — the discovery is checkout-local, not global.
        let main_deep = repo.root.join("code").join("deep");
        std::fs::create_dir_all(&main_deep).unwrap();
        assert_eq!(specs::find_apg_root(&main_deep), Some(main_apg.clone()));

        // 2a. lookups — the `apg query`-equivalent: open the branch DB found
        // by walk-up and query it; the module/struct/function triple from the
        // scanned payload is there, and no authored tiers yet (the branch DB
        // is the fresh start-scan).
        let db = artifacts::ArtifactDb::open(&resolved).unwrap();
        assert!(db.has_node(MOD), "module from the scanned payload");
        assert!(
            db.has_node(format!("{MOD}.Store").as_str()),
            "struct from the scanned payload"
        );
        assert!(
            db.has_node(format!("{MOD}.Lookup").as_str()),
            "function from the scanned payload"
        );
        assert!(!db.has_node("requirements.requirement.r1"));
        let count = |q: &str| -> i64 {
            db.q(q)
                .unwrap()
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0)
        };
        assert_eq!(count("MATCH (n:Function) RETURN count(*)"), 1);
        drop(db);

        // 2b. mutations — the `apg node add`-equivalent (the exact node_cmd
        // shape through layers::write_project) against the walk-up root:
        // membership guard -> validate -> atomic write -> auto-commit -> DB
        // re-merge, and a fresh query sees the new node.
        layers::write_project(
            &resolved,
            &[nf(
                "requirements",
                "note",
                "dogfood-log",
                "The task-4 dogfood node: authored through the apg node add-equivalent surface with cwd inside the worktree.",
                &[("kind", "background")],
            )],
            &[],
        )
        .unwrap();
        let node_file = wt_apg
            .join(layers::LAYERS_DIR)
            .join("requirements")
            .join("note")
            .join("dogfood-log.json");
        assert!(node_file.exists(), "{} must exist", node_file.display());
        let db = artifacts::ArtifactDb::open(&resolved).unwrap();
        assert!(
            db.has_node("requirements.note.dogfood-log"),
            "the mutation's DB re-merge must make the new node visible to a fresh query"
        );
        drop(db);
        // The node file auto-committed on the project branch (R8).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        assert!(
            wt_repo
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/note/dogfood-log.json"))
                .is_ok(),
            "the node file must be committed on the project branch"
        );

        // 2c. author the re-materialized dogfood tiers (task-1 builder reused)
        // + the transient plan (task-1 builder, project name parametrized).
        layers::write_project(&resolved, &apg_projects_tier_nodes(), &[]).unwrap();
        let plan_path = resolved
            .join(specs::TRANS)
            .join("plans")
            .join("round-trip.jsonl");
        let tip_before_plan = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
        artifacts::write_jsonl_and_reingest(
            &resolved,
            &plan_path,
            "round-trip",
            &apg_projects_plan_records("round-trip"),
        )
        .unwrap();
        // The plan mutation commits nothing (.trans is transient — R8): the
        // branch tip is unchanged and the plan JSONL never enters a commit.
        assert!(plan_path.exists(), "the plan JSONL lands under .trans");
        let wt_tip = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            wt_tip.id(),
            tip_before_plan,
            "plan mutations never commit on the project branch"
        );
        assert!(
            wt_tip
                .tree()
                .unwrap()
                .get_path(Path::new("apg/.trans/plans/round-trip.jsonl"))
                .is_err(),
            "the plan JSONL must never be committed"
        );

        // 2d. the MAIN checkout's apg/ is untouched by all of it: its db.lbug
        // and graph.jsonl are byte-identical, no apg/layers was created there,
        // and main's branch never moved.
        assert_eq!(
            std::fs::read(main_apg.join(specs::TRANS).join("db.lbug")).unwrap(),
            main_db_bytes,
            "main's db.lbug must be unchanged by the in-worktree operations"
        );
        assert_eq!(
            std::fs::read(main_apg.join(specs::TRANS).join("graph.jsonl")).unwrap(),
            main_graph_bytes,
            "main's graph.jsonl must be unchanged by the in-worktree operations"
        );
        assert!(
            !main_apg.join("layers").exists(),
            "no apg/layers may be created under the main checkout"
        );
        assert_eq!(repo.head_sha(), main_tip, "main's branch must not move");

        // 3. scan the worktree (the `apg scan`-equivalent rebuild, cwd inside
        // the worktree): the branch DB now holds code + tiers + plan together.
        start_scan(&wt).unwrap();
        let db = artifacts::ArtifactDb::open(&wt_apg).unwrap();
        for f in expected_tier_fqns() {
            assert!(db.has_node(&f), "branch DB must hold tier node `{f}`");
        }
        assert!(db.has_node("requirements.note.dogfood-log"));
        for f in [
            "round-trip/plan",
            "round-trip/plan.phase-01",
            "round-trip/plan.phase-01.task-1",
            "round-trip/plan.phase-01.task-5",
        ] {
            assert!(
                db.has_node(f),
                "branch DB must hold transient plan node `{f}`"
            );
        }
        drop(db);
        assert_eq!(
            std::fs::read(main_apg.join(specs::TRANS).join("db.lbug")).unwrap(),
            main_db_bytes,
            "a worktree scan must not touch main's DB"
        );

        // 4. verify: the coherence gate passes green — no planned nodes, no
        // feedback, and derived solution coverage holds.
        plan_cmd::plan_verify_at(&wt_apg, "round-trip").unwrap();

        // 5. merge from the main checkout: verify gate -> fast-forward -> main
        // rebuilds unguarded (a plain scan of the main checkout).
        project_merge_at(&repo.apg_root(), "round-trip", Some(&start_scan)).unwrap();

        // The default branch holds the project tip; the merged main checkout
        // carries the node files (the tiers + the dogfood node).
        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert_eq!(
            main_repo.head().unwrap().peel_to_commit().unwrap().id(),
            wt_tip.id(),
            "main must fast-forward to the project tip"
        );
        assert!(
            repo.root
                .join("apg/layers/solution/container/project-commands.json")
                .exists(),
            "the merged main checkout carries the tier node files"
        );
        assert!(
            repo.root
                .join("apg/layers/requirements/note/dogfood-log.json")
                .exists(),
            "the merged main checkout carries the dogfood node"
        );
        assert!(repo.is_clean(), "merged main must be clean");

        // Main rebuild: the main DB has the code + the merged tiers, does NOT
        // hold the transient plan, and its scan_meta is fresh (not stale).
        let db = artifacts::ArtifactDb::open(&main_apg).unwrap();
        for f in [
            "requirements.requirement.r1",
            "requirements.requirement.r20",
            "requirements.note.dogfood-log",
            "domain.group.change-sets",
            "solution.system.apg-cli",
            "solution.container.project-commands",
            format!("{MOD}.Store").as_str(),
            format!("{MOD}.Lookup").as_str(),
            format!("{MOD}.ProjectStart").as_str(),
        ] {
            assert!(db.has_node(f), "main DB must hold `{f}` after the rebuild");
        }
        assert!(
            !db.has_node("round-trip/plan"),
            "transient plans never reach main"
        );
        drop(db);
        assert!(!git::is_stale(&main_apg), "main's scan_meta must be fresh");
        testutil::remove(&repo);
    }

    #[test]
    fn merge_keeps_worktree_and_branch_cleanup_deletes_no_branch() {
        // SPEC §6: "Test worktree apg/.worktrees/test stays until before
        // shipping ... cleanup deletes no branch." The guard: neither the
        // merge act nor a subsequent project start may remove the test
        // worktree or its branch.
        let repo = Repo::new("dogfood-guard");
        repo.write(
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        repo.commit_all("seed code");

        let wt = project_start_at(&repo.apg_root(), "test", Some(&start_scan)).unwrap();
        let wt_apg = wt.join(specs::LAYOUT);
        // Durable content (a node file, auto-committed) + a minimal transient
        // plan (no planned nodes, no feedback) so the verify gate passes.
        write_spec_node(&wt_apg);
        let plan_path = wt_apg.join(specs::TRANS).join("plans").join("test.jsonl");
        artifacts::write_jsonl_and_reingest(
            &wt_apg,
            &plan_path,
            "test",
            &[
                Record::Plan {
                    fqn: "test/plan".to_string(),
                    title: "test plan".to_string(),
                    strategy: String::new(),
                },
                Record::PlanPhase {
                    fqn: "test/plan.phase-01".to_string(),
                    number: 1,
                    title: "P1".to_string(),
                    deliverable: "D".to_string(),
                    status: "pending".to_string(),
                },
                Record::Contains {
                    from: "test/plan".to_string(),
                    to: "test/plan.phase-01".to_string(),
                },
            ],
        )
        .unwrap();
        start_scan(&wt).unwrap();
        plan_cmd::plan_verify_at(&wt_apg, "test").unwrap();

        // Merge: the terminal lifecycle act — and it must NOT clean up the
        // project behind itself.
        project_merge_at(&repo.apg_root(), "test", Some(&start_scan)).unwrap();

        let main_repo = git2::Repository::open(&repo.root).unwrap();
        assert!(
            wt.is_dir(),
            "the test worktree apg/.worktrees/test must survive the merge (SPEC §6)"
        );
        assert!(
            main_repo.find_worktree("test").is_ok(),
            "the test worktree must stay registered after the merge"
        );
        assert!(
            main_repo
                .find_branch("test", git2::BranchType::Local)
                .is_ok(),
            "cleanup deletes no branch — branch `test` must survive the merge"
        );

        // A second project start (a fresh dogfood cycle) must not sweep the
        // test worktree away either.
        let wt2 = project_start_at(&repo.apg_root(), "foo", Some(&start_scan)).unwrap();
        assert!(wt2.is_dir());
        assert!(
            wt.is_dir(),
            "starting another project must not delete the test worktree"
        );
        assert!(
            main_repo
                .find_branch("test", git2::BranchType::Local)
                .is_ok(),
            "starting another project must not delete the test branch"
        );
        testutil::remove(&repo);
    }
}
