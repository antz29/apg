//! Deterministic spec lint (`apg spec lint`) and the shared advisory wording
//! classifier.
//!
//! `apg spec lint` is the read-only, whole-durable-spec integrity check that
//! sits beside review. It reports the mechanical R2/R3/R4 violations and the
//! spec-delta gates as **errors**, and emits non-blocking advisory wording
//! warnings for tier-1-3 node bodies a reviewer is likely to reject.
//!
//! The delta arm reuses phase 1's merge-base durable-spec delta
//! (`crate::plan_cmd::change_set_spec_delta` / `SpecDelta`) and its
//! requirement-Satisfies gate (`crate::plan_cmd::coverage_check`) — one source
//! of truth, never a reimplementation. `wording_warning` is the single
//! definition of the tier-1-3 wording advisory: the lint and the
//! `apg node add` / `apg node update` write hooks all consume it. The linter is
//! read-only — it never writes.
