//! Win-B impact closure + signature early cutoff (phase-02 tasks 5 and 6).
//!
//! The re-emission target set is **not** just the changed files:
//!
//! ```text
//! changed files
//!   ∪ reverse-dependency closure over the cached file-level dep index
//!   ∪ overload-group peers
//! ```
//!
//! with a **signature early cutoff**: a body-only change whose exported
//! signature is unchanged does not cascade to dependents.
//!
//! ## Granularity and signatures (DECIDED per language, task-6)
//!
//! A frontend's *emission granularity* is the unit it can re-emit on a targeted
//! scan:
//!
//! | language | granularity |
//! |---|---|
//! | Java | package |
//! | Go | package |
//! | Rust | crate |
//! | TypeScript | file |
//! | C# | project |
//! | Python | package/module |
//! | C++ | file |
//! | Markdown | file |
//!
//! The **exported signature** of a unit is its declaration surface: for a
//! struct, its FQN + kind; for a function, its FQN + arity/params; for a file,
//! the sorted set of its declared FQNs. A body-only edit that leaves every
//! declared FQN and every function's arity/params unchanged has an unchanged
//! signature and does not pull dependents into the target set.

// The phase-02 win-B surface: several helpers here (e.g. `target_set`,
// `signature_changed_files`, `by_language`) are consumed by the impact unit
// tests and the phase-03 orchestration rather than by `cmd_scan` directly.
#![allow(dead_code)]

// The impact closure is split into its three cohesive halves — the signature
// surface ([`signature`]), the file-level dependency index ([`depindex`]), and
// the target-set computation ([`targetset`]) — all re-exported here so
// `crate::impact::<name>` keeps resolving at the original paths.
pub mod depindex;
pub mod signature;
pub mod targetset;

pub use depindex::*;
pub use signature::*;
pub use targetset::*;

// The relocated unit tests reach `Graph`/`NodeKind`/`BTreeSet` through
// `use super::*` (they were declared in this module before the split); keep
// them reachable in test builds only, so a non-test build carries no unused
// import.
#[cfg(test)]
use crate::graph::{Graph, NodeKind};
#[cfg(test)]
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;
