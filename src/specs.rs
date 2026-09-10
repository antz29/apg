//! Spec/plan serialization (SPEC R4/R5) and the `apg scan` re-ingest of the
//! transient `apg/.trans/` legs.
//!
//! Layout (a committed `apg/` dir at the repo root, gitignored `apg/.trans/`):
//! - `apg/layers/` — the durable node-file store (SPEC §4.1), ingested by
//!   `layers::ingest_tree`.
//! - `apg/.trans/plans/<project>.jsonl` — the transient plan store
//!   (gitignored); also the tier dir of plan nodes.
//! - `apg/.trans/<tier>/<project>.jsonl` — the five feedback tier mirrors
//!   (requirements/domain/solution/implementation/global): review feedback
//!   sits in the tier dir of its attached node (SPEC §5).
//!
//! All records are the unified-JSONL `Record` enum: canonical FQNs, no opaque
//! ids. `apg scan` re-ingests the layers tree plus the transient plan leg and
//! the feedback mirrors into the DB.

use std::path::{Path, PathBuf};

use crate::layers::Layer;
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

/// Path of a plan's JSONL under the gitignored `apg/.trans/plans/`.
#[allow(dead_code)]
pub fn plan_jsonl_path(apg_root: &Path, project: &str) -> PathBuf {
    apg_root
        .join(TRANS)
        .join("plans")
        .join(format!("{project}.jsonl"))
}

/// The transient JSONL a project's feedback on `tier` nodes lands in (SPEC
/// §5): `.trans/plans/<project>.jsonl` for plan nodes (the plan store
/// itself — feedback on plan-phase/task targets co-locates with the plan
/// records), `.trans/<tier>/<project>.jsonl` for the five tier mirrors. Both
/// halves of a feedback relationship — the `Feedback` record AND its
/// `Reviews` edge — live in the file (committed node files never hold
/// transient references, §4.1).
pub fn transient_feedback_path(apg_root: &Path, project: &str, tier: Layer) -> PathBuf {
    apg_root
        .join(TRANS)
        .join(tier.layer_dir())
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

/// Every `.trans/<tier>/*.jsonl` feedback-mirror file (sorted), or `[]` when
/// absent — the five tier mirrors (requirements/domain/solution/
/// implementation/global) where review feedback on durable/code nodes lands
/// (SPEC §5: feedback sits in the tier dir of its attached node). The plans
/// dir is the plan store and is owned by [`plan_files`]. A scan chains these
/// after the plan leg so `.trans` Feedback + Reviews edges pair against the
/// durable nodes in the branch DB.
pub fn trans_mirror_files(apg_root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for layer in Layer::ALL {
        if matches!(layer, Layer::Plans) {
            continue; // `.trans/plans/` is the plan store — plan_files owns it
        }
        out.extend(jsonl_files(&apg_root.join(TRANS).join(layer.layer_dir())));
    }
    out.sort();
    out
}

/// The six transient JSONLs a project's review state can live in, in layer
/// order: the plan store plus the five feedback tier mirrors (`.trans/plans/
/// <project>.jsonl` and `.trans/<tier>/<project>.jsonl`). All six share the
/// project's single `<project>/feedback-<n>` namespace, so numbering and
/// status updates scan every file, and a re-ingest after ANY of them merges
/// ALL of them (a write-through detaches every `<project>/…` node first).
pub fn project_transient_files(apg_root: &Path, project: &str) -> Vec<PathBuf> {
    let mut out = vec![plan_jsonl_path(apg_root, project)];
    for layer in Layer::ALL {
        if matches!(layer, Layer::Plans) {
            continue;
        }
        out.push(
            apg_root
                .join(TRANS)
                .join(layer.layer_dir())
                .join(format!("{project}.jsonl")),
        );
    }
    out
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
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
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

    #[test]
    fn feedback_mirrors_discovered_and_paths_are_per_tier_per_project() {
        let dir = std::env::temp_dir().join(format!("apg-specs-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(TRANS).join("plans")).unwrap();
        std::fs::create_dir_all(dir.join(TRANS).join("requirements")).unwrap();
        std::fs::create_dir_all(dir.join(TRANS).join("implementation")).unwrap();
        std::fs::create_dir_all(dir.join(TRANS).join("global")).unwrap();

        // Feedback mirrors are per-project JSONLs in the tier dir of the
        // attached node (SPEC §5); a second project shares the dirs.
        write_jsonl(
            &dir.join(TRANS).join("requirements").join("foo.jsonl"),
            &[Record::Feedback {
                fqn: "foo/feedback-1".to_string(),
                body: "x".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            }],
        )
        .unwrap();
        write_jsonl(
            &dir.join(TRANS).join("implementation").join("foo.jsonl"),
            &[Record::Reviews {
                from: "foo/feedback-2".to_string(),
                to: "fixture.mod.Store".to_string(),
            }],
        )
        .unwrap();
        write_jsonl(
            &dir.join(TRANS).join("global").join("bar.jsonl"),
            &[Record::Feedback {
                fqn: "bar/feedback-1".to_string(),
                body: "y".to_string(),
                status: "open".to_string(),
                disposition: String::new(),
            }],
        )
        .unwrap();

        // trans_mirror_files finds every tier mirror, never the plans store.
        let mirrors = trans_mirror_files(&dir);
        let got: Vec<String> = mirrors
            .iter()
            .map(|p| {
                p.strip_prefix(dir.join(TRANS))
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect();
        assert_eq!(
            got,
            vec![
                "global/bar.jsonl",
                "implementation/foo.jsonl",
                "requirements/foo.jsonl"
            ]
        );

        // transient_feedback_path: plans tier == the plan store, the other
        // tiers mirror under their layer dir.
        assert_eq!(
            transient_feedback_path(&dir, "foo", Layer::Plans),
            plans_jsonl(&dir, "foo")
        );
        assert_eq!(
            transient_feedback_path(&dir, "foo", Layer::Requirements),
            dir.join(TRANS).join("requirements").join("foo.jsonl")
        );

        // project_transient_files: the plan store + the five tier mirrors for
        // one project, in layer order.
        let files = project_transient_files(&dir, "foo");
        assert_eq!(files.len(), 6);
        assert_eq!(files[0], plans_jsonl(&dir, "foo"));
        assert_eq!(
            files[1],
            dir.join(TRANS).join("requirements").join("foo.jsonl")
        );
        assert_eq!(files[2], dir.join(TRANS).join("domain").join("foo.jsonl"));
        assert_eq!(files[3], dir.join(TRANS).join("solution").join("foo.jsonl"));
        assert_eq!(
            files[4],
            dir.join(TRANS).join("implementation").join("foo.jsonl")
        );
        assert_eq!(files[5], dir.join(TRANS).join("global").join("foo.jsonl"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn plans_jsonl(dir: &Path, project: &str) -> PathBuf {
        dir.join(TRANS)
            .join("plans")
            .join(format!("{project}.jsonl"))
    }
}
