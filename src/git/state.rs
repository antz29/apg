use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lbug::{Connection, Database};

use crate::artifacts::db::system_config;
use crate::specs;
use crate::splice::seed::{cell, query_rows};

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

// ---------------------------------------------------------------------------
// git2 plumbing helpers
// ---------------------------------------------------------------------------

/// Discovers the repo enclosing `dir` (walking up, like `git CLI`).
pub(crate) fn discover_repo(dir: &Path) -> anyhow::Result<git2::Repository> {
    git2::Repository::discover(dir).map_err(|e| {
        anyhow::anyhow!(
            "mutation refused — no project context: the checkout holds no branch ({e} — {} is not inside a git repository). Fix: run `git init`, commit an initial state, then `apg init`; from the main checkout run `apg project start <name>` and work inside its worktree.",
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
pub(crate) fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Canonicalize a path whose leaf may no longer exist (a staged deletion):
/// canonicalize the deepest existing ancestor and re-append the missing
/// components. Falls back to the lexical path when nothing resolves. Needed
/// because `std::fs::canonicalize` follows symlinks only through existing
/// components — a removed file would otherwise compare un-canonicalized
/// against the canonical workdir (the `/var` → `/private/var` macOS alias).
pub(crate) fn canonical_allow_missing(path: &Path) -> PathBuf {
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
pub(crate) fn head_sha(repo: &git2::Repository) -> Option<String> {
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
    let Some(sha) = head_sha(&repo) else {
        return GitState {
            sha: None,
            clean: false,
            content_key: None,
        };
    };
    // ONE status collection yields both facts (cleanliness and content key).
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let (clean, key) = match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => {
            let workdir = repo.workdir().map(Path::to_path_buf);
            let index = repo.index().ok();
            let entries: Vec<StatusEntry> = statuses
                .iter()
                .map(|e| {
                    let path = e.path().unwrap_or_default().to_string();
                    let index_oid = index
                        .as_ref()
                        .and_then(|i| i.get_path(Path::new(&path), 0))
                        .map(|ie| ie.id.to_string());
                    let content = workdir
                        .as_ref()
                        .map(|wd| wd.join(&path))
                        .filter(|full| full.is_file())
                        .and_then(|full| std::fs::read(full).ok());
                    StatusEntry {
                        path,
                        bits: e.status().bits(),
                        index_oid,
                        content,
                    }
                })
                .collect();
            fold_status_entries(&sha, &entries)
        }
        // A failed walk fails closed: not clean, and the HEAD-only digest — an
        // empty collection cannot express the error.
        Err(_) => (false, fold_status_entries(&sha, &[]).1),
    };
    GitState {
        sha: Some(sha),
        clean,
        content_key: Some(key),
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

/// One `git status` entry flattened to plain data, so [`fold_status_entries`]
/// needs no repository, index or filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    /// The entry's `git2` status bits.
    pub bits: u32,
    /// The staged (index) blob oid for the path, when it is in the index.
    pub index_oid: Option<String>,
    /// The working-tree file's bytes, when the path is an existing file.
    pub content: Option<Vec<u8>>,
}

/// Folds one status collection into `(clean, content_key)`: clean when the
/// collection is empty, and the content-identity digest over the HEAD sha plus
/// each entry's path, status bits, index oid and working-tree bytes. Entries
/// are sorted by `(path, bits)` first, so the digest is order-independent.
pub fn fold_status_entries(head: &str, entries: &[StatusEntry]) -> (bool, String) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    fnv1a(&mut h, head.as_bytes());
    let mut ordered: Vec<&StatusEntry> = entries.iter().collect();
    ordered.sort_by(|a, b| (a.path.as_str(), a.bits).cmp(&(b.path.as_str(), b.bits)));
    for e in ordered {
        fnv1a(&mut h, e.path.as_bytes());
        fnv1a(&mut h, &e.bits.to_le_bytes());
        if let Some(oid) = &e.index_oid {
            fnv1a(&mut h, oid.as_bytes());
        }
        if let Some(bytes) = &e.content {
            fnv1a(&mut h, bytes);
        }
    }
    (entries.is_empty(), format!("{h:016x}"))
}

/// The recorded state of the scan that built the live DB (its own `Scan` row,
/// [`db_recorded_scan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedScan {
    pub sha: String,
    pub clean: bool,
    /// The content-identity key; `None` on a pre-hardening record (freshness
    /// cannot be verified then).
    pub content_key: Option<String>,
    /// The scan's resolved `--module` selection — canonical absolute roots,
    /// newline-joined; empty for a whole-tree scan.
    pub modules: String,
}

/// The outcome of **one** freshness evaluation: the verdict plus the captured
/// checkout state and recorded scan it was derived from, so every
/// refuse-on-stale site renders its reasons from this one capture instead of
/// re-reading git or the database.
///
/// The arm *is* the verdict — [`NotApplicable`](Self::NotApplicable) when there
/// is nothing to compare, [`Fresh`](Self::Fresh) when every input matches, and
/// [`Stale`](Self::Stale) when at least one input differs. The variant set is
/// open by design: a later transient-unreadable-database outcome takes a
/// further arm rather than a closed shape, so a non-stale read failure never
/// has to masquerade as a stale verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessSnapshot {
    /// Nothing to compare: no `db.lbug`, or no git repo / unborn HEAD.
    /// `differing` names the precondition that made it N/A (`"database"` or
    /// `"git sha"`) so the collect-all callers can still report it.
    NotApplicable {
        current: GitState,
        recorded: Option<RecordedScan>,
        differing: Vec<&'static str>,
    },
    /// Every freshness input matches the recorded scan.
    Fresh {
        current: GitState,
        recorded: Option<RecordedScan>,
    },
    /// At least one input differs, each named in `differing` — never empty, so
    /// a stale verdict always names at least one differing input.
    Stale {
        current: GitState,
        recorded: Option<RecordedScan>,
        differing: Vec<&'static str>,
    },
}

/// The recorded state of the scan that built the live DB, read from the DB's
/// **own** single `Scan` row (SCAN_HEAD) in `<apg_root>/.trans/db.lbug` — the
/// DB's account of itself. This row is the ONE recorded-state source of the
/// freshness check ([`is_fresh`]).
///
/// The DB is opened **read-only** and never modified. Every failure **fails
/// closed** and never panics: a missing, unopenable or locked DB; a DB with no
/// `Scan` row; an empty `git_sha` (a non-git scan); a `git_clean` that is
/// neither `"true"` nor `"false"`; or an empty `content_key` (a pre-hardening
/// DB). Every field is parsed from the row itself: an empty `git_sha` means
/// non-git, `git_clean` is `"true"`/`"false"`, an empty `content_key` is
/// `None`, and the module selection is the `modules` column verbatim (empty for
/// a whole-tree scan).
pub fn db_recorded_scan(apg_root: &Path) -> Option<RecordedScan> {
    let db = Database::new(db_path(apg_root), system_config(true).ok()?).ok()?;
    let conn = Connection::new(&db).ok()?;
    let (_, rows) = query_rows(
        &conn,
        "MATCH (s:Scan) RETURN s.git_sha AS git_sha, s.git_clean AS git_clean, s.content_key AS content_key, s.modules AS modules",
    )
    .ok()?;
    let row = rows.first()?;
    let sha = cell(row, 0);
    if sha.is_empty() {
        return None;
    }
    let clean = match cell(row, 1).as_str() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    let key = cell(row, 2);
    Some(RecordedScan {
        sha,
        clean,
        content_key: (!key.is_empty()).then_some(key),
        modules: cell(row, 3),
    })
}

/// The same `Scan` row [`db_recorded_scan`] reads, taken through the live
/// session's routed read instead of opening a second handle on the
/// session-owned DB. Parses the `apg query --json` rows and fails closed
/// exactly like the direct read (an empty sha, a non-`true`/`false` clean, or
/// an empty content key ⇒ `None`); the module selection is the routed
/// `modules` field, so the two reads yield the same [`RecordedScan`].
fn db_recorded_scan_routed(apg_root: &Path) -> Option<RecordedScan> {
    let out = crate::session::forward_query(
        apg_root,
        "MATCH (s:Scan) RETURN s.git_sha AS git_sha, s.git_clean AS git_clean, s.content_key AS content_key, s.modules AS modules",
        true,
    )
    .ok()?;
    let rows: serde_json::Value = serde_json::from_str(&out).ok()?;
    let row = rows.as_array()?.first()?.as_object()?;
    let field = |name: &str| {
        row.get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let sha = field("git_sha");
    if sha.is_empty() {
        return None;
    }
    let clean = match field("git_clean").as_str() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    let key = field("content_key");
    Some(RecordedScan {
        sha,
        clean,
        content_key: (!key.is_empty()).then_some(key),
        modules: field("modules"),
    })
}

/// Collects the freshness facts from **one** capture of the database and the
/// caller's already-captured [`GitState`]: whether `db.lbug` exists, its own
/// [`RecordedScan`] row (when readable) and the other DB facts (pending state,
/// schema, authored digest, session socket), folded into the list of inputs
/// that differ (empty = fresh).
///
/// The precondition failures short-circuit exactly as the collect-all form
/// always has — `"database"`, `"socket"`, `"git sha"` and `"recorded scan"`
/// each reported alone — so [`freshness_differences_with`]'s `Ok`/`Err` is
/// unchanged. [`freshness_snapshot`] shares this gather, so the labels and the
/// verdict come from one evaluation and cannot drift.
fn freshness_facts(
    apg_root: &Path,
    state: &GitState,
    requested: Option<&[String]>,
) -> (bool, Option<RecordedScan>, Vec<&'static str>) {
    if !db_path(apg_root).exists() {
        return (false, None, vec!["database"]);
    }
    let socket = crate::session::socket_path(apg_root);
    let live_session = socket.exists() && crate::session::live_session_at(&socket);
    if socket.exists() && !live_session {
        return (true, None, vec!["socket"]);
    }
    let Some(cur_sha) = state.sha.as_deref() else {
        return (true, None, vec!["git sha"]);
    };
    let Some(rec) = (if live_session {
        db_recorded_scan_routed(apg_root)
    } else {
        db_recorded_scan(apg_root)
    }) else {
        return (true, None, vec!["recorded scan"]);
    };
    let mut differing: Vec<&'static str> = Vec::new();
    if !live_session {
        if crate::artifacts::db::db_has_pending_state(apg_root) {
            differing.push("pending state");
        }
        if crate::splice::seed::validate_compatible(&db_path(apg_root)).is_err() {
            differing.push("schema");
        }
        let authored_ok = match crate::splice::seed::seed_authored_identity(&db_path(apg_root)) {
            Ok(db) => {
                crate::splice::seed::tree_authored_identity(apg_root).is_ok_and(|tree| tree == db)
            }
            Err(_) => false,
        };
        if !authored_ok {
            differing.push("authored digest");
        }
    }
    match rec.content_key.as_deref() {
        // Pre-hardening: freshness cannot be verified.
        None => differing.push("content key"),
        Some(rec_key) => {
            if state.content_key.as_deref() != Some(rec_key) {
                differing.push("content key");
            }
        }
    }
    if rec.sha != cur_sha {
        differing.push("sha");
    }
    if rec.clean != state.clean {
        differing.push("cleanliness");
    }
    if let Some(requested) = requested
        && modules_differ(&rec.modules, requested)
    {
        differing.push("modules");
    }
    (true, Some(rec), differing)
}

/// Maps already-captured freshness facts to the [`FreshnessSnapshot`] verdict.
///
/// **PURE**: no git, filesystem, database or process access — the caller
/// ([`freshness_snapshot`]) does the one capture and this only folds it, so the
/// unit tier can test the verdict in memory. `NotApplicable` when there is no
/// database or no HEAD sha (not a repo / unborn HEAD); `Fresh` when the
/// differing-label set is empty; `Stale` carrying the non-empty label set
/// otherwise — so a stale verdict can never render an empty `differing` list.
pub fn classify_freshness(
    current: GitState,
    recorded: Option<RecordedScan>,
    database_present: bool,
    differing: Vec<&'static str>,
) -> FreshnessSnapshot {
    if !database_present || current.sha.is_none() {
        return FreshnessSnapshot::NotApplicable {
            current,
            recorded,
            differing,
        };
    }
    if differing.is_empty() {
        FreshnessSnapshot::Fresh { current, recorded }
    } else {
        FreshnessSnapshot::Stale {
            current,
            recorded,
            differing,
        }
    }
}

/// The single freshness evaluation the refuse-on-stale sites share: **one**
/// [`git_state`] read plus **one** pass over the database's own facts
/// ([`freshness_facts`]), folded by the pure [`classify_freshness`] into a
/// [`FreshnessSnapshot`] carrying the verdict, the captured current
/// [`GitState`] and the recorded [`RecordedScan`]. Nothing in the path
/// re-reads, so a later projection derives its refusal message from this one
/// capture rather than re-evaluating freshness.
///
/// The module-selection comparison is not applied (`requested` is `None`):
/// this is the gate-shaped evaluation, whose callers carry no `--module`
/// selection.
pub fn freshness_snapshot(apg_root: &Path) -> FreshnessSnapshot {
    let current = git_state(apg_root);
    let (database_present, recorded, differing) = freshness_facts(apg_root, &current, None);
    classify_freshness(current, recorded, database_present, differing)
}

/// The content-identity freshness predicate (win A): the live DB is reusable
/// as-is iff it exists AND the current tree's content identity matches the git
/// state recorded in the DB's **own** `Scan` row exactly — recorded HEAD sha,
/// cleanliness and content-identity key all equal the current values. mtime is
/// never consulted: a touch without a byte change stays fresh, a byte edit is
/// stale.
///
/// The verdict is exactly [`freshness_differences_with`]'s — the body is
/// `freshness_differences_with(apg_root, state, requested).is_ok()` — so the
/// predicate and the collect-all form can never disagree. `requested` is the
/// caller's resolved `--module` selection: `None` skips the module comparison
/// (the gates that carry no selection), `Some(set)` compares it to the recorded
/// one.
///
/// The DB-existence precondition comes FIRST. The fast-path's action is "reuse
/// the existing DB", so with no `db.lbug` at all there is nothing to reuse →
/// NOT fresh. A missing/pre-hardening recorded key means freshness cannot be
/// verified → NOT fresh. The recorded state is read from the DB alone
/// ([`db_recorded_scan`]); the DB↔export agreement half has retired, so
/// `graph.jsonl` plays no part in the verdict.
///
/// **Live session**: while a live session owns `db.lbug`, the same `Scan` row
/// is taken through the session's routed read ([`db_recorded_scan_routed`])
/// instead of opening a second handle on the session-owned DB, so a live
/// session never makes the on-disk fast path unfresh. The pending-state, table
/// shape and authored/transient checks below are SHORT-CIRCUITED then; the
/// recorded/git content comparison runs for both cases.
///
/// **Authored/transient reconciliation** (no-live-session branch only): the
/// DB's own authored/transient digest ([`crate::splice::seed::seed_authored_identity`])
/// must also equal the on-disk tree's ([`crate::splice::seed::tree_authored_identity`])
/// — the independent source of truth. This is the recovery safety net: a
/// durability commit that re-anchors the DB `Scan` row to the current state
/// cannot mask an authored row the DB dropped, because the DB digest is
/// compared to the tree, never to a value refreshed from the DB. An error
/// computing either digest is NOT fresh (fail closed).
///
/// **Pending state and table shape** (no-live-session branch only): the DB must
/// carry no pending state — no dirty row and no pending-removal identity
/// ([`crate::artifacts::db::db_has_pending_state`]) — because an admitted but
/// unsaved mutation leaves the derived DB ahead of the last saved state, which
/// the on-disk fast path must not serve; and its tables must match this
/// binary's schema ([`crate::splice::seed::validate_compatible`]), because a
/// database another binary wrote cannot be reasoned about. Either verdict is
/// NOT fresh (fail closed). Both reads sit in the no-live-session branch, so a
/// live session short-circuits them.
///
/// A session socket that is present but fails a connect/`Ping` is also NOT
/// fresh: an unclean session exit leaves the socket behind while the derived
/// DB reflects only the last saved state, so a phantom projection exists and
/// the on-disk DB cannot be trusted until a rebuild.
///
/// Every failure **fails closed** (never panics): a missing/unopenable/locked
/// DB, a DB with no readable `Scan` row, or a mismatch all mean "not fresh".
///
/// This is deliberately not `!is_stale`: `is_stale` is N/A (false) when there
/// is no DB or the dir is not a git repo, so `is_stale != !is_fresh` there.
///
/// The final recorded/git comparison consumes the caller's `state` (its `sha`,
/// `clean` and `content_key`) instead of re-reading the tree, so a caller that
/// already captured a [`GitState`] pays for one status walk, not two.
pub fn is_fresh_with_state(
    apg_root: &Path,
    state: &GitState,
    requested: Option<&[String]>,
) -> bool {
    freshness_differences_with(apg_root, state, requested).is_ok()
}

/// The collect-all form of [`is_fresh_with_state`]: `Ok(())` when the DB is
/// reusable as-is, otherwise `Err` naming EVERY freshness input that differs
/// between the scan recorded in the DB's own `Scan` row and the current
/// checkout — one entry per differing input, never short-circuited. The verdict
/// is exactly [`is_fresh_with_state`]'s (`Err` ⇔ not fresh), so that predicate
/// can be `freshness_differences(apg_root, state).is_ok()`, and the STALE
/// refusal / scan preamble can name every reported input.
///
/// The label strings (stable — pinned by tests):
///
/// - `"database"` — no `db.lbug` under `apg_root`;
/// - `"socket"` — a session socket exists but fails its connect/`Ping`;
/// - `"git sha"` — the checkout has no HEAD sha (not a git repo / unborn HEAD);
/// - `"recorded scan"` — the DB has no readable, hardening-era `Scan` row;
/// - `"pending state"` — the DB carries an admitted-but-unsaved mutation;
/// - `"schema"` — the DB's table shape is not this binary's;
/// - `"authored digest"` — the DB's authored/transient digest differs from the
///   tree's (or either digest could not be computed);
/// - `"content key"` — the recorded content-identity key is absent
///   (pre-hardening) or differs from the current tree's;
/// - `"sha"` — the recorded HEAD sha differs from the current one;
/// - `"cleanliness"` — the recorded cleanliness differs from the current one;
/// - `"modules"` — the requested resolved `--module` set differs from the
///   recorded module selection (order- and duplicate-insensitive). Reported only
///   by [`freshness_differences_with`] with a `Some` requested set; the two-arg
///   [`freshness_differences`] passes `None` and never reports it.
///
/// [`freshness_differences`] is the no-requested-set comparison: it delegates
/// with `None`, so `refusal_message` and `plan_verify_at` keep their existing
/// calls and are unaffected by the module selection.
///
/// The first four are precondition failures — without them the remaining inputs
/// cannot be evaluated, so each is reported alone. The rest are collected
/// together. The live-session read skip is preserved exactly: pending state,
/// schema and authored digest are evaluated only when no live session owns
/// `db.lbug`; the recorded/git content comparison runs either way.
pub fn freshness_differences(apg_root: &Path, state: &GitState) -> Result<(), Vec<&'static str>> {
    freshness_differences_with(apg_root, state, None)
}

/// Whether the requested resolved module set differs from the recorded
/// canonical selection (`\n`-joined, empty for a whole-tree scan). Both sides
/// compare as sets, so request order and duplicates never change the verdict:
/// empty recorded + empty requested is equal; empty versus non-empty differs.
fn modules_differ(recorded: &str, requested: &[String]) -> bool {
    let recorded: BTreeSet<&str> = recorded
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let requested: BTreeSet<&str> = requested
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    recorded != requested
}

/// [`freshness_differences`] with the caller's resolved `--module` selection.
/// `requested` `None` skips the module comparison (the gates that carry no
/// selection); `Some(set)` additionally reports `"modules"` when the requested
/// resolved set differs from the recorded module selection.
///
/// This is the collect-all projection of the one shared freshness evaluation:
/// the snapshot is built from the shared gather ([`freshness_facts`], also used
/// by [`freshness_snapshot`]) folded by the pure [`classify_freshness`] with the
/// caller's already-captured `state` and `requested` selection, and the verdict
/// is projected from that snapshot — `Ok(())` ⇔
/// [`Fresh`](FreshnessSnapshot::Fresh), `Err(differing)` otherwise.
/// [`NotApplicable`](FreshnessSnapshot::NotApplicable) is NOT short-circuited:
/// it reports its precondition label (`"database"`, `"socket"` or `"git sha"`)
/// exactly as the collect-all form always has, so the label set and the
/// `Ok`/`Err` partition match the shared snapshot.
pub fn freshness_differences_with(
    apg_root: &Path,
    state: &GitState,
    requested: Option<&[String]>,
) -> Result<(), Vec<&'static str>> {
    let (database_present, recorded, differing) = freshness_facts(apg_root, state, requested);
    match classify_freshness(state.clone(), recorded, database_present, differing) {
        FreshnessSnapshot::Fresh { .. } => Ok(()),
        FreshnessSnapshot::NotApplicable { differing, .. }
        | FreshnessSnapshot::Stale { differing, .. } => Err(differing),
    }
}

/// The `&Path` entry point to [`is_fresh_with_state`]: the DB-existence
/// precondition runs first (no DB → NOT fresh without any git walk); otherwise
/// it captures the current [`GitState`] once and delegates to the state-based
/// predicate, so the freshness verdict is one shared rule.
pub fn is_fresh(apg_root: &Path) -> bool {
    if !db_path(apg_root).exists() {
        return false;
    }
    is_fresh_with_state(apg_root, &git_state(apg_root), None)
}

/// The refuse-on-stale predicate, projected from the one shared freshness
/// evaluation ([`freshness_snapshot`]): **true iff** that snapshot is
/// [`FreshnessSnapshot::Stale`].
///
/// - [`NotApplicable`](FreshnessSnapshot::NotApplicable) → **false**: no
///   `db.lbug` (nothing to be stale), or no git repo / unborn HEAD (no
///   recorded state to compare). These N/A short-circuits are why this is not
///   simply `!is_fresh`.
/// - [`Fresh`](FreshnessSnapshot::Fresh) → **false**: every freshness input
///   matches the recorded scan.
/// - [`Stale`](FreshnessSnapshot::Stale) → **true**: at least one input
///   differs, always named in the snapshot's non-empty `differing` list. A
///   missing/pre-hardening recorded key counts here (freshness cannot be
///   verified).
///
/// The verdict comes from the single snapshot capture — one git read plus one
/// pass over the database's own facts — never a second freshness pass, so the
/// refusal gate and the sites rendering the reasons derive from one evaluation.
pub fn is_stale(apg_root: &Path) -> bool {
    matches!(
        freshness_snapshot(apg_root),
        FreshnessSnapshot::Stale { .. }
    )
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
///
/// The exact shape (stable — pinned by tests):
///
/// `graph is stale (recorded <rec>, current <cur>; differing: <labels>) — run `apg scan` before mutating`
///
/// `<rec>`/`<cur>` are [`state_str`] (`<sha>@<clean>`, or `-@-` when there is
/// no sha to display) and `<labels>` is [`freshness_differences`]' reason list
/// joined by `, `, so every differing freshness input is named: `database`,
/// `socket`, `git sha`, `recorded scan`, `pending state`, `schema`,
/// `authored digest`, `content key`, `sha`, `cleanliness`.
pub fn refusal_message(apg_root: &Path) -> Option<String> {
    if !is_stale(apg_root) {
        return None;
    }
    let current = git_state(apg_root);
    let Err(differing) = freshness_differences(apg_root, &current) else {
        // A tree that moved between the `is_stale` gate and the read above.
        return None;
    };
    let recorded = match db_recorded_scan(apg_root) {
        Some(rec) => state_str(Some(&rec.sha), rec.clean),
        None => state_str(None, false),
    };
    let cur = state_str(current.sha.as_deref(), current.clean);
    Some(format!(
        "graph is stale (recorded {recorded}, current {cur}; differing: {}) — run `apg scan` before mutating",
        differing.join(", ")
    ))
}

/// The one-line staleness summary `apg scan` prints (requirement 5): recorded
/// `<sha>@<clean>` vs current `<sha>@<clean>` → `STALE`/`FRESH`, evaluated
/// against the *pre-scan* DB before the new scan overwrites it. N/A when the
/// scan is not in a git repo, or when there is no prior scan to be stale.
///
/// The exact line shapes:
///
/// - `Git state: N/A (not a git repo)` — no HEAD sha to compare;
/// - `Git state: no scan yet (current <sha>@<clean>)` — no `db.lbug`;
/// - `Git state: recorded <rec> vs current <cur> → FRESH` — byte-identical to
///   the pre-collect-all line, with no parenthetical;
/// - `Git state: recorded <rec> vs current <cur> → STALE (differing: <labels>)`
///   — `<labels>` is [`freshness_differences_with`]' list joined by `, `, so
///   every differing freshness input is named, the `modules` selection included
///   when `requested` is `Some` and differs from the recorded one. A DB with no
///   readable `Scan` row reports the single `recorded scan` label with recorded
///   `-@-`.
///
/// The verdict is the SAME content-identity rule [`is_fresh_with_state`] uses —
/// it is [`freshness_differences_with`] with the same `requested` selection — so
/// the printed line and the fast-path decision can never disagree (a
/// recorded-dirty tree whose content digest changed at the same sha prints
/// STALE, never FRESH followed by a full pipeline run). The one exception is
/// `apg scan --force`: it prints this pre-force verdict and then discards the
/// index and rebuilds anyway, so a FRESH line there is followed by a full
/// pipeline run by design — the forced scan is the only path that overrides the
/// verdict it just printed.
pub fn staleness_line(apg_root: &Path, current: &GitState, requested: Option<&[String]>) -> String {
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
    let rec = match db_recorded_scan(apg_root) {
        Some(rec) => state_str(Some(&rec.sha), rec.clean),
        None => state_str(None, false),
    };
    match freshness_differences_with(apg_root, current, requested) {
        Ok(()) => format!("Git state: recorded {rec} vs current {cur} → FRESH"),
        Err(differing) => format!(
            "Git state: recorded {rec} vs current {cur} → STALE (differing: {})",
            differing.join(", ")
        ),
    }
}
