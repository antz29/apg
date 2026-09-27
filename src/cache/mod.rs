//! Win-B incremental content: the **content-addressed manifest** and the shared
//! **content-addressed fact store** (phase-02 tasks 1 and 2).
//!
//! Two concerns live here:
//!
//! 1. [`Manifest`] — a `path -> git blob OID` map recorded with each scan
//!    (`rust.apg.cache`, task-1). Delta/reuse decisions compare blob OIDs,
//!    never mtime: touching a file without changing a byte keeps the same OID
//!    (its cached facts are reused); a byte edit changes the OID and
//!    invalidates them. The OID is git's own blob hash of the working-tree
//!    bytes ([`git2::Oid::hash_object`]), so the manifest is exactly the git
//!    content identity of the tree.
//!
//! 2. [`FactStore`] — the shared content-addressed store
//!    (`rust.apg.cache.FactStore`, task-2). The store root is the repository's
//!    **git common dir** (`<git-common-dir>/apg/facts`), shared across every
//!    worktree and branch of the repo. It holds per-file **fact units** keyed
//!    by content OID + resolution inputs + the global cache key, and serves
//!    per-worktree projections: a unit stored from one worktree can be read
//!    back from a fresh, near-identical worktree (same relative path + content
//!    OID) and re-based onto the reading worktree's absolute paths.
//!
//! The global cache key ([`CacheKey`], `domain.value.cache-key`) is the
//! version/format/config identity that invalidates the whole cache on drift:
//! binary version, scanner JSONL schema/format, ingestor projection rules, the
//! scan config (languages, excludes, modules), and the classification config
//! (`apg/config.json`'s `default` / `types` / `structural` — the ingestor folds
//! it into each record's `code_type`, so a config change must force a full
//! load).

// This module lands the phase-02 win-B content-addressed API; a few of its
// accessors (e.g. `FactStore::has`, `Manifest::rebase_root`) are consumed by
// the phase-03 splice path and the test suites rather than by `cmd_scan`
// today, so the whole module's surface is deliberately public.
//
// The implementation is split across cohesive submodules — the manifest and
// global cache key ([`manifest`]), the per-file fact units, single-pass index
// and module scaffolding ([`fragment`]), and the shared store ([`factstore`])
// — all re-exported here so `crate::cache::<name>` keeps resolving.
#![allow(dead_code)]

pub mod factstore;
pub mod fragment;
pub mod manifest;

pub use factstore::*;
pub use fragment::*;
pub use manifest::*;

// Test-only reaches: `src/cache/tests.rs` gets these through `use super::*;`,
// gated so the non-test library carries no unused import.
#[cfg(test)]
use crate::graph::{Location, Node, NodeKind};
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
mod tests;
