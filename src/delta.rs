//! Win-B git delta + correctness-only full-scan fallbacks (phase-02 tasks 3
//! and 4).
//!
//! The delta is `git diff --name-status -M <recorded-commit>` for **committed**
//! changes (`rust.apg.delta`), PLUS the index/working-tree/untracked state
//! (`git status --porcelain` semantics). A path is keyed **checkout-relative**
//! with `/` separators, matching the shared manifest.
//!
//! Two correctness/version gates force a **full scan** rather than an
//! incremental one (task-4), never as a heuristic:
//!
//! 1. the recorded commit is not an ancestor of HEAD (a rebase/force-push/history
//!    rewrite makes a commit-range diff meaningless), or
//! 2. the global cache key has drifted (binary version, JSONL schema/format,
//!    ingestor projection rules, or the scan config).
//!
//! When either holds, [`plan`] returns `None` and the caller runs the full
//! pipeline — the correctness reference.
//!
//! Everything here is libgit2 (the git CLI is never shelled out to, R6).

// The phase-02 win-B surface: some accessors (e.g. `FullScanReason::tag`,
// `Plan::requires_full_scan`) are exercised by the delta unit tests and the
// phase-03 orchestration rather than by `cmd_scan` directly.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cache::{CacheKey, Manifest, ScanConfigKey};

/// The three status sets a git delta resolves to, plus the rename pairs. Paths
/// are checkout-relative (`/`-separated).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delta {
    pub added: BTreeSet<String>,
    pub modified: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    /// `(old, new)` checkout-relative pairs for renamed files.
    pub renamed: Vec<(String, String)>,
}

impl Delta {
    /// Every changed path (added ∪ modified ∪ removed ∪ both rename ends).
    pub fn changed(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .added
            .iter()
            .chain(&self.modified)
            .chain(&self.removed)
            .cloned()
            .collect();
        for (old, new) in &self.renamed {
            out.insert(old.clone());
            out.insert(new.clone());
        }
        out
    }

    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.modified.is_empty()
            && self.removed.is_empty()
            && self.renamed.is_empty()
    }
}

/// Why an incremental scan cannot be trusted and a full scan is required. These
/// are correctness/version gates, not heuristics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullScanReason {
    /// The scanned directory is not inside a git repo (no recorded state).
    NotAGitRepo,
    /// There is no prior scan record (no manifest / recorded commit).
    NoRecordedScan,
    /// HEAD is unborn or missing.
    NoHead,
    /// The recorded commit still exists but is not an ancestor of HEAD.
    NotAncestor { recorded: String, head: String },
    /// The global cache key drifted (binary/format/config).
    CacheKeyDrift { recorded: String, current: String },
    /// The previous export (`apg/.trans/graph.jsonl`) is absent, so the
    /// full code-FQN universe cannot be derived from it. Only raised when the
    /// scan would otherwise emission-filter (a non-empty target set): a
    /// worktree with no local export whose target set is empty emits every
    /// fact, so the full spool is the universe.
    NoPreviousExport,
    /// The delta could not be computed (a corrupt/unreadable store record).
    DeltaUnavailable,
}

impl FullScanReason {
    /// A short machine-readable tag for logging.
    pub fn tag(&self) -> &'static str {
        match self {
            FullScanReason::NotAGitRepo => "not-a-git-repo",
            FullScanReason::NoRecordedScan => "no-recorded-scan",
            FullScanReason::NoHead => "no-head",
            FullScanReason::NotAncestor { .. } => "recorded-not-ancestor",
            FullScanReason::CacheKeyDrift { .. } => "cache-key-drift",
            FullScanReason::NoPreviousExport => "no-previous-export",
            FullScanReason::DeltaUnavailable => "delta-unavailable",
        }
    }

    /// A human line for the scan log.
    pub fn describe(&self) -> String {
        match self {
            FullScanReason::NotAGitRepo => "not a git repo — full scan".to_string(),
            FullScanReason::NoRecordedScan => "no recorded scan/manifest — full scan".to_string(),
            FullScanReason::NoHead => "no HEAD commit — full scan".to_string(),
            FullScanReason::NotAncestor { recorded, head } => format!(
                "recorded commit {} is not an ancestor of HEAD {} (history rewrite) — full scan",
                &recorded[..recorded.len().min(12)],
                &head[..head.len().min(12)]
            ),
            FullScanReason::CacheKeyDrift { recorded, current } => {
                format!("cache key drifted ({recorded} → {current}) — full scan")
            }
            FullScanReason::NoPreviousExport => {
                "no previous export to derive the full code universe — full scan".to_string()
            }
            FullScanReason::DeltaUnavailable => {
                "previous scan record unavailable — full scan".to_string()
            }
        }
    }
}

/// The prior scan record persisted under the shared store root: the commit the
/// scan ran at, the global cache key it ran under, and its content manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRecord {
    pub sha: String,
    pub cache_key: CacheKey,
    pub manifest: Manifest,
    /// The content-identity key of the tree this scan ran on (win A,
    /// phase-01) — the same key the scan wrote into the DB `Scan` row and
    /// `graph.jsonl` line 1, and the same tree `manifest` describes. It is
    /// captured here so the NEXT scan can verify that this worktree's LOCAL
    /// `db.lbug` was built from the very tree the SHARED delta is derived from
    /// (see `crate::splice::seed_checked`, feedback-101). A missing key (a
    /// pre-hardening record, or one written before this field existed) makes
    /// the win-C splice ineligible — equivalence cannot be verified.
    #[serde(default)]
    pub content_key: Option<String>,
}

impl ScanRecord {
    fn path(store_root: &Path) -> std::path::PathBuf {
        store_root.join("scan.json")
    }

    /// Persists the record under the store root.
    pub fn save(&self, store_root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(store_root)?;
        std::fs::write(Self::path(store_root), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Loads a prior record, or `None` when none exists / it is unreadable.
    pub fn load(store_root: &Path) -> Option<ScanRecord> {
        let text = std::fs::read_to_string(Self::path(store_root)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// The in-memory scan verdict ([`scan_verdict`]): which scan path the current
/// checkout should take, decided WITHOUT git discovery or a filesystem walk.
/// [`full_scan_reason`] and [`plan`] are the git wrappers that gather the facts
/// this predicate consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanVerdict {
    /// The recorded scan is at exactly HEAD under the current cache key: the
    /// shared store's recorded facts describe this tree, so a scan can assemble
    /// from them with zero frontends (the warm-cache path) when the store is
    /// complete. The delta is not needed — the recorded manifest IS the state.
    WarmCache,
    /// The recorded scan is a usable ancestor of HEAD under the current key:
    /// the ordinary incremental path (delta + fact reuse) applies.
    Incremental,
    /// A full scan is required, with the reason.
    FullScan(FullScanReason),
}

impl ScanVerdict {
    /// `Some(reason)` when the verdict requires the full pipeline.
    pub fn full_scan_reason(&self) -> Option<&FullScanReason> {
        match self {
            ScanVerdict::FullScan(reason) => Some(reason),
            _ => None,
        }
    }

    /// True when the verdict requires the full pipeline.
    pub fn requires_full_scan(&self) -> bool {
        self.full_scan_reason().is_some()
    }

    /// True when the recorded scan is at exactly HEAD under the current key.
    pub fn is_warm_cache(&self) -> bool {
        matches!(self, ScanVerdict::WarmCache)
    }
}

/// The **pure** scan verdict (task-9): decided only from an in-memory scan
/// record, the current cache key, the current HEAD and the recorded commit's
/// ancestry — **no git discovery and no filesystem access**. This is the single
/// source of truth the git wrappers [`full_scan_reason`]/[`plan`] consume, so
/// every `FullScanReason` branch (and the recorded-HEAD warm-cache condition) is
/// unit-testable in memory.
///
/// `recorded_is_ancestor` is the git fact the wrapper gathers separately:
/// `Some(true)` means the recorded commit is HEAD or an ancestor of it;
/// `Some(false)`/`None` both mean it cannot be verified as an ancestor (a
/// garbage-collected commit or a history rewrite), which is a full-scan gate.
pub fn scan_verdict(
    in_repo: bool,
    record: Option<&ScanRecord>,
    current_key: &CacheKey,
    head: Option<&str>,
    recorded_is_ancestor: Option<bool>,
) -> ScanVerdict {
    if !in_repo {
        return ScanVerdict::FullScan(FullScanReason::NotAGitRepo);
    }
    let Some(head) = head else {
        return ScanVerdict::FullScan(FullScanReason::NoHead);
    };
    let Some(record) = record else {
        return ScanVerdict::FullScan(FullScanReason::NoRecordedScan);
    };

    // Cache-key drift: binary version / JSONL schema / projection / config.
    if !record.cache_key.matches(current_key) {
        return ScanVerdict::FullScan(FullScanReason::CacheKeyDrift {
            recorded: record.cache_key.token(),
            current: current_key.token(),
        });
    }

    // The recorded-HEAD warm-cache condition: the shared store's recorded facts
    // are exactly this tree's, so no delta and no frontends are needed.
    if record.sha == head {
        return ScanVerdict::WarmCache;
    }

    // A recorded sha that is not even a valid object id cannot be verified as an
    // ancestor, so it is a full-scan too — never an "incremental OK".
    if git2::Oid::from_str(&record.sha).is_err() {
        return ScanVerdict::FullScan(FullScanReason::DeltaUnavailable);
    }
    if recorded_is_ancestor == Some(true) {
        ScanVerdict::Incremental
    } else {
        // The recorded commit is gone (GC after a rewrite) or a non-ancestor:
        // the commit-range diff is meaningless.
        ScanVerdict::FullScan(FullScanReason::NotAncestor {
            recorded: record.sha.clone(),
            head: head.to_string(),
        })
    }
}

/// The in-memory recorded scan plus its verdict at a checkout — ONE git
/// discovery, no filesystem walk. The warm-cache seed path consumes the
/// returned record's manifest; [`full_scan_reason`]/[`plan`] are thin wrappers.
pub fn scan_state(
    apg_root: &Path,
    store_root: Option<&Path>,
    current_key: &CacheKey,
) -> (Option<ScanRecord>, ScanVerdict) {
    let repo = git2::Repository::discover(apg_root).ok();
    let head = repo.as_ref().and_then(head_oid);
    let head_str = head.map(|o| o.to_string());
    let record = store_root.and_then(ScanRecord::load);
    let ancestor = match (&repo, head, &record) {
        (Some(repo), Some(head), Some(record)) => git2::Oid::from_str(&record.sha)
            .ok()
            .and_then(|oid| repo.find_commit(oid).ok())
            .map(|c| head == c.id() || repo.graph_descendant_of(head, c.id()).unwrap_or(false)),
        _ => None,
    };
    let verdict = scan_verdict(
        repo.is_some(),
        record.as_ref(),
        current_key,
        head_str.as_deref(),
        ancestor,
    );
    (record, verdict)
}

/// The full-scan predicate (task-4): `Some(reason)` when an incremental scan is
/// unsafe and the full pipeline must run. Applied before any delta is computed.
///
/// `current_key` is the key this binary+config would compute now; a mismatch
/// with the recorded key is a full-scan gate. The decision itself is the pure
/// [`scan_verdict`]; this wrapper only gathers the git facts (discovery, HEAD,
/// recorded-commit ancestry).
pub fn full_scan_reason(
    apg_root: &Path,
    store_root: Option<&Path>,
    current_key: &CacheKey,
) -> Option<FullScanReason> {
    scan_state(apg_root, store_root, current_key)
        .1
        .full_scan_reason()
        .cloned()
}

/// Computes the git delta from the recorded commit to the current tree: the
/// committed `diff --name-status -M` PLUS the index/working-tree/untracked
/// status. `recorded_sha` must be an ancestor of HEAD (the caller checks
/// [`full_scan_reason`] first).
pub fn compute(apg_root: &Path, recorded_sha: &str) -> anyhow::Result<Delta> {
    let repo = git2::Repository::discover(apg_root)?;
    let mut delta = Delta::default();

    // 1. Committed changes: recorded tree -> HEAD tree, with rename detection.
    let recorded = repo
        .find_commit(git2::Oid::from_str(recorded_sha)?)
        .map_err(|e| anyhow::anyhow!("recorded commit {recorded_sha}: {e}"))?;
    let recorded_tree = recorded.tree()?;
    let head_commit = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| anyhow::anyhow!("HEAD: {e}"))?;
    let head_tree = head_commit.tree()?;
    let mut opts = git2::DiffOptions::new();
    let mut diff =
        repo.diff_tree_to_tree(Some(&recorded_tree), Some(&head_tree), Some(&mut opts))?;
    let mut find = git2::DiffFindOptions::new();
    find.renames(true);
    let _ = diff.find_similar(Some(&mut find));
    for d in diff.deltas() {
        apply_delta_status(
            &mut delta,
            d.status(),
            d.old_file().path(),
            d.new_file().path(),
        );
    }

    // 2. Index/working-tree/untracked state (`git status --porcelain`).
    let mut sopts = git2::StatusOptions::new();
    sopts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut sopts))?;
    for entry in statuses.iter() {
        let s = entry.status();
        let path = entry.path().unwrap_or_default().to_string();
        // A rename's old path comes from the head→index or index→workdir diff.
        let rename_old = entry
            .head_to_index()
            .or_else(|| entry.index_to_workdir())
            .and_then(|d| {
                if d.status() == git2::Delta::Renamed {
                    d.old_file().path().map(|p| p.to_string_lossy().to_string())
                } else {
                    None
                }
            });
        if s.intersects(git2::Status::INDEX_RENAMED | git2::Status::WT_RENAMED) {
            if let Some(old) = rename_old {
                delta.renamed.push((old, path.clone()));
            } else {
                delta.modified.insert(path.clone());
            }
        } else if s.intersects(git2::Status::INDEX_NEW | git2::Status::WT_NEW) {
            delta.added.insert(path.clone());
        } else if s.intersects(git2::Status::INDEX_DELETED | git2::Status::WT_DELETED) {
            delta.removed.insert(path.clone());
        } else if s.intersects(
            git2::Status::INDEX_MODIFIED
                | git2::Status::WT_MODIFIED
                | git2::Status::INDEX_TYPECHANGE
                | git2::Status::WT_TYPECHANGE
                | git2::Status::CONFLICTED,
        ) {
            delta.modified.insert(path.clone());
        }
    }

    // A path can appear in both the committed diff and the status (e.g. a file
    // deleted in a commit then re-added untracked). Normalise so a path is not
    // both removed and added: the working-tree status wins (it is the present
    // truth).
    for p in delta.added.clone() {
        delta.removed.remove(&p);
    }
    Ok(delta)
}

/// The incremental plan: the full-scan reason (if any) plus the git delta when
/// an incremental scan is permitted.
#[derive(Debug, Clone)]
pub struct Plan {
    pub delta: Option<Delta>,
    pub full_scan: Option<FullScanReason>,
    /// The recorded scan is at exactly HEAD under the current key (task-3): the
    /// shared store's recorded facts describe this tree, so the caller may
    /// assemble from them with zero frontends when the store is complete. The
    /// delta is `None` on this path — the recorded manifest IS the state.
    pub warm_cache: bool,
}

impl Plan {
    /// True when the caller must run the full pipeline.
    pub fn requires_full_scan(&self) -> bool {
        self.full_scan.is_some()
    }
}

/// The one entry point the scan orchestration uses: evaluate the fallbacks,
/// then compute the delta (or report the full-scan reason), or report the
/// recorded-HEAD warm-cache condition.
pub fn plan(apg_root: &Path, store_root: Option<&Path>, config: &ScanConfigKey) -> Plan {
    let current_key = CacheKey::compute(config);
    let (record, verdict) = scan_state(apg_root, store_root, &current_key);
    match verdict {
        ScanVerdict::FullScan(reason) => Plan {
            delta: None,
            full_scan: Some(reason),
            warm_cache: false,
        },
        ScanVerdict::WarmCache => Plan {
            delta: None,
            full_scan: None,
            warm_cache: true,
        },
        ScanVerdict::Incremental => {
            // The verdict guarantees a record; an unreadable sha/commit is a
            // full-scan (`compute` errors -> DeltaUnavailable).
            let sha = record.map(|r| r.sha).unwrap_or_default();
            match compute(apg_root, &sha) {
                Ok(delta) => Plan {
                    delta: Some(delta),
                    full_scan: None,
                    warm_cache: false,
                },
                Err(_) => Plan {
                    delta: None,
                    full_scan: Some(FullScanReason::DeltaUnavailable),
                    warm_cache: false,
                },
            }
        }
    }
}

fn apply_delta_status(
    delta: &mut Delta,
    status: git2::Delta,
    old: Option<&Path>,
    new: Option<&Path>,
) {
    let old_s = old.map(|p| p.to_string_lossy().replace('\\', "/"));
    let new_s = new.map(|p| p.to_string_lossy().replace('\\', "/"));
    match status {
        git2::Delta::Added | git2::Delta::Untracked => {
            if let Some(n) = new_s {
                delta.added.insert(n);
            }
        }
        git2::Delta::Deleted => {
            if let Some(o) = old_s {
                delta.removed.insert(o);
            }
        }
        git2::Delta::Modified | git2::Delta::Typechange => {
            if let Some(n) = new_s.clone().or(old_s.clone()) {
                delta.modified.insert(n);
            }
        }
        git2::Delta::Renamed => {
            if let (Some(o), Some(n)) = (old_s, new_s) {
                delta.renamed.push((o, n));
            }
        }
        git2::Delta::Copied => {
            if let Some(n) = new_s {
                delta.added.insert(n);
            }
        }
        _ => {}
    }
}

fn head_oid(repo: &git2::Repository) -> Option<git2::Oid> {
    repo.head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .map(|c| c.id())
}

#[cfg(test)]
mod tests;
