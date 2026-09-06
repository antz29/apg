//! Git-state capture for `apg scan` and the refuse-on-stale mutation gate
//! (agent-loop hardening).
//!
//! Every scan records the git state it ran under — the repo HEAD sha plus
//! whether `git status --porcelain` was empty — as a `scan_meta` control record
//! on line 1 of `apg/.trans/graph.jsonl` and as the DB's `Scan` node. A later
//! spec/plan/review mutation that would re-ingest into a stale DB is refused
//! *before* any JSONL write: if the tree moved on (new sha, or the same sha
//! now dirty), the code graph no longer matches what the authoring agents are
//! reasoning about, so writing more `future/…` state against it would be
//! building on sand.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Runs `git` in `dir`; returns trimmed stdout, or `None` when git cannot run
/// (not a repo, no git binary, or a failing command).
fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Captures the current git state of the repo containing `dir`: `rev-parse
/// HEAD` for the sha and `git status --porcelain` (empty ⇒ clean). Both
/// commands resolve the enclosing repo by walking up, so passing a
/// subdirectory (a scanned module dir, or the `apg/` layout root) observes the
/// whole repo's state.
pub fn git_state(dir: &Path) -> GitState {
    match git_stdout(dir, &["rev-parse", "HEAD"]) {
        Some(sha) => {
            let clean = git_stdout(dir, &["status", "--porcelain"])
                .map(|s| s.is_empty())
                .unwrap_or(false);
            GitState {
                sha: Some(sha),
                clean,
            }
        }
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
    use crate::graph::{Graph, Node, NodeKind};
    use crate::load;

    fn run_git(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {}: {e}", dir.display()))
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = run_git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = run_git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A temp dir with a fresh git repo (one commit, `apg/.trans/` gitignored
    /// so later writes under it stay clean) and an `apg/.trans/db.lbug` marker.
    /// Returns `(apg_root, repo_root, head_sha)`. `is_stale` only checks DB
    /// existence — it never opens the file — so an empty marker is enough for
    /// the predicate tests (the write-through tests in artifacts.rs build a
    /// real DB over the same layout).
    fn git_fixture(tag: &str) -> (PathBuf, PathBuf, String) {
        let root = std::env::temp_dir().join(format!("apg-git-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-q"]);
        git_ok(&root, &["config", "user.email", "apg-test@example.com"]);
        git_ok(&root, &["config", "user.name", "apg test"]);
        std::fs::write(root.join(".gitignore"), "apg/.trans/\n").unwrap();
        git_ok(&root, &["add", ".gitignore"]);
        git_ok(&root, &["commit", "-q", "-m", "init"]);
        let sha = git_out(&root, &["rev-parse", "HEAD"]);
        let apg = root.join("apg");
        std::fs::create_dir_all(apg.join(specs::TRANS)).unwrap();
        std::fs::write(apg.join(specs::TRANS).join("db.lbug"), "").unwrap();
        (apg, root, sha)
    }

    /// Writes a graph.jsonl whose line 1 is the scan_meta record (via the real
    /// export writer). `sha = None` models a non-git scan (git fields absent).
    fn write_scan_meta(apg_root: &Path, sha: Option<&str>, clean: bool, at: &str) {
        let mut g = Graph::default();
        g.nodes.insert(
            crate::schema::SCAN_HEAD.to_string(),
            Node {
                kind: NodeKind::Scan,
                git_sha: sha.map(str::to_string),
                git_clean: sha.map(|_| clean),
                scanned_at: Some(at.to_string()),
                ..Node::default()
            },
        );
        load::write_graph_jsonl(&g, &apg_root.join(specs::TRANS).join("graph.jsonl")).unwrap();
    }

    /// A non-git temp dir carrying an `apg/.trans/db.lbug` marker.
    fn non_git_fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("apg-nongit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apg").join(specs::TRANS)).unwrap();
        std::fs::write(root.join("apg").join(specs::TRANS).join("db.lbug"), "").unwrap();
        root.join("apg")
    }

    #[test]
    fn same_sha_and_clean_is_fresh() {
        let (apg, _, sha) = git_fixture("fresh");
        write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
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
        let _ = std::fs::remove_dir_all(apg.parent().unwrap());
    }

    #[test]
    fn differing_sha_is_stale() {
        let (apg, root, sha0) = git_fixture("sha");
        write_scan_meta(&apg, Some(&sha0), true, "2026-09-07T00:00:00Z");
        git_ok(&root, &["commit", "-q", "--allow-empty", "-m", "second"]);
        let sha1 = git_out(&root, &["rev-parse", "HEAD"]);
        assert_ne!(sha0, sha1);
        assert!(is_stale(&apg), "a new commit must make the DB stale");
        let msg = refusal_message(&apg).unwrap();
        assert!(
            msg.contains(&format!("recorded {sha0}@true, current {sha1}@true")),
            "refusal message: {msg}"
        );
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");
        assert!(staleness_line(&apg, &git_state(&apg)).contains("→ STALE"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dirty_tree_at_same_sha_is_stale() {
        let (apg, root, sha) = git_fixture("dirty");
        write_scan_meta(&apg, Some(&sha), true, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        // Same sha, but the tree moved: the scan did not see this content.
        let mut gi = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        gi.push_str("# dirty after scan\n");
        std::fs::write(root.join(".gitignore"), gi).unwrap();
        assert_eq!(git_state(&apg).clean, false);
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
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recorded_dirty_matching_dirty_tree_is_fresh() {
        // A scan over an already-dirty tree records clean=false; the same dirty
        // tree at the same sha is (by design) indistinguishable from it, so the
        // DB stays fresh until the tree changes again.
        let (apg, root, sha) = git_fixture("dirtyrec");
        std::fs::write(root.join("dirty.txt"), "x").unwrap();
        assert_eq!(git_state(&apg).clean, false);
        write_scan_meta(&apg, Some(&sha), false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_recorded_scan_meta_is_stale() {
        let (apg, root, _) = git_fixture("norec");
        // DB exists in a git repo, but no graph.jsonl / no scan_meta record:
        // freshness cannot be verified.
        assert!(is_stale(&apg));
        let msg = refusal_message(&apg).unwrap();
        assert!(msg.contains("recorded -@-"), "{msg}");
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_git_repo_is_never_stale() {
        let apg = non_git_fixture("nongit");
        // A DB whose scan was not in a git repo, plus no git repo now → N/A.
        write_scan_meta(&apg, None, false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        assert!(refusal_message(&apg).is_none());
        assert_eq!(
            staleness_line(&apg, &git_state(&apg)),
            "Git state: N/A (not a git repo)"
        );
        let _ = std::fs::remove_dir_all(apg.parent().unwrap());
    }

    #[test]
    fn no_db_is_never_stale() {
        let (apg, root, sha) = git_fixture("nodb");
        std::fs::remove_file(apg.join(specs::TRANS).join("db.lbug")).unwrap();
        // Even a recorded mismatch is irrelevant when there is no DB to guard.
        write_scan_meta(&apg, Some("stale-sha"), false, "2026-09-07T00:00:00Z");
        assert!(!is_stale(&apg));
        assert_eq!(
            staleness_line(&apg, &git_state(&apg)),
            format!("Git state: no scan yet (current {sha}@true)")
        );
        let _ = std::fs::remove_dir_all(&root);
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
}
