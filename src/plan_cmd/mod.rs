//! `apg plan` — the phased execution plan that bridges the spec to present
//! code (PHASE_03 execution + apply model). The plan is transient by design
//! (R22): it lives in the gitignored `apg/.trans/plans/<project>.jsonl` and is
//! branch-local. The plan-writer authors the tier-4 additions as **planned
//! Implementation nodes** (`apg plan add <project> planned <kind> <fqn>` —
//! Module/File/Struct/Function marked `status: planned` at the FQN where the
//! code will land, GraphModel-SPEC.md); a task declares how it touches the
//! Implementation tier through its **verb** (`creates`/`modifies`/`deletes`/
//! `renames`/`moves`, SPEC §5) with the target FQN(s) recorded on the task
//! itself. Nothing advances automatically with `plan done`/`plan complete` —
//! those are assertion + milestone only; a branch scan **replaces realized
//! planned nodes**. The plan survives until the apply act, whose coherence gate (every
//! planned node realized, all feedback resolved, derived solution coverage
//! holds — SPEC §5: every solution node reached from a satisfied requirement,
//! plus every solution node added on this branch, has its `implemented-by` FQN
//! touched by a plan task) precedes the merge + rebuild of `main`'s graph
//! (PlanCompletion-SPEC.md).
//!
//! The cohesive groups live in submodules — the plan envelope ([`envelope`]),
//! the add/update/rm authoring surface ([`authoring`]), the status/milestone
//! commands ([`status`]), the coherence gate ([`verify`]), and the markdown
//! renderer ([`render`]) — all re-exported here so `crate::plan_cmd::<name>`
//! keeps resolving.

pub mod authoring;
pub mod envelope;
pub mod render;
pub mod status;
pub mod verify;

pub use authoring::*;
pub use envelope::*;
pub use status::*;
pub use verify::*;

// The crate-internal CLI wrappers and cross-module helpers cannot travel
// through a `pub use …::*` glob (their visibility is narrower than `pub`);
// re-export them explicitly so `cmd_plan` and the relocated unit tests reach
// them through `crate::plan_cmd::<name>` / `super::*`.
pub(crate) use authoring::{plan_add, plan_rm, plan_update};
pub(crate) use envelope::{plan_project, require_apg_root};
pub(crate) use render::plan_render;
pub(crate) use status::{plan_complete, plan_done, plan_note, plan_undone};
pub(crate) use verify::{plan_verify, spec_has_requirement};

#[cfg(test)]
pub(crate) use authoring::{link_phase_edges, push_gate, validate_task_kind_tier};
#[cfg(test)]
pub(crate) use render::render_phase_tasks;
#[cfg(test)]
pub(crate) use verify::realization_candidates;

// The relocated unit/int tests reach the schema/collections names through
// `use super::*` (they were declared in this module before the split); keep
// them reachable in test builds only, so a non-test build carries no unused
// import.
#[cfg(test)]
use crate::schema::Record;
#[cfg(test)]
use std::collections::BTreeSet;

pub fn cmd_plan(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg plan <add|update|rm|done|undone|note|complete|render|verify> …");
    };
    match sub {
        "add" => plan_add(&args[1..]),
        "update" => plan_update(&args[1..]),
        "rm" => plan_rm(&args[1..]),
        "done" => plan_done(&args[1..]),
        "undone" => plan_undone(&args[1..]),
        "note" => plan_note(&args[1..]),
        "complete" => plan_complete(&args[1..]),
        "render" => plan_render(&args[1..]),
        "verify" => plan_verify(&args[1..]),
        // R5: `apg plan apply` was renamed `verify` — the binary applies
        // nothing; verify is the pre-merge coherence gate.
        "apply" => anyhow::bail!(
            "`apg plan apply` was renamed `verify` (R5) — the binary applies nothing; run `apg plan verify <project>` (the merge act is `apg project merge <project>`)"
        ),
        other => anyhow::bail!("unknown apg plan subcommand: {other}"),
    }
}

#[cfg(test)]
mod tests;
