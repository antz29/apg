//! Post-apply re-resolution of the seed's unresolved edges (phase-04 task-3).
//!
//! The splice seeds the working DB from the previous scan's `db.lbug` and then
//! applies the target-only delta as DML ([`super::apply`]). An **unchanged**
//! file's `UnresolvedCall`/`UnresolvedUse` rows survive the whole-file copy
//! untouched: the delta never re-emits that file, so `apply` neither deletes nor
//! re-merges its outgoing rels. When the delta's own new declarations make one
//! of those targets real, the seeded copy would keep an unresolved edge to a
//! symbol that now exists — a shape a full rebuild never produces
//! (`domain.constraint.db-splice-equivalence`).
//!
//! [`resolve_delta_unresolved`] flips exactly those edges, mirroring the
//! assembly-time partition (`ingest::resolve_unresolved_edges`):
//!
//! * `Calls` is `Function → Function` only, so only a seed `UnresolvedCall`
//!   (always Function-sourced in the schema) whose target FQN is a real
//!   `Function` becomes a `Calls` edge;
//! * `Uses` is `Function|Struct → Struct` only, so only a seed `UnresolvedUse`
//!   whose target FQN is a real `Struct` becomes a `Uses` edge;
//! * `UnresolvedCall` onto a Struct target and `UnresolvedUse` onto a Function
//!   target stay unresolved — a full rebuild leaves them unresolved too — so
//!   the shared `UnresolvedTarget` row they name survives the GC;
//! * a target outside `newly_real` is never touched: the work is bounded by the
//!   delta's new declarations, never by the repo's unresolved-edge count.
//!
//! The work is batched as one literal list of `newly_real` FQNs per target
//! label, split into a `MERGE` and a `DELETE` statement (one write clause per
//! statement, as everywhere else on the seeded copy). Finally every
//! `UnresolvedTarget` row no surviving unresolved edge names is GC'd, exactly as
//! `apply`'s last stage does.
//!
//! An error propagates to `pipeline::try_splice_build`, which discards the
//! seeded copy and full-loads.

use std::collections::BTreeSet;

use lbug::Connection;

use super::seed::cypher_escape;

/// Converts the seed-resident unresolved edges whose target FQN is in
/// `newly_real` (the delta graph's real Function/Struct FQNs minus the captured
/// previous scan) into the resolved edges a full rebuild carries, then GCs the
/// orphan `UnresolvedTarget` rows.
///
/// Runs on the open seeded connection after `apply` and before `publish`; a
/// failure must reach the caller so it discards the seeded copy and full-loads.
pub fn resolve_delta_unresolved(
    conn: &Connection,
    newly_real: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if newly_real.is_empty() {
        return Ok(());
    }
    let list = literal_list(newly_real);

    // Function targets: only `UnresolvedCall` rows flip (Calls is Function →
    // Function), and only onto a real Function row — a still-`planned`
    // placeholder leaves the edge unresolved.
    let fn_match = format!(
        "UNWIND [{list}] AS target MATCH (a:Function)-[r:UnresolvedCall]->\
         (t:UnresolvedTarget {{fqn: target}}), (b:Function {{fqn: target}}) \
         WHERE b.status IS NULL OR b.status <> 'planned'"
    );
    conn.query(&format!("{fn_match} MERGE (a)-[:Calls]->(b)"))?;
    conn.query(&format!("{fn_match} DELETE r"))?;

    // Struct targets: only `UnresolvedUse` rows flip (Uses is Function|Struct →
    // Struct — the rel table's declared source labels, apply.rs's
    // CODE_REL_PAIRS). An `UnresolvedCall` onto a Struct and an
    // `UnresolvedUse` onto a Function keep their rows.
    let struct_match = format!(
        "UNWIND [{list}] AS target MATCH (a)-[r:UnresolvedUse]->\
         (t:UnresolvedTarget {{fqn: target}}), (b:Struct {{fqn: target}}) \
         WHERE b.status IS NULL OR b.status <> 'planned'"
    );
    conn.query(&format!("{struct_match} MERGE (a)-[:Uses]->(b)"))?;
    conn.query(&format!("{struct_match} DELETE r"))?;

    // GC the shared rows by reference, as `apply`'s last stage does: an
    // `UnresolvedTarget` lives only while some unresolved edge still names it.
    conn.query(
        "MATCH (u:UnresolvedTarget) WHERE NOT (u)<-[:UnresolvedCall]-() \
         AND NOT (u)<-[:UnresolvedUse]-() DETACH DELETE u",
    )?;
    Ok(())
}

/// Single-quotes a value for a Cypher string literal.
fn lit(value: &str) -> String {
    format!("'{}'", cypher_escape(value))
}

/// A Cypher list literal of single-quoted strings.
fn literal_list(values: &BTreeSet<String>) -> String {
    values.iter().map(|v| lit(v)).collect::<Vec<_>>().join(", ")
}
