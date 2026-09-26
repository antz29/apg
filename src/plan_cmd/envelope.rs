//! The plan envelope: the layout root, the transient plan store's read/write
//! (`.trans/plans/<project>.jsonl`), and the plan-record identity helper.

use std::path::{Path, PathBuf};

use crate::artifacts;
use crate::schema::Record;
use crate::specs;

/// The layout root of the checkout the process runs in (walk-up discovery).
pub(crate) fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

pub fn load_plan(apg_root: &Path, project: &str) -> anyhow::Result<Vec<Record>> {
    let path = specs::plan_jsonl_path(apg_root, project);
    if !path.exists() {
        anyhow::bail!("no plan for project `{project}` — run `apg plan add {project}` first");
    }
    specs::read_jsonl(&path)
}

pub fn write_through(apg_root: &Path, project: &str, records: &[Record]) -> anyhow::Result<()> {
    artifacts::write_jsonl_and_reingest(
        apg_root,
        &specs::plan_jsonl_path(apg_root, project),
        project,
        records,
    )
}

/// The project a plan record set belongs to — the `{project}` segment of the
/// Plan record's `{project}/plan` FQN. Names the follow-up `update`/`rm`
/// commands in `plan_add_planned_at`'s refusal (that core takes no separate
/// `project` argument).
pub(crate) fn plan_project(records: &[Record]) -> String {
    records
        .iter()
        .find_map(|r| match r {
            Record::Plan { fqn, .. } => fqn.strip_suffix("/plan").map(str::to_string),
            _ => None,
        })
        .unwrap_or_default()
}
