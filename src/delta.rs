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

/// The full-scan predicate (task-4): `Some(reason)` when an incremental scan is
/// unsafe and the full pipeline must run. Applied before any delta is computed.
///
/// `current_key` is the key this binary+config would compute now; a mismatch
/// with the recorded key is a full-scan gate.
pub fn full_scan_reason(
    apg_root: &Path,
    store_root: Option<&Path>,
    current_key: &CacheKey,
) -> Option<FullScanReason> {
    let Ok(repo) = git2::Repository::discover(apg_root) else {
        return Some(FullScanReason::NotAGitRepo);
    };
    let Some(head) = head_oid(&repo) else {
        return Some(FullScanReason::NoHead);
    };
    let Some(store_root) = store_root else {
        return Some(FullScanReason::NoRecordedScan);
    };
    let Some(record) = ScanRecord::load(store_root) else {
        return Some(FullScanReason::NoRecordedScan);
    };

    // Cache-key drift: binary version / JSONL schema / projection / config.
    if !record.cache_key.matches(current_key) {
        return Some(FullScanReason::CacheKeyDrift {
            recorded: record.cache_key.token(),
            current: current_key.token(),
        });
    }

    // The recorded commit must still be an ancestor of HEAD; a history rewrite
    // makes the commit-range diff meaningless. A recorded sha that is not even
    // a valid object id cannot be verified as an ancestor, so it is a
    // full-scan too — never an "incremental OK" (`Option::?` would silently
    // return `None` here).
    let Ok(recorded_oid) = git2::Oid::from_str(&record.sha) else {
        return Some(FullScanReason::DeltaUnavailable);
    };
    let Ok(recorded_commit) = repo.find_commit(recorded_oid) else {
        // The recorded commit is gone (GC after a rewrite) — treat as a
        // non-ancestor full-scan.
        return Some(FullScanReason::NotAncestor {
            recorded: record.sha.clone(),
            head: head.to_string(),
        });
    };
    if !(head == recorded_commit.id()
        || repo
            .graph_descendant_of(head, recorded_commit.id())
            .unwrap_or(false))
    {
        return Some(FullScanReason::NotAncestor {
            recorded: record.sha.clone(),
            head: head.to_string(),
        });
    }
    None
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
}

impl Plan {
    /// True when the caller must run the full pipeline.
    pub fn requires_full_scan(&self) -> bool {
        self.full_scan.is_some()
    }
}

/// The one entry point the scan orchestration uses: evaluate the fallbacks,
/// then compute the delta (or report the full-scan reason).
pub fn plan(apg_root: &Path, store_root: Option<&Path>, config: &ScanConfigKey) -> Plan {
    let current_key = CacheKey::compute(config);
    let reason = full_scan_reason(apg_root, store_root, &current_key);
    if let Some(reason) = reason {
        return Plan {
            delta: None,
            full_scan: Some(reason),
        };
    }
    let Some(store_root) = store_root else {
        return Plan {
            delta: None,
            full_scan: Some(FullScanReason::NoRecordedScan),
        };
    };
    let Some(record) = ScanRecord::load(store_root) else {
        return Plan {
            delta: None,
            full_scan: Some(FullScanReason::NoRecordedScan),
        };
    };
    match compute(apg_root, &record.sha) {
        Ok(delta) => Plan {
            delta: Some(delta),
            full_scan: None,
        },
        Err(_) => Plan {
            delta: None,
            full_scan: Some(FullScanReason::DeltaUnavailable),
        },
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
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> (PathBuf, git2::Repository) {
        let dir = std::env::temp_dir().join(format!("apg-delta-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("refs/heads/main");
        let repo = git2::Repository::init_opts(&dir, &opts).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "apg test").unwrap();
            cfg.set_str("user.email", "t@example.com").unwrap();
        }
        (dir, repo)
    }

    fn commit(repo: &git2::Repository, msg: &str) -> String {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().ok().map(|h| h.peel_to_commit().unwrap());
        let parents: Vec<&git2::Commit> = head.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, parents.as_slice())
            .unwrap()
            .to_string()
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn delta_parses_committed_add_modify_delete_and_rename() {
        let (dir, repo) = scratch("parse");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        write(&dir, "b.go", "package b\n");
        write(&dir, "gone.go", "package g\n");
        let base = commit(&repo, "base");

        // Committed: modify a, delete gone, add c, rename b -> renamed.
        write(&dir, "a.go", "package a\n\nvar X = 1\n");
        std::fs::remove_file(dir.join("gone.go")).unwrap();
        write(&dir, "c.go", "package c\n");
        // A real rename: b.go -> renamed.go (same bytes so find_similar sees it).
        let b = std::fs::read(dir.join("b.go")).unwrap();
        std::fs::remove_file(dir.join("b.go")).unwrap();
        std::fs::write(dir.join("renamed.go"), b).unwrap();
        commit(&repo, "change");

        let d = compute(&dir, &base).unwrap();
        assert!(d.modified.contains("a.go"), "{d:?}");
        assert!(d.removed.contains("gone.go"), "{d:?}");
        assert!(d.added.contains("c.go"), "{d:?}");
        assert!(
            d.renamed
                .iter()
                .any(|(o, n)| o == "b.go" && n == "renamed.go"),
            "{d:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delta_parses_untracked_and_working_tree_state() {
        let (dir, repo) = scratch("status");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        let base = commit(&repo, "base");

        // Uncommitted: modify a (working tree), add untracked new.go, delete
        // b.go (tracked), all at the same sha.
        write(&dir, "b.go", "package b\n");
        commit(&repo, "add b");
        let base2 = commit(&repo, "noop");
        let _ = base;
        write(&dir, "a.go", "package a\n\nvar Y = 2\n");
        write(&dir, "new.go", "package new\n");
        std::fs::remove_file(dir.join("b.go")).unwrap();

        let d = compute(&dir, &base2).unwrap();
        assert!(d.modified.contains("a.go"), "{d:?}");
        assert!(d.added.contains("new.go"), "{d:?}");
        assert!(d.removed.contains("b.go"), "{d:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_scan_when_recorded_commit_not_an_ancestor() {
        let (dir, repo) = scratch("nonancestor");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        let base = commit(&repo, "base");
        write(&dir, "a.go", "package a\n\nvar X = 1\n");
        let tip = commit(&repo, "tip");

        let store = dir.join("store");
        let key = CacheKey::compute(&ScanConfigKey::default());
        ScanRecord {
            sha: tip.clone(),
            cache_key: key.clone(),
            manifest: Manifest::default(),
        }
        .save(&store)
        .unwrap();
        // Ancestor ⇒ no full-scan reason, the delta is computed.
        assert_eq!(full_scan_reason(&dir, Some(&store), &key), None);
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(!p.requires_full_scan(), "{p:?}");
        assert!(p.delta.is_some());

        // Rewrite history: reset main back to `base` (the recorded `tip`
        // commit is no longer an ancestor) ⇒ full scan.
        repo.reset(
            &repo
                .find_object(git2::Oid::from_str(&base).unwrap(), None)
                .unwrap(),
            git2::ResetType::Hard,
            None,
        )
        .unwrap();
        let reason = full_scan_reason(&dir, Some(&store), &key);
        assert!(
            matches!(reason, Some(FullScanReason::NotAncestor { .. })),
            "{reason:?}"
        );
        // The plan refuses the incremental path (correct full-scan result).
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(p.requires_full_scan());
        assert!(p.delta.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_recorded_sha_forces_a_full_scan() {
        // A recorded sha that is not a valid object id cannot be verified as an
        // ancestor of HEAD; the fallback must fire (a correct full-scan), never
        // fall through to the incremental path.
        let (dir, repo) = scratch("badsha");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        commit(&repo, "base");
        let key = CacheKey::compute(&ScanConfigKey::default());
        let store = dir.join("store");
        ScanRecord {
            sha: "not-a-sha".to_string(),
            cache_key: key.clone(),
            manifest: Manifest::default(),
        }
        .save(&store)
        .unwrap();
        let reason = full_scan_reason(&dir, Some(&store), &key);
        assert!(reason.is_some(), "a malformed recorded sha must full-scan");
        let p = plan(&dir, Some(&store), &ScanConfigKey::default());
        assert!(p.requires_full_scan());
        assert!(p.delta.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_scan_on_cache_key_drift() {
        let (dir, repo) = scratch("drift");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        let base = commit(&repo, "base");
        let _ = repo;

        let store = dir.join("store");
        let recorded = CacheKey::compute(&ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        });
        ScanRecord {
            sha: base,
            cache_key: recorded,
            manifest: Manifest::default(),
        }
        .save(&store)
        .unwrap();

        // Same config ⇒ incremental allowed.
        let same = ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        };
        assert_eq!(
            full_scan_reason(&dir, Some(&store), &CacheKey::compute(&same)),
            None
        );

        // Config drift (an added exclude) ⇒ cache-key drift ⇒ full scan.
        let drifted = ScanConfigKey {
            languages: vec!["go".into()],
            excludes: vec!["vendor".into()],
            ..Default::default()
        };
        let reason = full_scan_reason(&dir, Some(&store), &CacheKey::compute(&drifted));
        assert!(
            matches!(reason, Some(FullScanReason::CacheKeyDrift { .. })),
            "{reason:?}"
        );
        let p = plan(&dir, Some(&store), &drifted);
        assert!(p.requires_full_scan());
        assert!(p.delta.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_recorded_scan_forces_full_scan() {
        let (dir, repo) = scratch("norecord");
        write(&dir, ".gitignore", "apg/.trans/\n");
        write(&dir, "a.go", "package a\n");
        commit(&repo, "base");
        let key = CacheKey::compute(&ScanConfigKey::default());
        let reason = full_scan_reason(&dir, None, &key);
        assert!(matches!(reason, Some(FullScanReason::NoRecordedScan)));
        // Even with a store path but no record on disk, the same verdict holds.
        let store = dir.join("store");
        let reason = full_scan_reason(&dir, Some(&store), &key);
        assert!(matches!(reason, Some(FullScanReason::NoRecordedScan)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
