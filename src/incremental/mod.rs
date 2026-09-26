//! Win-B incremental-scan orchestration (phase-02 task-8): the glue that turns
//! the content-addressed manifest ([`crate::cache`]), the git delta + fallbacks
//! ([`crate::delta`]), and the impact closure ([`crate::impact`]) into the
//! per-scan state `apg scan` needs.
//!
//! The persisted state lives under the shared store root
//! (`<git-common-dir>/apg/facts`, [`crate::cache::FactStore`]) so it is shared
//! across every worktree and branch:
//!
//! * `manifest.json` — the last scan's content manifest (path → blob OID),
//! * `scan.json` — the last scan's commit sha + global cache key,
//! * `index/deps.json` — the file-level dependency index (checkout-relative),
//! * `index/signatures.json` — each file's exported signature set,
//! * `index/overloads.json` — overload groups (checkout-relative),
//! * `<lang>/<cache-key>/` — the per-file fact units.
//!
//! Paths are checkout-relative throughout, so a fresh worktree of the same
//! repository reads the same state and re-bases it onto its own root — the
//! cross-worktree sharing half of `requirements.requirement.cross-worktree-cache-sharing`.

// The phase-02 win-B surface: a few helpers are consumed by the phase-03 splice
// path and the integration tests rather than by `cmd_scan` directly.
#![allow(dead_code)]

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
