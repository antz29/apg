use std::io::BufRead;
use std::path::{Path, PathBuf};

use lbug::{Connection, Database, SystemConfig};

use crate::schema::Record;
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

pub(crate) fn graph_jsonl_path(apg_root: &Path) -> PathBuf {
    apg_root.join(specs::TRANS).join("graph.jsonl")
}

// ---------------------------------------------------------------------------
// git2 plumbing helpers
// ---------------------------------------------------------------------------

/// Discovers the repo enclosing `dir` (walking up, like the git CLI).
pub(crate) fn discover_repo(dir: &Path) -> anyhow::Result<git2::Repository> {
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

/// The recorded state of the scan that built the live DB, read from the DB's
/// **own** single `Scan` row (SCAN_HEAD) in `<apg_root>/.trans/db.lbug` — the
/// DB's account of itself, independent of the `graph.jsonl` export
/// [`recorded_scan`] reads. This is the SCAN_HEAD half of the freshness check:
/// the export and the DB must agree before the DB is reusable.
///
/// The DB is opened **read-only** and never modified. Every failure **fails
/// closed** and never panics: a missing, unopenable or locked DB; a DB with no
/// `Scan` row; an empty `git_sha` (a non-git scan); a `git_clean` that is
/// neither `"true"` nor `"false"`; or an empty `content_key` (a pre-hardening
/// DB). Parsing matches [`recorded_scan`]: `git_sha` empty means non-git,
/// `git_clean` is `"true"`/`"false"`, and an empty key is `None`.
pub fn db_recorded_scan(apg_root: &Path) -> Option<RecordedScan> {
    let db = Database::new(db_path(apg_root), SystemConfig::default().read_only(true)).ok()?;
    let conn = Connection::new(&db).ok()?;
    let (_, rows) = query_rows(
        &conn,
        "MATCH (s:Scan) RETURN s.git_sha AS git_sha, s.git_clean AS git_clean, s.content_key AS content_key",
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
    })
}

/// The content-identity freshness predicate (win A): the live DB is reusable
/// as-is iff it exists, the DB's **own** `Scan` row agrees with the
/// `graph.jsonl` recorded scan, AND the current tree's content identity matches
/// that recorded scan exactly — recorded HEAD sha, cleanliness and
/// content-identity key all equal the current values. mtime is never consulted:
/// a touch without a byte change stays fresh, a byte edit is stale.
///
/// The DB-existence precondition comes FIRST. The fast-path's action is "reuse
/// the existing DB", so with no `db.lbug` at all (or a `graph.jsonl` without
/// its DB) there is nothing to reuse → NOT fresh. A missing/pre-hardening
/// recorded key means freshness cannot be verified → NOT fresh.
///
/// **DB↔export agreement** ([`db_recorded_scan`] vs [`recorded_scan`]): the two
/// halves must name the same git state (sha, cleanliness and content key) — the
/// DB is reusable only when its own `Scan` row and the export cannot disagree.
/// A DB with no `Scan` row, or one that disagrees, is NOT fresh. Every failure
/// **fails closed** (never panics): a missing/unopenable/locked DB, a missing
/// export, or a mismatch all mean "not fresh".
///
/// **Authored/transient reconciliation**: the DB's own authored/transient
/// digest ([`crate::splice::seed::seed_authored_identity`]) must also equal the
/// on-disk tree's ([`crate::splice::seed::tree_authored_identity`]) — the
/// independent source of truth. This is the recovery safety net: a durability
/// commit that re-anchors the DB `Scan` row to the current state cannot mask an
/// authored row the DB dropped, because the DB digest is compared to the tree,
/// never to a value refreshed from the DB. An error computing either digest is
/// NOT fresh (fail closed).
///
/// A session socket that is present but fails a connect/`Ping` is also NOT
/// fresh: an unclean session exit leaves the socket behind while the derived
/// DB reflects only the last saved state, so a phantom projection exists and
/// the on-disk DB cannot be trusted until a rebuild. A LIVE session (one that
/// answers `Ping`) owns the DB directly, so opening it here would contend with
/// the session and see only its last saved buffer: the DB reads
/// ([`db_recorded_scan`] and the authored/transient reconciliation) are
/// SHORT-CIRCUITED and only the `recorded_scan`/git half is evaluated. A live
/// session therefore never makes the on-disk fast path unfresh.
///
/// This is deliberately not `!is_stale`: `is_stale` is N/A (false) when there
/// is no DB or the dir is not a git repo, so `is_stale != !is_fresh` there.
pub fn is_fresh(apg_root: &Path) -> bool {
    if !db_path(apg_root).exists() {
        return false;
    }
    // A present socket that fails a connect/`Ping` is an unclean exit (see the
    // doc above) — NOT fresh. A LIVE session is handled by the short-circuit
    // below. An absent socket is the ordinary case.
    let socket = crate::session::socket_path(apg_root);
    let live_session = socket.exists() && crate::session::live_session_at(&socket);
    if socket.exists() && !live_session {
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
    // While a live session owns `db.lbug` it holds the DB exclusively; opening
    // it here would contend with the session and observe only its last saved
    // buffer. Short-circuit BOTH DB reads and evaluate the recorded_scan/git
    // half alone, so a live session never makes the on-disk fast path unfresh.
    if !live_session {
        // The DB's own `Scan` row must exist and agree with the export exactly:
        // either half alone is not enough to reuse the DB (the DB and graph.jsonl
        // are refreshed together at every durability point, so they agree by
        // construction on a soundly-built DB).
        let Some(db_rec) = db_recorded_scan(apg_root) else {
            return false;
        };
        if db_rec.sha != rec.sha
            || db_rec.clean != rec.clean
            || db_rec.content_key != rec.content_key
        {
            return false;
        }
        // Recovery safety net: the DB's ACTUAL authored/transient digest must
        // equal the on-disk tree's (the independent source of truth), so a
        // re-anchored `Scan` row cannot mask an authored row the DB dropped.
        // Any error fails closed.
        let Ok(db_authored) = crate::splice::seed::seed_authored_identity(&db_path(apg_root))
        else {
            return false;
        };
        let Ok(tree_authored) = crate::splice::seed::tree_authored_identity(apg_root) else {
            return false;
        };
        if db_authored != tree_authored {
            return false;
        }
    }
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
