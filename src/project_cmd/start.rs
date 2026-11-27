//! `apg project start` — branch + worktree + branch DB in one command.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::artifacts::parse_args;
use crate::git;
use crate::specs;
use crate::version_gate;

use super::cache_copy::{materialise, resolve_policy};
use super::{require_apg_root, validate_project_name, worktree_hosting};

/// `apg project start <name>` — the one-command project context creation.
pub(crate) fn project_start(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let Some(name) = p.positional.first() else {
        anyhow::bail!("usage: apg project start <name>");
    };
    let cache_copy = p.get("cache-copy");
    if cache_copy.is_none() && p.has("cache-copy") {
        anyhow::bail!("usage: `--cache-copy` needs a value (auto, clone, copy or skip)");
    }
    let apg_root = require_apg_root()?;
    let path = project_start_at_with(&apg_root, name, cache_copy.as_deref())?;
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
/// missing (or its database is contended), else creates the branch + worktree
/// and copies main's `apg/.trans` into the branch — no frontend scan. The
/// toolchain cache is materialised separately, by [`project_start_at_with`]
/// under its resolved policy.
pub fn project_start_at(apg_root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    project_start_at_with(apg_root, name, None)
}

/// [`project_start_at`] with the `--cache-copy` CLI value (the highest-priority
/// cache-copy policy source). Takes the spec lock before the freshness gate and
/// holds it through the `.trans` seed copy and the cache materialisation, so a
/// running scan is waited out rather than mis-read as stale.
///
/// The freshness gate has three outcomes, from **one** snapshot: `Fresh`
/// proceeds; `Contended` — a main-database read failure, refused naming the
/// database and its underlying cause — claims contention (`waited`/`budget`,
/// another process holds it) only when the read retried to the budget, never
/// for a terminal first-read error such as EACCES; either way the scan is not
/// stale or missing and a stale-graph `apg scan` is not the fix; `Stale` and
/// `NotApplicable` (a differing input, or no database/HEAD at all) keep the
/// stale-or-missing refusal asking for `apg scan` in the main checkout. The
/// refusals and the version gate run first, then the spec lock, then this
/// gate, then the gitignore self-heal — so a refusal creates no branch/worktree
/// and never mutates main.
pub fn project_start_at_with(
    apg_root: &Path,
    name: &str,
    cache_copy: Option<&str>,
) -> anyhow::Result<PathBuf> {
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

    // Registry: a name that already maps to a worktree is a duplicate start,
    // refused before any side effect on git — the row names the worktree to
    // use instead. Opening the registry creates it under main's gitignored
    // `apg/.trans/`; the handle drops with this statement.
    let registry_path = crate::registry::registry_path(&identity.main_root);
    let existing = crate::registry::Registry::open(&registry_path)?.get(name)?;
    if let Some(row) = existing {
        anyhow::bail!(
            "refused: project `{name}` already has a registry row in the project registry `{reg}` pointing at worktree {wt} — one project name resolves to one worktree. Fix: work in that worktree, or rebuild stale rows with `apg project reconstruct`, then re-run `apg project start {name}`.",
            reg = registry_path.display(),
            wt = row.worktree.display()
        );
    }

    // Spec lock: taken AFTER the refusals and the version gate (a refusing
    // start must not hold it) and BEFORE the freshness gate, held through the
    // seed copy and the cache materialisation. A running scan holds the same
    // flock, so start waits it out instead of reading a half-written scan as
    // stale and refusing.
    let _lock = crate::artifacts::acquire_spec_lock(apg_root)?;

    // Freshness gate: start seeds the branch DB by copying main's scan, so
    // main must have a CURRENT scan for main's HEAD. One snapshot decides the
    // three outcomes. `Fresh` proceeds. `Contended` — a main-database read
    // failure — splits on the retry: a transient lock/busy read retried to
    // the budget is refused as named contention, claiming `waited`/`budget`
    // (another process holds it), while a terminal first read names the
    // database and its underlying cause with no contention claim. Neither is
    // stale or missing, so a stale-graph `apg scan` is not the fix. `Stale`
    // and `NotApplicable` (a differing input, or no `db.lbug`/HEAD at
    // all — `!is_fresh` was true for both) keep the stale-or-missing
    // refusal asking for `apg scan` in main. This sits AFTER `refuse_start`
    // (dirty/collision/in-worktree refusals keep precedence) and the version
    // gate, and BEFORE the gitignore self-heal, so a refusal never mutates
    // the main checkout and never creates a branch/worktree.
    match git::freshness_snapshot(apg_root) {
        git::FreshnessSnapshot::Fresh { .. } => {}
        git::FreshnessSnapshot::Contended {
            database,
            waited,
            budget,
            cause,
            ..
        } => {
            // A terminal first read never retried, so a contention claim would misstate it.
            if waited.is_zero() {
                anyhow::bail!(
                    "refused: the main checkout's scan is not stale or missing — its database at `{}` is unreadable: {cause}",
                    database.display()
                );
            }
            anyhow::bail!(
                "refused: the main checkout's scan is not stale or missing — its database at `{}` is unreadable because another process holds it (waited {waited:?} of a {budget:?} budget for the contention to clear). Fix: retry `apg project start {name}` once that process finishes; a stale-graph `apg scan` is not the fix.",
                database.display()
            )
        }
        git::FreshnessSnapshot::Stale { .. } | git::FreshnessSnapshot::NotApplicable { .. } => {
            anyhow::bail!(
                "refused: the main checkout's scan is stale or missing — `apg project start` copies main's scan into the branch, so main must have a current scan. Fix: run `apg scan` in the main checkout, then re-run `apg project start {name}`."
            );
        }
    }

    // Self-heal (R9): when either apg layout location — the worktree dir or
    // the main checkout's scratch dir — is not gitignored (the repo .gitignore
    // dropped the `apg/.worktrees/` or `apg/.scratch/` entry, or the repo was
    // cloned before it existed), scaffold the apg layout entries and commit
    // the scaffold on the current branch. The dirty-main refusal above
    // guarantees the tree was clean at entry, and a dropped entry must not
    // leave the main checkout dirty (a later start would refuse it). If a
    // location still is not ignored after the scaffold (e.g. an explicit
    // negation shadowing the entries), refuse naming the actual state. The
    // `.worktrees/` dir itself is created below (libgit2's worktree add does
    // not create intermediate dirs; `apg init` scaffolds it, start heals it).
    //
    // The main checkout's scratch dir is healed HERE, before the probe (as the
    // worktrees parent is healed before the worktree add below): a gitignored
    // directory is never cloned, so a fresh clone carries the committed
    // `apg/.scratch/` entry but no directory — and libgit2 matches a dir-only
    // ignore entry only against a directory that exists, so probing an absent
    // `.scratch` reads "not ignored", the scaffold finds nothing missing, and
    // start would refuse over a negation that is not there (or, with the entry
    // also dropped, commit the scaffold and then refuse anyway). Creating it
    // first makes all three states converge: both present (no-op), entry
    // without dir (probe passes, no commit), neither (the scaffold writes the
    // entries and commits once).
    std::fs::create_dir_all(identity.main_root.join(specs::LAYOUT).join(specs::SCRATCH))?;
    let probes = [
        git::project_worktree_dir(&identity.main_root, name),
        identity.main_root.join(specs::LAYOUT).join(specs::SCRATCH),
    ];
    let unignored = || {
        probes
            .iter()
            .filter(|probe| !git::path_is_ignored(&identity.main_root, probe))
            .cloned()
            .collect::<Vec<_>>()
    };
    if !unignored().is_empty() {
        if crate::scaffold_gitignore(&identity.main_root)? {
            git::commit_file(
                &identity.main_root,
                &identity.main_root.join(".gitignore"),
                "apg: scaffold .gitignore entries for the apg layout",
            )?;
        }
        if let Some(probe) = unignored().first() {
            anyhow::bail!(
                "refused: {} is still not gitignored after scaffolding the apg layout entries — an explicit .gitignore negation must be shadowing them. Fix: remove the negation (or append the matching apg layout entry — `apg/.worktrees/` or `apg/.scratch/` — at the end of the .gitignore), then re-run `apg project start {name}`.",
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
    // Materialise the worktree's toolchain cache from the main checkout's by
    // the policy resolved CLI > `APG_CACHE_COPY` > `apg/config.json`
    // `cache.project-start` > auto. A missing source cache or an existing
    // destination is a no-op; a clone/copy failure fails the start (the
    // materialiser leaves no partial destination behind).
    let src_cache = apg_root.join(specs::TRANS).join("cache");
    let dst_cache = wt_apg.join(specs::TRANS).join("cache");
    let env_value = std::env::var("APG_CACHE_COPY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let config_value = crate::classify::ApgConfig::load(&identity.main_root)
        .and_then(|c| c.cache)
        .and_then(|s| s.project_start)
        .filter(|v| !v.trim().is_empty());
    let policy = resolve_policy(cache_copy, env_value.as_deref(), config_value.as_deref())?;
    materialise(&src_cache, &dst_cache, policy)?;
    // Seed the new worktree's fact-store rows from the main checkout's, under
    // the spec lock: refs/scan/deps rows are per-worktree, so without this the
    // worktree's landing scan finds no recorded scan for its own id and
    // full-scans. A store that cannot be resolved or a missing main row is a
    // graceful no-op — the landing scan then full-scans as before.
    seed_fact_rows(apg_root, &identity.main_root, &wt_dir)?;
    // The copy only seeds `.trans`; the worktree also needs the scratch dir
    // the researcher writes its result markdown into.
    std::fs::create_dir_all(wt_apg.join(specs::SCRATCH))?;
    // Register the project's row: the one index a bare name resolves through,
    // carrying the worktree, the branch libgit2 just created, and the calling
    // session as owner (a raw CLI call has none). A registry write failure
    // fails the start rather than leaving an unregistered context behind.
    let wt_root = std::fs::canonicalize(&wt_dir).unwrap_or_else(|_| wt_dir.clone());
    let mut entry = crate::registry::RegistryEntry::new(name, wt_root);
    entry.branch = Some(name.to_string());
    entry.owner = std::env::var("APG_CALLER_SESSION")
        .ok()
        .filter(|v| !v.trim().is_empty());
    crate::registry::Registry::open(&registry_path)?
        .put(&entry)
        .with_context(|| {
            format!(
                "registering project `{name}` in the project registry at {}",
                registry_path.display()
            )
        })?;
    Ok(wt_dir)
}

/// Seeds the new worktree's fact-store rows from the main checkout's so the
/// worktree's landing scan is incremental instead of a cold full scan: the main
/// checkout's refs/manifest, scan record and `deps`/`signatures`/`overloads`
/// rows are copied under the worktree's own [`crate::cache::worktree_id`] and
/// committed in one transaction. A store that cannot be resolved or a missing
/// main row is a graceful no-op (the landing scan then full-scans); a write
/// failure on the resolved store propagates.
fn seed_fact_rows(main_apg_root: &Path, main_root: &Path, wt_dir: &Path) -> anyhow::Result<()> {
    let store = match crate::cache::FactStore::resolve(main_apg_root) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("[project] fact-store seed skipped: {err:#}");
            return Ok(());
        }
    };
    // No main row means there is no baseline to seed (an unscanned main
    // checkout); the worktree's first scan records one itself.
    let Some(main) = crate::cache::Manifest::load(&store.root, main_root) else {
        return Ok(());
    };
    let main_id = main.worktree_id();
    let wt_id = crate::cache::worktree_id(wt_dir);
    // The registry string is the worktree's own canonical root — the collector
    // tests its liveness with `is_dir`, so main's root would be wrong.
    let wt_root = std::fs::canonicalize(wt_dir).unwrap_or_else(|_| wt_dir.to_path_buf());
    store.put_refs(wt_id, &main.entries)?;
    store.register_worktree(wt_id, &wt_root.to_string_lossy())?;
    // The opaque per-worktree rows: absent rows (a store written before one
    // existed) simply seed nothing, and the landing scan records them.
    if let Some(scan) = store.scan(main_id) {
        store.put_scan(wt_id, &scan)?;
    }
    if let Some(deps) = store.deps(main_id) {
        store.put_deps(wt_id, &deps)?;
    }
    if let Some(signatures) = store.signatures(main_id) {
        store.put_signatures(wt_id, &signatures)?;
    }
    if let Some(overloads) = store.overloads(main_id) {
        store.put_overloads(wt_id, &overloads)?;
    }
    store.save_index()
}

/// Recursively copies the main checkout's `apg/.trans` scan into the new
/// worktree's `apg/.trans` (files and nested directories), skipping the
/// **top-level** `cache` entry — the toolchain cache is materialised separately
/// by the cache-copy policy (a nested directory merely named `cache` is an
/// ordinary scan artifact and still copies). Reads only the `.trans` tree —
/// never the source tree — creates `dst`, and is a no-op when `src` and `dst`
/// are the same directory.
fn copy_trans_artifacts(src: &Path, dst: &Path) -> anyhow::Result<()> {
    copy_trans_contents(src, dst, true)
}

fn copy_trans_contents(src: &Path, dst: &Path, top_level: bool) -> anyhow::Result<()> {
    if src == dst {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if top_level && entry.file_name().as_os_str() == std::ffi::OsStr::new("cache") {
            continue;
        }
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_trans_contents(&entry.path(), &to, false)?;
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
