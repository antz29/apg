//! Bulk-load of a [`Graph`] into `db.lbug` via `COPY FROM` PARQUET load files
//! (SPEC §6 step 4), plus the `graph.jsonl` export (step 5).
//!
//! Load files are written with the low-level `parquet` writer so that string
//! columns carry the legacy `ConvertedType::UTF8` annotation. lbug 0.19.1's
//! PARQUET reader derives logical types from `converted_type` only, so the
//! arrow-rs default (`LogicalType::String`) would be misread as `BLOB`.
//!
//! The cohesive groups live in submodules — the low-level PARQUET sink
//! ([`parquet`]), the load tables and their pair enumerations ([`tables`]),
//! and the `graph.jsonl` export ([`export`]) — all re-exported here so
//! `crate::load::<name>` keeps resolving.

pub mod export;
pub mod parquet;
pub mod tables;

pub use export::*;
pub use parquet::*;
pub use tables::*;

// `spec_rel_pairs` is crate-internal (the pair enumeration the unit tests
// exercise); it cannot travel through a `pub use …::*` glob, so re-export it
// explicitly here — test builds only, since production reaches it inside
// `tables`.
#[cfg(test)]
pub(crate) use tables::spec_rel_pairs;

// The relocated unit tests reach `NodeKind` through `use super::*` (it was
// declared in this module before the split); keep it reachable in test builds
// only, so a non-test build carries no unused import.
#[cfg(test)]
use crate::graph::NodeKind;

#[cfg(test)]
pub(crate) mod tests;
