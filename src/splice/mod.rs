//! Win-C DB seed (phase-03 task-1): seed the working database from the previous
//! scan's `apg/.trans/db.lbug` by a **whole-file copy**.
//!
//! The splice mechanism is deliberately unambiguous — copy file → open copied
//! DB → apply the delta as DML → atomically publish:
//!
//! 1. **this module (task-1)** — resolve/validate the previous DB, copy it as a
//!    whole file to a temp sibling in the SAME directory (so the eventual
//!    publish is a same-filesystem `rename`, never a cross-device copy), and
//!    open the copy read-write;
//! 2. **task-2** — apply the delta to the copy as DML only (delete-then-insert
//!    of the affected rows). No parquet build and no `COPY` of the unaffected
//!    data: the whole-file copy already preserves every unaffected node/rel row,
//!    so the splice path runs no full-load pass;
//! 3. **task-3** — checkpoint+close the spliced copy, then atomically publish
//!    BOTH artifacts: the `db.lbug` rename over `target_path` and the fresh
//!    `graph.jsonl` rename over its `.trans/` sibling — with backups and
//!    rollback, so a partial swap can never leave a new DB paired with a stale
//!    export;
//! 4. **task-4** — the pipeline dispatch that decides seed-vs-full-load.
//!
//! This is seed-from-previous, not overlay-on-empty, per
//! `domain.note.cache-store-and-splice`.
//!
//! ## Invalidation → the existing full load
//!
//! A seed is only valid when the previous DB **exists** AND its
//! **schema/format/version** is compatible with this binary:
//!
//! * no `db.lbug` at all → [`SeedFallback::MissingPrevious`];
//! * a file this binary cannot open (a different LadybugDB storage format, or
//!   corruption) → [`SeedFallback::Unreadable`];
//! * a DB that opens but whose schema differs from this binary's
//!   [`crate::load::create_schema`] → [`SeedFallback::IncompatibleSchema`].
//!
//! Any of these hands the caller back to the existing
//! `remove + create_schema + copy_from` full load. The full load stays the
//! **correctness reference** (`domain.constraint.db-splice-equivalence`): a
//! spliced DB must answer identically to a full rebuild, so the seed is an
//! optimization that must be provably equivalent, never a second source of
//! truth.
//!
//! The expected schema is not hand-maintained. It is produced by running this
//! binary's own `create_schema` against a throwaway in-memory DB and
//! introspecting the result, so any future schema change automatically
//! invalidates seeds written by an older/newer binary with no list to update.

// The phase-03 surface is staged: the delta (task-2), the publish (task-3), and
// the pipeline dispatch (task-4) consume these entry points. Until they land the
// module is compiled but not yet wired into `run_pipeline` — the same staging
// `incremental` (phase-02) used before task-4.
#![allow(dead_code)]

// The seam is split into its three cohesive halves — the seed ([`seed`]), the
// delta application ([`apply`]), and the atomic publish ([`publish`]) — all
// re-exported here so `crate::splice::<name>` keeps resolving at the original
// paths.
pub mod apply;
pub mod publish;
pub mod seed;

pub use apply::*;
pub use publish::*;
pub use seed::*;

#[cfg(test)]
mod tests;
