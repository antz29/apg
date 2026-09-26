//! The layer catalog and tree layout of the node-file model (apg-projects
//! SPEC §3.1/§4.1/§5): the six logical layers — requirements, domain,
//! solution, plans, implementation, global — the node types each layer may
//! hold, and where each layer's node files live.
//!
//! Storage policy is separate from the catalog: requirements, domain,
//! solution, and global serialize durably under `apg/layers/`; plans
//! serialize **only** under the gitignored `apg/.trans/plans/` (transient,
//! per branch); implementation's real nodes ARE the scanned code (never
//! serialized) and only its attach-only note/constraint files exist durably
//! under `apg/layers/implementation/`. `apg/.trans/` mirrors every layer dir
//! (all six tiers, incl. global): feedback lands in the tier dir of its
//! attached node; plan nodes live under `.trans/plans/`.
//!
//! This module ships the catalog + layout as **data/constants**, plus the
//! SPEC §3.3 node-rule validation ([`validate_node`], [`validate_trees_acyclic`]).
//! The write/ingest and path/identity helpers are later phase-3/4 tasks. Every
//! layout constant is a
//! directory name relative to the layout root (the `apg/` dir — `specs::LAYOUT`
//! from the repo root); later consumers join them onto the root they resolve:
//!
//! ```text
//! apg/layers/                         (durable — one file per node)
//!   requirements/{stakeholder,user,requirement,note,constraint}/
//!   domain/{group,entity,value,service,note,constraint}/
//!   solution/{system,container,component,person,note,constraint}/
//!   implementation/{note,constraint}/ (attach-only — the real nodes are scanned code)
//!   global/{constraint,note}/
//! apg/.trans/                         (gitignored — mirrors the structure)
//!   plans/                            (the plan, per branch; tier dir of plan nodes)
//!   requirements/ domain/ solution/ implementation/ global/   (feedback mirrors)
//! ```
//!
//! The cohesive groups live in submodules — the catalog itself ([`catalog`]),
//! write-time validation ([`validate`]), the node-file schema and single-node
//! writer ([`node_file`]), code-endpoint resolution ([`code_refs`]), the atomic
//! write-through/orchestration path ([`write`]), and durable-tree ingestion
//! ([`tree`]) — all re-exported here so `crate::layers::<name>` keeps
//! resolving.

pub mod catalog;
pub mod code_refs;
pub mod node_file;
pub mod tree;
pub mod validate;
pub mod write;

pub use catalog::*;
pub use code_refs::*;
pub use node_file::*;
pub use tree::*;
pub use validate::*;
pub use write::*;

// Items whose own visibility is narrower than `pub` cannot travel through a
// glob re-export (E0365); a parent cannot re-export a child's `pub(crate)`
// item either (E0603). The cross-module helpers therefore carry `pub(crate)`
// visibility in their own submodule and are re-exported explicitly here: the
// production siblings (`write`/`tree`) reach `layer_of` and
// `validate_assembled_rules`, and the unit/int tests reach
// `validate_edge`/`EDGE_KINDS`/`MATRIX` through `use super::*`.
pub(crate) use tree::layer_of;
pub(crate) use validate::validate_assembled_rules;
#[cfg(test)]
pub(crate) use validate::{EDGE_KINDS, MATRIX, validate_edge};

// The relocated unit/int tests reach `std`'s collections through `use super::*`
// (they were declared in this module before the split); keep them reachable in
// test builds only, so a non-test build carries no unused import.
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;
