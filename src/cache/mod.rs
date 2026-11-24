//! Win-B incremental content: the **content-addressed manifest**, the shared
//! **fact store** and its body codec, and the checkout-local toolchain cache.
//!
//! Three concerns live here:
//!
//! 1. [`Manifest`] — a `path -> git blob OID` map recorded with each scan
//!    (`rust.apg.cache`, task-1). Delta/reuse decisions compare blob OIDs,
//!    never mtime: touching a file without changing a byte keeps the same OID
//!    (its cached facts are reused); a byte edit changes the OID and
//!    invalidates them. The OID is git's own blob hash of the working-tree
//!    bytes ([`git2::Oid::hash_object`]), so the manifest is exactly the git
//!    content identity of the tree.
//!
//! 2. [`Store`] — the shared fact store: **one** redb database at
//!    `<git-common-dir>/apg/facts.db`, shared by every worktree and branch of
//!    the repository. A body is a [`BodyId`] (BLAKE3-256) addressing a
//!    postcard payload behind a one-byte codec header ([`body`]), so two
//!    byte-identical bodies share one id and the store deduplicates on content
//!    alone. A unit is keyed by the five-part [`UnitKey`] — `(format, config
//!    digest, language, repo-relative path, blob OID)` — with its resolution
//!    `inputs` recorded as a diagnostic column, never as part of the reuse
//!    predicate. The refs, scans and delta rows are per-worktree: every such
//!    row is scoped by the reading worktree's [`WorktreeId`], so a fresh
//!    worktree of the same repository reads the shared content units and
//!    re-bases them onto its own absolute root.
//!
//! 3. The per-worktree toolchain cache layout ([`toolchain`]) — each frontend's
//!    own native build cache lives under the checkout's
//!    `apg/.trans/cache/<language>/`, resolved from the apg root alone. The
//!    store **never** touches it: it is neither the shared fact store nor under
//!    the git common dir, a store format or config change leaves it intact, and
//!    every worktree keeps its own.
//!
//! The phase-02 JSON records — `manifest.json`, `scan.json`, `index.json`, the
//! `index/` dependency/signature/overload files and `scaffolding.json` — still
//! live at their current `<git-common-dir>/apg/facts/` location until the
//! phase-4 tasks move them into the redb store; the store does not read them.
//!
//! The global cache key ([`CacheKey`], `domain.value.cache-key`) is the
//! version/format/config identity that invalidates the whole cache on drift:
//! binary version, scanner JSONL schema/format, ingestor projection rules, the
//! scan config (languages, excludes, modules), and the classification config
//! (`apg/config.json`'s `default` / `types` / `structural` — the ingestor folds
//! it into each record's `code_type`, so a config change must force a full
//! load).

// This module lands the win-B content-addressed API; a few of its accessors
// (e.g. `FactStore::has`, `Manifest::rebase_root`) are consumed by the splice
// path and the test suites rather than by `cmd_scan` today, so the whole
// module's surface is deliberately public.
//
// The implementation is split across cohesive submodules — the body codec
// ([`body`]), the manifest and global cache key ([`manifest`]), the per-file
// fact units, single-pass index and module scaffolding ([`fragment`]), the
// shared redb store ([`store`]), the transitional file-backed fact store
// ([`factstore`]), and the checkout-local toolchain cache layout
// ([`toolchain`]) — all re-exported here so `crate::cache::<name>` keeps
// resolving.
#![allow(dead_code)]

pub mod body;
pub mod factstore;
pub mod fragment;
pub mod manifest;
pub mod store;
pub mod toolchain;

pub use body::*;
pub use factstore::*;
pub use fragment::*;
pub use manifest::*;
pub use store::*;
pub use toolchain::*;

// Test-only reaches: `src/cache/tests.rs` gets these through `use super::*;`,
// gated so the non-test library carries no unused import.
#[cfg(test)]
use crate::graph::{Location, Node, NodeKind};
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
mod tests;
