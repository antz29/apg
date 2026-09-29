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

/// The advisory message [`wording_warning`] returns for likely-flagged
/// tier-1-3 wording — the single source of truth both the lint and the
/// `apg node add` / `apg node update` write hooks print.
pub const WORDING_ADVISORY: &str =
    "this wording appears likely to be flagged in review — would you like to re-phrase?";

/// The shared advisory wording classifier over a tier-1-3 node body (R1/R5):
/// returns [`WORDING_ADVISORY`] when `body` carries negation, future-tense or
/// obligation, or time-relative wording a reviewer is likely to reject, and
/// `None` for plain present-tense prose. Pure — no I/O.
///
/// Words are matched as whole tokens (case-insensitively), so `note` and
/// `non-note` never trip the negation token `no`, while `no longer` does (its
/// `no` token). Tier-1-3 constraints are excluded by the lint, not here: a
/// negative rule legitimately lives in a layer-scoped constraint, so this
/// classifier stays a pure predicate over a body.
pub fn wording_warning(body: &str) -> Option<&'static str> {
    const NEGATION: [&str; 4] = ["not", "never", "no", "without"];
    const FUTURE: [&str; 5] = ["will", "shall", "must", "should", "would"];
    const TIME_RELATIVE: [&str; 5] = ["today", "now", "currently", "was", "previously"];
    let lowered = body.to_lowercase();
    let flagged = lowered.split(|c: char| !c.is_alphanumeric()).any(|tok| {
        NEGATION.contains(&tok) || FUTURE.contains(&tok) || TIME_RELATIVE.contains(&tok)
    });
    if flagged {
        Some(WORDING_ADVISORY)
    } else {
        None
    }
}
