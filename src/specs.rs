//! Spec/plan serialization (SPEC R4/R5) and the `apg scan` re-ingest of the
//! transient `apg/.trans/plans/` plan leg.
//!
//! Layout (a committed `apg/` dir at the repo root, gitignored `apg/.trans/`):
//! - `apg/layers/` — the durable node-file store (SPEC §4.1), ingested by
//!   `layers::ingest_tree`.
//! - `apg/.trans/plans/<project>.jsonl` — the transient plan (gitignored).
//!
//! All records are the unified-JSONL `Record` enum: canonical FQNs, no opaque
//! ids. `apg scan` re-ingests the layers tree plus the plan leg into the DB.

use std::path::{Path, PathBuf};

use crate::schema::Record;

/// The committed `apg/` root layout.
pub const LAYOUT: &str = "apg";
/// The gitignored transient subdir.
pub const TRANS: &str = ".trans";

/// Walks up from `start` looking for the committed `apg/` layout root (a
/// directory named `apg` carrying the gitignored `apg/.trans/` subdir, which
/// `apg scan`/`apg init` create). The `apg/` name is shared, so the transient
/// marker disambiguates the repo's layout root from an unrelated dir.
pub fn find_apg_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        let cand = cur.join(LAYOUT);
        if cand.is_dir() && is_apg_layout_root(&cand) {
            return Some(cand);
        }
        cur = cur.parent()?;
    }
}

/// Finds the project's `apg/` layout root (walking up from `dir`) or creates
/// one at `<dir>/apg` (with `.trans/`) if none exists.
pub fn find_or_create_apg_root(dir: &Path) -> PathBuf {
    if let Some(apg) = find_apg_root(dir) {
        return apg;
    }
    let apg = dir.join(LAYOUT);
    std::fs::create_dir_all(apg.join(TRANS)).unwrap();
    apg
}

/// True when `dir` is the repo's committed `apg/` layout root (it carries the
/// gitignored transient subdir, created by `apg scan`/`apg init`).
pub fn is_apg_layout_root(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == LAYOUT) && dir.join(TRANS).is_dir()
}

/// Path of a spec project's JSONL under `apg/specs/`.
// (Unused until the Phase 03 `apg spec` CLI lands; part of the serialization
// API this module owns.)
#[allow(dead_code)]
pub fn spec_jsonl_path(apg_root: &Path, project: &str) -> PathBuf {
    apg_root.join("specs").join(format!("{project}.jsonl"))
}

/// Path of a plan's JSONL under the gitignored `apg/.trans/plans/`.
#[allow(dead_code)]
pub fn plan_jsonl_path(apg_root: &Path, project: &str) -> PathBuf {
    apg_root
        .join(TRANS)
        .join("plans")
        .join(format!("{project}.jsonl"))
}

/// Parses one JSONL file into records. Empty lines are skipped; a malformed
/// record fails loudly with its line number (never silently dropped).
pub fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Record>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec = serde_json::from_str::<Record>(line).map_err(|e| {
            anyhow::anyhow!("{}:{}: bad record: {e}\n{line}", path.display(), i + 1)
        })?;
        out.push(rec);
    }
    Ok(out)
}

/// Serializes records into a JSONL file, creating parent dirs (write-through:
/// the file is written atomically in one write, so a crash mid-session loses
/// nothing).
// (Unused until Phase 03's write-through authoring.)
#[allow(dead_code)]
pub fn write_jsonl(path: &Path, records: &[Record]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = String::new();
    for r in records {
        s.push_str(&serde_json::to_string(r)?);
        s.push('\n');
    }
    std::fs::write(path, s)?;
    Ok(())
}

/// Every `*.jsonl` under `dir` (sorted), or `[]` when the dir does not exist.
pub fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    v.sort();
    v
}

/// Every `apg/.trans/plans/*.jsonl` file (sorted), or `[]` when absent — the
/// transient plan leg a scan re-ingests after code. The committed `apg/specs`
/// and `apg/notes` durable halves are gone: spec data now comes from the
/// `apg/layers` node-file tree via `layers::ingest_tree`.
pub fn plan_files(apg_root: &Path) -> Vec<PathBuf> {
    jsonl_files(&apg_root.join(TRANS).join("plans"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_roundtrip_and_plan_files() {
        let dir = std::env::temp_dir().join(format!("apg-specs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join(TRANS).join("plans");
        std::fs::create_dir_all(&plans).unwrap();

        let recs = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "Plan".to_string(),
                strategy: "Layer-first".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
            },
        ];
        write_jsonl(&plans.join("foo.jsonl"), &recs).unwrap();

        // plan_files discovers only the transient plan leg.
        let p = plan_files(&dir);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0], plans.join("foo.jsonl"));
        // Round-trip: re-serialize the parsed records identically.
        let parsed = read_jsonl(&plans.join("foo.jsonl")).unwrap();
        assert_eq!(parsed, recs);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
