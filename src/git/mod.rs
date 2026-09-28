//! Git-state capture, repo identity, and the project membership guard — the
//! git2 plumbing of the projects model (apg-projects R1–R8).
//!
//! Everything here is libgit2 (`git2`, `default-features = false` — no
//! https/ssh, no OpenSSL). **The git CLI is never shelled out to** (R6);
//! push/tag remain human-approved acts.
//!
//! The concerns are split into cohesive submodules — scan staleness
//! ([`state`]), repo identity + membership ([`identity`]), auto-commit +
//! scan_meta re-anchor ([`commit`]), and timestamps ([`time`]) — all
//! re-exported here so `crate::git::<name>` keeps resolving at the original
//! paths:
//!
//! 1. **Scan staleness** (agent-loop hardening, pre-projects model): every
//!    scan records the git state it ran under — HEAD sha plus tree cleanliness
//!    — as a `scan_meta` control record on line 1 of
//!    `apg/.trans/graph.jsonl` and as the DB's `Scan` node. A later
//!    spec/plan/review mutation that would re-ingest into a stale DB is
//!    refused *before* any JSONL write. All git reads here are git2-based.
//!
//! 2. **Repo identity + membership** (R3/R4/R7): mutations may only happen
//!    inside a project context — the project's worktree at
//!    `<main>/apg/.worktrees/<project>`, on the project's branch. Identity
//!    (branch, checkout path, main path, worktree existence) comes from git2;
//!    the walk-up `apg/` discovery stays the layout resolver (correct in
//!    worktrees by construction). The invariant — the git checkout root
//!    contains the walked-up `apg/` — is verified cheaply in
//!    [`repo_identity`], erroring on divergence (R7).
//!
//! 3. **Auto-commit + scan_meta re-anchor** (R8): after a graph mutation the
//!    funnel commits the touched file on the project branch via git2 — one
//!    commit per mutation, single-file diffs — and re-anchors the recorded
//!    scan_meta to the new state so consecutive mutations do not each demand
//!    a rescan. Plan mutations never commit: `apg/.trans` is gitignored and
//!    transient by design.
//!
//! 4. **Lifecycle self-cleanup** (merge self-cleanup / delete subcommand):
//!    [`remove_worktree`] + [`delete_branch`] — the shared git2 primitives
//!    `apg project merge`'s success path and `apg project delete` use to
//!    remove a project's worktree and delete its branch. The
//!    never-touch-default-branch law lives in [`delete_branch`].

pub mod commit;
pub mod identity;
pub mod state;
pub mod time;

pub use commit::*;
pub use identity::*;
pub use state::*;
pub use time::*;

// `iso8601` is crate-internal (the epoch-seconds formatter `now_iso8601`
// builds on, and the unit test's subject); an item narrower than `pub` cannot
// travel through a `pub use …::*` glob (E0365), so re-export it explicitly
// here — test builds only, since production reaches it inside `time`.
#[cfg(test)]
pub(crate) use time::iso8601;

#[cfg(test)]
mod tests;
