//! Incremental content: the **content-addressed manifest**, the shared
//! **fact store** and its body codec, and the checkout-local toolchain cache.
//!
//! Three concerns live here:
//!
//! 1. [`Manifest`] — a `path -> git blob OID` map recorded with each scan
//!    (`rust.apg.cache`). Delta/reuse decisions compare blob OIDs,
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
//!    re-bases them onto its own absolute root. Opening the store takes the
//!    exclusive `<root>/facts.lock` sibling of `facts.db`, polling up to a
//!    bounded ~30 s and failing with a named contention error, and the one
//!    write transaction holds that gate for its lifetime — never across a
//!    frontend run.
//!
//! 3. The per-worktree toolchain cache layout ([`toolchain`]) — each frontend's
//!    own native build cache lives under the checkout's
//!    `apg/.trans/cache/<language>/`, resolved from the apg root alone. The
//!    store **never** touches it: it is neither the shared fact store nor under
//!    the git common dir, a store format or config change leaves it intact, and
//!    every worktree keeps its own.
//!
//! Every fact a scan records lives in the redb tables alone: the content bodies
//! and units, the module scaffolding, and the per-worktree `refs`, `scans`,
//! `deps`, `signatures` and `overloads` rows, beside the `worktrees` registry and
//! the `meta` format row. There are no `manifest.json`, `scan.json` or
//! `index/*.json` files beside `facts.db` — the store carries everything.
//!
//! A scan only ever writes; the store's one deleting pass is [`gc`]. It runs in
//! a write transaction of its own after the record commit and collects every
//! body no live worktree manifest, live scaffolding row or unexpired scan intent
//! reaches — with the grace period resolved against the per-worktree intent rows,
//! the store's only time input — deleting each collected body together with every
//! unit row that would dangle from it.
//!
//! The global cache key ([`CacheKey`], `domain.value.cache-key`) is the
//! version/format/config identity that invalidates the whole cache on drift:
//! the explicit fact-store format version ([`FACTS_FORMAT_VERSION`]), scanner
//! JSONL schema/format, ingestor projection rules, the
//! scan config (languages, excludes, modules), and the classification config
//! (`apg/config.json`'s `default` / `types` / `structural` — the ingestor folds
//! it into each record's `code_type`, so a config change must force a full
//! load).

// This module holds the content-addressed API, and the whole module's
// surface is deliberately public even though a few accessors are not wired into
// production: `FactStore::has` is used only by the test suites, and
// `Manifest::rebase_root` currently has no caller at all.
//
// The implementation is split across cohesive submodules — the body codec
// ([`body`]), the manifest and global cache key ([`manifest`]), the per-file
// fact units, single-pass index and module scaffolding ([`fragment`]), the
// redb substrate and its `facts.lock` writer gate ([`store`]), the `FactStore`
// facade over the redb store ([`factstore`]), the reachability collector
// ([`gc`]), and the checkout-local toolchain cache layout ([`toolchain`]) — all
// re-exported here so `crate::cache::<name>` keeps resolving.
#![allow(dead_code)]

pub mod body;
pub mod factstore;
pub mod fragment;
pub mod gc;
pub mod manifest;
pub mod store;
pub mod toolchain;

pub use body::*;
pub use factstore::*;
pub use fragment::*;
pub use gc::*;
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
