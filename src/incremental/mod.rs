//! Incremental-scan orchestration: the glue that turns
//! the content-addressed manifest ([`crate::cache`]), the git delta + fallbacks
//! ([`crate::delta`]), and the impact closure ([`crate::impact`]) into the
//! per-scan state `apg scan` needs.
//!
//! The persisted state lives under the shared store root
//! (`<git-common-dir>/apg`, [`crate::cache::FactStore`]) so it is shared across
//! every worktree and branch: `facts.db`, whose redb tables carry the fact
//! units, the body blobs and the module scaffolding, beside the `facts.lock`
//! writer gate — an opening reader or writer waits on it for a bounded ~30 s
//! and then fails with a named contention error.
//!
//! This worktree's own orchestration state is kept as redb rows in that same
//! store, scoped by its [`crate::cache::WorktreeId`]:
//!
//! * `scans` — this worktree's scan record (commit sha, global cache key and
//!   content key),
//! * `refs` — the last scan's content manifest (path → blob OID),
//! * `deps` — the file-level dependency index (checkout-relative),
//! * `signatures` — each file's exported signature set,
//! * `overloads` — overload groups (checkout-relative).
//!
//! There are no `manifest.json`, `scan.json` or `index/*.json` files beside
//! `facts.db` — the store carries every row.
//!
//! Paths are checkout-relative throughout, so the content units one worktree
//! records are reachable from every other worktree of the repository
//! (`requirements.requirement.fact-store-is-one-redb-database`), while the
//! refs/scan/delta rows stay this worktree's own
//! (`requirements.requirement.fact-manifests-are-per-worktree`).

// The orchestration is split into its cohesive halves — the language mapping
// ([`language`]), the preparation ([`prepare`]), and the recording
// ([`record`]) — all re-exported here so `crate::incremental::<name>` keeps
// resolving at the original paths. The submodule names `prepare` and `record`
// share their names with the re-exported free functions `incremental::prepare`
// and `incremental::record`; the names live in different namespaces, so the
// call paths keep resolving to the functions.
pub mod language;
pub mod prepare;
pub mod record;

pub use language::*;
pub use prepare::*;
pub use record::*;

// The relocated unit tests reach `Graph`/`signatures_of_graph`/`Path` through
// `use super::*` (they were declared in this module before the split); keep
// them reachable in test builds only, so a non-test build carries no unused
// import.
#[cfg(test)]
use crate::graph::Graph;
#[cfg(test)]
use crate::impact::signatures_of_graph;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
mod tests;
