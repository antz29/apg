//! The delta-application half of the splice (phase-03 task-2): apply the win-B
//! assembled graph's delta to the seeded copy as DML only — upsert the
//! re-emitted units, detach the disappeared ones, lifecycle the shared
//! `UnresolvedTarget` rows, and refresh the single `Scan` row.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use lbug::Connection;

use crate::graph::{Graph, NodeKind};
use crate::load;
use crate::schema::SCAN_HEAD;

use super::seed::{SeededDb, cell, cypher_escape, query_rows};

// ---------------------------------------------------------------------------
// Delta application (phase-03 task-2)
// ---------------------------------------------------------------------------
//
// The seeded DB is the previous scan's `db.lbug`, whole-file copied and opened
// read-write (task-1). Task-2 applies **only the delta** as DML: the re-emitted
// target units are upserted in place, the units whose FQN disappears are
// detached, the shared `UnresolvedTarget` rows are lifecycle-managed by
// reference, and the single `Scan` row (SCAN_HEAD) is refreshed. No parquet
// build and no `COPY`: every unaffected row survives byte-for-byte from the
// seed, which is what makes a spliced DB equal a full rebuild
// (`domain.constraint.db-splice-equivalence`).
//
// ## The input is the win-B assembled graph, not the target-only spool
//
// `graph` is the full assembled graph (`ingest::ingest_with_reuse`'s output:
// the re-emitted target units PLUS the cached unaffected units). The splicer
// selects the **delta rows** from it:
//
// * a Struct/Function is a delta unit when its location path is in `targets`;
// * a File is a delta unit when its FQN (the absolute path) is in `targets`;
// * a Module is always re-upserted (there are few, and the module set is part
//   of the equivalence — a removed file can orphan its module);
// * an UnresolvedTarget is inserted only when a delta edge first references it.
//
// Reading the FULL assembled graph (rather than a target-only spool) is what
// keeps a target unit's edge to an **unaffected** unit: the frontend emits such
// a cross-cut edge with its canonical FQN, and the cached endpoint only exists
// in the assembled graph — a target-only graph would have pruned it in
// `finalize_graph` before the splicer ever saw it.
//
// ## Scheme (feedback-90, option (i): upsert in place + replace outgoing rels)
//
// For a **persisting** FQN (body-only or signature change):
//   1. delete every rel it AUTHORS (outgoing Calls/Uses/UnresolvedCall/
//      UnresolvedUse/Contains) — delete-before-insert holds for rels;
//   2. UPSERT the node in place (`MERGE … SET`, the `ArtifactDb::merge_node`
//      pattern) — never a node delete, so INCOMING Calls/Uses from units
//      OUTSIDE the re-emission target set survive untouched (a body-only change
//      re-emits only the changed file, so its callers are not in the delta);
//   3. MERGE its new outgoing rels from the delta (the `ArtifactDb::merge_edge`
//      rel-upsert pattern).
//
// For a **disappearing** FQN (removed file/module, or an overload re-suffix
// retiring an old FQN): delete its authored rels, then `DETACH DELETE` the
// node. The silent incoming-edge drop is safe ONLY here: a disappearing
// exported symbol is a signature change, so the reverse-dependency closure put
// every referrer in the re-emission target set, and their replacement outgoing
// rels are merged **before** the `DETACH DELETE` runs. No edge a full rebuild
// keeps is lost. DETACH-DELETEing a persisting FQN is forbidden.
//
// ## Authored/transient seed assumption
//
// The seed already holds the CURRENT authored/transient tables (Requirement/
// Entity/Note/Constraint/Plan/PlanPhase/Task/Feedback and their rels) by
// write-through: every durable/transient mutation synchronously projects into
// `db.lbug`. The splice writes no authored row — `graph` is the **code**
// assembly (scanner records spliced with cached fact units), never the layer/
// transient records, so a planned node can never be re-marked over a realized
// one. A missing/incompatible previous DB falls back to the full load
// (task-1/task-4).

/// The single `Scan` row (SCAN_HEAD) a spliced DB must carry. Mirrors the
/// `scan_meta` control record (`schema::Record::ScanMeta`) and the columns
/// `build_load_files` writes from a `Scan` node: `git_sha`/`git_clean` are
/// `None` outside a git repo (stored as empty strings), and `content_key` is
/// the phase-01 content-identity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRow {
    pub git_sha: Option<String>,
    pub git_clean: Option<bool>,
    pub content_key: Option<String>,
    pub scanned_at: String,
}

/// The delta a spliced DB must apply, expressed against the seeded copy.
///
/// `graph` is the win-B assembled graph (target units + cached unaffected
/// units, see the module comment); `targets` is the FULL phase-2 re-emission
/// target set — changed files ∪ reverse-dependency closure ∪ overload-group
/// peers — and is the **delete scope**; `removed_fqns` is the subtraction half
/// of the full-universe seam (`incremental::Prepared::removed_fqns`); `scan` is
/// the new SCAN_HEAD row.
pub struct SpliceDelta<'a> {
    pub graph: &'a Graph,
    pub targets: &'a BTreeSet<String>,
    pub removed_fqns: &'a BTreeSet<String>,
    pub scan: ScanRow,
}

/// What the delta application touched — logged by the pipeline and asserted by
/// the unit tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpliceReport {
    /// Nodes MERGE-upserted in place (delta code units + modules + first-
    /// referenced UnresolvedTargets).
    pub nodes_upserted: u64,
    /// Nodes DETACH-DELETEd (the disappeared units).
    pub nodes_deleted: u64,
    /// Outgoing rels deleted from the delta/delete units before re-insert.
    pub edges_deleted: u64,
    /// New outgoing rels merged from the delta.
    pub edges_merged: u64,
    /// UnresolvedTarget rows GC'd after the delta rels landed.
    pub unresolved_gc: u64,
    /// Whether the SCAN_HEAD row was refreshed.
    pub scan_refreshed: bool,
    /// DML statements executed by [`apply`] — each BATCHED statement counted
    /// ONCE, never per row (phase-04 task-30). The batching claim of task-14:
    /// a jgrapht-scale delta (the pre-fix ~82 298 per-row statements,
    /// `plan.note-87`) must report a small constant instead of a number that
    /// grows with the delta's node/edge count.
    pub dml_statements: u64,
}

impl SeededDb {
    /// Applies `delta` to the seeded copy (task-2's DML half) on a fresh
    /// connection. Drop the returned report; the seeded DB is then ready for
    /// task-3's atomic publish.
    pub fn apply(&self, delta: &SpliceDelta<'_>) -> anyhow::Result<SpliceReport> {
        let conn = self.conn()?;
        apply(&conn, delta)
    }
}

/// Applies `delta` to the open (seeded) DB. Idempotent for a repeated identical
/// delta: an upsert re-sets the same props and a re-merge of a present rel is a
/// no-op. Runs as a sequence of DML statements; a mid-sequence failure leaves
/// the seed partially mutated, which the caller abandons via
/// `SeededDb::discard` and re-runs through the full load (task-4).
pub fn apply(conn: &Connection, delta: &SpliceDelta<'_>) -> anyhow::Result<SpliceReport> {
    let mut report = SpliceReport::default();

    // The module delete scope is bounded by the delta's target set: a seeded
    // Module is only re-decided when the delta reaches a File somewhere in its
    // subtree. The assembled graph cannot vouch for a module the delta never
    // touched — a skipped/emission-filtered language emits no global module
    // scaffolding, so its `Module -> Module` hierarchy and pure-intermediate
    // modules never reach the assembled graph. Deleting such a module (or
    // wiping its outgoing Contains rels) would drop rows a full rebuild keeps
    // (`domain.constraint.db-splice-equivalence`).
    let subtrees = module_file_subtrees(conn)?;
    let reached = |m: &str| {
        subtrees
            .get(m)
            .is_some_and(|files| files.iter().any(|f| delta.targets.contains(f)))
    };
    // A module can only be *disappearing* when EVERY file under it is in the
    // delta scope: any unchanged file keeps it alive in a full rebuild.
    let fully_reached = |m: &str| {
        subtrees.get(m).is_some_and(|files| {
            !files.is_empty() && files.iter().all(|f| delta.targets.contains(f))
        })
    };

    // --- 1. Select the delta's code units from the assembled graph ---------
    //
    // The seeded Module FQNs, fetched ONCE (also reused for the delete scope in
    // step 2): a module the seed does not own — a package/dir the previous scan
    // did not have — is always re-decided, because the seed can vouch for
    // neither its row nor its outgoing Contains rels.
    let seeded_modules: BTreeSet<String> = seed_modules(conn)?.into_iter().collect();
    let mut delta_funcs: Vec<String> = Vec::new();
    let mut delta_structs: Vec<String> = Vec::new();
    let mut delta_files: Vec<String> = Vec::new();
    let mut delta_modules: Vec<String> = Vec::new();
    // PHASE_09: the language roots the delta's assembled graph carries (one per
    // spawned stream plus the replayed scaffolding of a skipped language).
    let mut delta_languages: Vec<String> = Vec::new();
    // FQN -> label for every node the delta owns (code units + modules).
    let mut delta_labels: HashMap<String, &'static str> = HashMap::new();
    for (fqn, node) in &delta.graph.nodes {
        match node.kind {
            NodeKind::Language => {
                // The language root is global scaffolding: upsert it
                // unconditionally (a few rows) so the spliced DB carries the
                // same Language set as a full rebuild. It never disappears on a
                // partial scan, so it is not in the delete scope.
                delta_languages.push(fqn.clone());
                delta_labels.insert(fqn.clone(), "Language");
            }
            NodeKind::Module => {
                // A module is re-decided when the delta's target set reaches a
                // File in its SEED subtree, OR when the seed has no Module row
                // for it at all (a NEW package/dir: the assembled graph is the
                // only source of truth for its node and its Contains rels). An
                // otherwise-untouched module's seed row and outgoing Contains
                // rels are already exact, and the assembled graph may not even
                // carry them (a skipped language's scaffolding) — that guard is
                // preserved. A delta File/Module added UNDER an untouched module
                // is handled additively by the `Contains` merge in step 6.
                if reached(fqn) || !seeded_modules.contains(fqn) {
                    delta_modules.push(fqn.clone());
                    delta_labels.insert(fqn.clone(), "Module");
                }
            }
            NodeKind::Struct => {
                if location_in_targets(node, delta.targets) {
                    delta_structs.push(fqn.clone());
                    delta_labels.insert(fqn.clone(), "Struct");
                }
            }
            NodeKind::Function => {
                if location_in_targets(node, delta.targets) {
                    delta_funcs.push(fqn.clone());
                    delta_labels.insert(fqn.clone(), "Function");
                }
            }
            NodeKind::File if delta.targets.contains(fqn) => {
                delta_files.push(fqn.clone());
                delta_labels.insert(fqn.clone(), "File");
            }
            _ => {}
        }
    }

    // --- 2. The delete scope: seed units owned by the target set -----------
    //
    // The delete scope is the WHOLE re-emission target set, never just the
    // changed+removed files: an overload re-suffix retires the old FQN of a
    // unit in an *unchanged* peer file, and a cascaded dependent's new facts
    // can retire FQNs too. A seeded code unit whose location path is a target
    // path (or whose FQN directly names one, for a File) is therefore in scope;
    // it disappears iff the assembled graph does not re-emit it.
    let mut in_scope: BTreeMap<String, &'static str> = code_fqns_in_paths(conn, delta.targets)?;
    for m in &seeded_modules {
        // Only a module whose ENTIRE file subtree is in the delta can be
        // genuinely gone; an untouched module (or one with any reused file left)
        // is left exactly as the seed left it.
        if fully_reached(m) {
            in_scope.insert(m.clone(), "Module");
        }
    }
    for fqn in delta.removed_fqns {
        if !in_scope.contains_key(fqn)
            && let Some(label) = db_code_label(conn, fqn)?
        {
            in_scope.insert(fqn.clone(), label);
        }
    }

    let mut disappearing: BTreeMap<String, &'static str> = BTreeMap::new();
    for (fqn, label) in &in_scope {
        if !delta_labels.contains_key(fqn) {
            disappearing.insert(fqn.clone(), label);
        }
    }

    // --- 3. Delete every rel authored by a delta OR disappearing unit -------
    //
    // Bulk per (label, declared rel-set): an authored rel is deleted before its
    // node is upserted (persisting) or detached (disappearing), so the delta's
    // new rels are the only ones left. `edges_deleted` is the pre-count.
    let mut delete_funcs: BTreeSet<String> = delta_funcs.iter().cloned().collect();
    let mut delete_structs: BTreeSet<String> = delta_structs.iter().cloned().collect();
    let mut delete_files: BTreeSet<String> = delta_files.iter().cloned().collect();
    let mut delete_modules: BTreeSet<String> = delta_modules.iter().cloned().collect();
    for (fqn, label) in &disappearing {
        match *label {
            "Function" => {
                delete_funcs.insert(fqn.clone());
            }
            "Struct" => {
                delete_structs.insert(fqn.clone());
            }
            "File" => {
                delete_files.insert(fqn.clone());
            }
            "Module" => {
                delete_modules.insert(fqn.clone());
            }
            _ => {}
        }
    }
    let (deleted, stmts) =
        delete_authored_rels(conn, "Function", AUTH_RELS_FUNCTION, &delete_funcs)?;
    report.edges_deleted += deleted;
    report.dml_statements += stmts;
    let (deleted, stmts) = delete_authored_rels(conn, "Struct", AUTH_RELS_STRUCT, &delete_structs)?;
    report.edges_deleted += deleted;
    report.dml_statements += stmts;
    let (deleted, stmts) = delete_authored_rels(conn, "File", AUTH_RELS_CONTAINS, &delete_files)?;
    report.edges_deleted += deleted;
    report.dml_statements += stmts;
    let (deleted, stmts) =
        delete_authored_rels(conn, "Module", AUTH_RELS_CONTAINS, &delete_modules)?;
    report.edges_deleted += deleted;
    report.dml_statements += stmts;

    // --- 4. Upsert the delta's nodes in place (batched, one statement/label) --
    //
    // Every node the pre-fix per-row helper upserted (delta_funcs, delta_structs,
    // delta_files, delta_modules, delta_unresolved) is still upserted with the
    // same properties; the DML is now grouped per label into ONE multi-row
    // `UNWIND … MERGE … SET` (task-29), so ~tens of thousands of node statements
    // become ≤5.
    for (label, fqns) in [
        ("Function", &delta_funcs),
        ("Struct", &delta_structs),
        ("File", &delta_files),
        ("Module", &delta_modules),
        ("Language", &delta_languages),
    ] {
        let (rows, stmts) = upsert_node(conn, label, fqns, delta.graph)?;
        report.nodes_upserted += rows;
        report.dml_statements += stmts;
    }

    // --- 5. Insert every first-referenced UnresolvedTarget, before its edge -
    let mut delta_unresolved: BTreeSet<String> = BTreeSet::new();
    for (from, to, _) in &delta.graph.unresolved_calls {
        if delta_labels.contains_key(from)
            && delta
                .graph
                .nodes
                .get(to)
                .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget)
        {
            delta_unresolved.insert(to.clone());
        }
    }
    for (from, to) in &delta.graph.unresolved_uses {
        if delta_labels.contains_key(from)
            && delta
                .graph
                .nodes
                .get(to)
                .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget)
        {
            delta_unresolved.insert(to.clone());
        }
    }
    let unresolved_rows: Vec<String> = delta_unresolved.iter().cloned().collect();
    let (rows, stmts) = upsert_node(conn, "UnresolvedTarget", &unresolved_rows, delta.graph)?;
    report.nodes_upserted += rows;
    report.dml_statements += stmts;

    // --- 6. MERGE the delta's new outgoing rels (batched per rel-table pair) --
    //
    // Resolve every delta edge exactly as the pre-fix per-row `merge` closure
    // did (the Contains additive exception, the disappearing-target skip, the
    // declared-pair guard), but COLLECT the survivors into `(table, from_label,
    // to_label)` groups and emit ONE multi-row `UNWIND … MATCH … MERGE` per
    // group — so ~tens of thousands of per-edge statements become one per
    // declared `(table, label-pair)` the delta actually uses (task-14).
    let mut resolved: HashMap<String, Option<&'static str>> = HashMap::new();
    type RelGroup =
        BTreeMap<(&'static str, &'static str, &'static str), Vec<(String, String, String)>>;
    let mut groups: RelGroup = BTreeMap::new();
    {
        let mut stage =
            |table: &'static str, from: &str, to: &str, target_type: &str| -> anyhow::Result<()> {
                // The delta authors an edge when it owns the SOURCE unit.
                // `Contains` is the one exception (feedback-118): a seeded
                // Module the delta did NOT re-decide still authors `Module ->
                // File` / `Module -> Module` to a child the delta ADDED — a full
                // rebuild carries that edge and the seed cannot, so it must be
                // merged ADDITIVELY (nothing is deleted for that module, so its
                // pre-existing Contains rels stay untouched).
                let from_label = match delta_labels.get(from).copied() {
                    Some(label) => label,
                    None if table == "Contains" && delta_labels.contains_key(to) => {
                        match endpoint_label(conn, from, delta.graph, &mut resolved)? {
                            Some(label) => label,
                            // An unknown/unseeded source: the full load prunes it too.
                            None => return Ok(()),
                        }
                    }
                    // An edge authored outside the delta is untouched.
                    None => return Ok(()),
                };
                // An edge into a unit that is being detached would be dropped by
                // the DETACH DELETE anyway; a full rebuild prunes it as dangling.
                if disappearing.contains_key(to) {
                    return Ok(());
                }
                let to_label = endpoint_label(conn, to, delta.graph, &mut resolved)?;
                let Some(to_label) = to_label else {
                    return Ok(()); // dangling endpoint — the full load prunes it too
                };
                if !pair_allowed(table, from_label, to_label) {
                    return Ok(());
                }
                groups
                    .entry((table, from_label, to_label))
                    .or_default()
                    .push((from.to_string(), to.to_string(), target_type.to_string()));
                Ok(())
            };
        for (from, to) in &delta.graph.contains {
            stage("Contains", from, to, "")?;
        }
        for (from, to) in &delta.graph.calls {
            stage("Calls", from, to, "")?;
        }
        for (from, to) in &delta.graph.uses {
            stage("Uses", from, to, "")?;
        }
        for (from, to, tt) in &delta.graph.unresolved_calls {
            stage("UnresolvedCall", from, to, tt)?;
        }
        for (from, to) in &delta.graph.unresolved_uses {
            stage("UnresolvedUse", from, to, "")?;
        }
    }
    for (&(table, from_label, to_label), edges) in &groups {
        let rows = edges
            .iter()
            .map(|(from, to, target_type)| {
                if table == "UnresolvedCall" {
                    format!(
                        "{{from: {}, to: {}, tt: {}}}",
                        lit(from),
                        lit(to),
                        lit(target_type)
                    )
                } else {
                    format!("{{from: {}, to: {}}}", lit(from), lit(to))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        if table == "UnresolvedCall" {
            conn.query(&format!(
                "UNWIND [{rows}] AS row MATCH (a:{from_label} {{fqn: row.from}}), \
                 (b:{to_label} {{fqn: row.to}}) MERGE (a)-[r:UnresolvedCall]->(b) \
                 SET r.target_type = row.tt"
            ))?;
        } else {
            conn.query(&format!(
                "UNWIND [{rows}] AS row MATCH (a:{from_label} {{fqn: row.from}}), \
                 (b:{to_label} {{fqn: row.to}}) MERGE (a)-[:{table}]->(b)"
            ))?;
        }
        report.edges_merged += edges.len() as u64;
        report.dml_statements += 1;
    }

    // --- 7. DETACH DELETE the disappeared units ----------------------------
    //
    // After every replacement rel has been merged (step 6): the only edges left
    // pointing at a disappearing node are stale incoming edges whose referrer
    // either re-emitted a replacement or was never in the reverse closure.
    for (label, fqns) in group_by_label(&disappearing) {
        detach_delete(conn, label, &fqns)?;
        report.nodes_deleted += fqns.len() as u64;
        report.dml_statements += 1;
    }

    // --- 8. GC the shared UnresolvedTarget rows by reference ---------------
    //
    // UnresolvedTarget is deduplicated by FQN and SHARED across units, never
    // unit-owned: after the delta rels land, a target with no surviving
    // UnresolvedCall/UnresolvedUse edge is unreferenced by ANY unit.
    report.unresolved_gc = count(
        conn,
        "MATCH (u:UnresolvedTarget) WHERE NOT (u)<-[:UnresolvedCall]-() \
         AND NOT (u)<-[:UnresolvedUse]-() RETURN count(*)",
    )? as u64;
    conn.query(
        "MATCH (u:UnresolvedTarget) WHERE NOT (u)<-[:UnresolvedCall]-() \
         AND NOT (u)<-[:UnresolvedUse]-() DETACH DELETE u",
    )?;
    report.dml_statements += 1;

    // --- 9. Refresh the single Scan row (SCAN_HEAD) ------------------------
    //
    // A full load rewrites the Scan node from the scan_meta record; the splice
    // must DELETE the seeded row and INSERT the new one, or the previous scan's
    // git state survives and the spliced DB disagrees with a full rebuild and
    // with graph.jsonl line 1.
    conn.query("MATCH (s:Scan) DELETE s")?;
    conn.query(&format!(
        "CREATE (s:Scan {{fqn: {}, git_sha: {}, git_clean: {}, content_key: {}, scanned_at: {}}})",
        lit(SCAN_HEAD),
        lit(delta.scan.git_sha.as_deref().unwrap_or("")),
        lit(&delta
            .scan
            .git_clean
            .map(|c| c.to_string())
            .unwrap_or_default()),
        lit(delta.scan.content_key.as_deref().unwrap_or("")),
        lit(&delta.scan.scanned_at),
    ))?;
    report.dml_statements += 2;
    report.scan_refreshed = true;

    Ok(report)
}

/// The outgoing rel sets a code node authors, per its label. Each type is
/// declared `FROM` that label in the schema (`create_schema`), so the Cypher
/// never names an undeclared pair.
const AUTH_RELS_FUNCTION: &str = "Calls|Uses|UnresolvedCall|UnresolvedUse";
const AUTH_RELS_STRUCT: &str = "Uses|UnresolvedUse|Contains";
const AUTH_RELS_CONTAINS: &str = "Contains";

/// True when `node`'s location path is one of the delete-scope target paths.
fn location_in_targets(node: &crate::graph::Node, targets: &BTreeSet<String>) -> bool {
    node.location
        .as_ref()
        .is_some_and(|l| targets.contains(&l.path.to_string_lossy().into_owned()))
}

/// Single-quotes a value for a Cypher string literal.
fn lit(value: &str) -> String {
    format!("'{}'", cypher_escape(value))
}

/// A Cypher list literal of single-quoted strings.
fn literal_list(values: &BTreeSet<String>) -> String {
    values.iter().map(|v| lit(v)).collect::<Vec<_>>().join(", ")
}

/// Every seeded Struct/Function located in one of `targets`, plus every seeded
/// File whose FQN is a target path, with its label.
fn code_fqns_in_paths(
    conn: &Connection,
    targets: &BTreeSet<String>,
) -> anyhow::Result<BTreeMap<String, &'static str>> {
    let mut out = BTreeMap::new();
    if targets.is_empty() {
        return Ok(out);
    }
    let list = literal_list(targets);
    for label in ["Struct", "Function"] {
        let (_, rows) = query_rows(
            conn,
            &format!("MATCH (n:{label}) WHERE n.path IN [{list}] RETURN n.fqn AS fqn"),
        )?;
        for row in rows {
            out.insert(cell(&row, 0), label);
        }
    }
    let (_, rows) = query_rows(
        conn,
        &format!("MATCH (n:File) WHERE n.fqn IN [{list}] RETURN n.fqn AS fqn"),
    )?;
    for row in rows {
        out.insert(cell(&row, 0), "File");
    }
    Ok(out)
}

/// Every **real** seeded Module FQN — `planned` placeholder modules are
/// transient records the splice never owns (the authored/transient seed
/// assumption), so they are excluded from the delete scope.
fn seed_modules(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let (_, rows) = query_rows(
        conn,
        "MATCH (n:Module) WHERE n.status IS NULL OR n.status <> 'planned' RETURN n.fqn AS fqn",
    )?;
    Ok(rows.iter().map(|r| cell(r, 0)).collect())
}

/// The File FQNs transitively contained under each seeded Module — the module's
/// whole file subtree through the `Module -> Module` hierarchy plus its direct
/// `Module -> File` children. The splicer uses it to decide whether the delta's
/// re-emission target set reaches a module's content: a module whose subtree is
/// outside the target set is untouched and must survive the splice verbatim.
fn module_file_subtrees(conn: &Connection) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let (_, rows) = query_rows(
        conn,
        "MATCH (p:Module)-[:Contains]->(c:Module) RETURN p.fqn AS p, c.fqn AS c",
    )?;
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        children
            .entry(cell(&row, 0))
            .or_default()
            .push(cell(&row, 1));
    }
    let (_, rows) = query_rows(
        conn,
        "MATCH (m:Module)-[:Contains]->(f:File) RETURN m.fqn AS m, f.fqn AS f",
    )?;
    let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        direct
            .entry(cell(&row, 0))
            .or_default()
            .insert(cell(&row, 1));
    }

    // Every module that names a child or owns a file directly. A module with no
    // direct file (a pure-intermediate package node) still appears as a parent.
    let mut modules: BTreeSet<String> = children.keys().cloned().collect();
    for kids in children.values() {
        modules.extend(kids.iter().cloned());
    }
    modules.extend(direct.keys().cloned());

    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in &modules {
        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = vec![m.clone()];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(cur) = stack.pop() {
            // `Contains` is acyclic, but guard the walk anyway so a malformed
            // seed can never loop.
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(fs) = direct.get(&cur) {
                files.extend(fs.iter().cloned());
            }
            if let Some(kids) = children.get(&cur) {
                stack.extend(kids.iter().cloned());
            }
        }
        out.insert(m.clone(), files);
    }
    Ok(out)
}

/// The label of a code node at `fqn`, or `None`. Excludes `planned`
/// placeholders on the four Implementation labels.
fn db_code_label(conn: &Connection, fqn: &str) -> anyhow::Result<Option<&'static str>> {
    for label in ["Function", "Struct", "File", "Module"] {
        let n = count(
            conn,
            &format!(
                "MATCH (n:{label} {{fqn: {}}}) WHERE n.status IS NULL OR n.status <> 'planned' RETURN count(*)",
                lit(fqn)
            ),
        )?;
        if n > 0 {
            return Ok(Some(label));
        }
    }
    Ok(None)
}

/// The DB label of an edge endpoint: the assembled graph's node when present,
/// else the seeded DB (a cached/unaffected endpoint).
fn endpoint_label(
    conn: &Connection,
    fqn: &str,
    graph: &Graph,
    cache: &mut HashMap<String, Option<&'static str>>,
) -> anyhow::Result<Option<&'static str>> {
    if let Some(node) = graph.nodes.get(fqn) {
        return Ok(node_label_of(node.kind));
    }
    if let Some(hit) = cache.get(fqn) {
        return Ok(*hit);
    }
    let label = db_code_label(conn, fqn)?;
    cache.insert(fqn.to_string(), label);
    Ok(label)
}

/// The node-table label of a code kind, or `None` for a non-code kind.
fn node_label_of(kind: NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::Module => Some("Module"),
        NodeKind::Struct => Some("Struct"),
        NodeKind::Function => Some("Function"),
        NodeKind::File => Some("File"),
        NodeKind::UnresolvedTarget => Some("UnresolvedTarget"),
        NodeKind::Language => Some("Language"),
        _ => None,
    }
}

/// The scanned **code** rel-table `(table, from, to)` triples the delta
/// re-merges. [`load::rel_table_pairs`] deliberately omits these (see its
/// `AUTHORED_REL_PAIRS` comment): it exists for the write-through merge guard,
/// which never re-merges scanned code edges — those are written by the load
/// path. The splicer is the exception: it re-emits scanned code edges as DML,
/// so it must admit the pairs the schema declares in `CREATE REL TABLE`
/// (`Calls`/`Uses`/`UnresolvedCall`/`UnresolvedUse`). The `Contains` code pairs
/// need no entry here — they already appear in `rel_table_pairs` via
/// `contains_pairs`.
pub const CODE_REL_PAIRS: [(&str, &str, &str); 6] = [
    ("Calls", "Function", "Function"),
    ("Uses", "Function", "Struct"),
    ("Uses", "Struct", "Struct"),
    ("UnresolvedCall", "Function", "UnresolvedTarget"),
    ("UnresolvedUse", "Function", "UnresolvedTarget"),
    ("UnresolvedUse", "Struct", "UnresolvedTarget"),
];

/// Whether the schema's rel table `table` declares an edge between the two node
/// labels — [`load::rel_table_pairs`] plus the scanned code pairs
/// ([`CODE_REL_PAIRS`]) — so an undeclared pair is skipped rather than fed to a
/// binder exception.
fn pair_allowed(table: &str, from: &str, to: &str) -> bool {
    CODE_REL_PAIRS
        .iter()
        .any(|(t, a, b)| *t == table && *a == from && *b == to)
        || load::rel_table_pairs()
            .iter()
            .any(|(t, a, b)| *t == table && *a == from && *b == to)
}

/// Counts and deletes the rels the `fqns` author from `label`. Returns
/// `(deleted, statements)`: the number of rels deleted and the number of DML
/// statements executed (0 for an empty set, else the ONE batched DELETE) so
/// [`apply`] can count the statement once, never per row.
fn delete_authored_rels(
    conn: &Connection,
    label: &str,
    rels: &str,
    fqns: &BTreeSet<String>,
) -> anyhow::Result<(u64, u64)> {
    if fqns.is_empty() {
        return Ok((0, 0));
    }
    let list = literal_list(fqns);
    let n = count(
        conn,
        &format!("MATCH (n:{label})-[r:{rels}]->() WHERE n.fqn IN [{list}] RETURN count(*)"),
    )?;
    conn.query(&format!(
        "MATCH (n:{label})-[r:{rels}]->() WHERE n.fqn IN [{list}] DELETE r"
    ))?;
    Ok((n as u64, 1))
}

/// Batched in-place node upsert (phase-04 task-29; the node-DML half of
/// task-14): `MERGE (n:Label {fqn: row.fqn}) SET props` over ONE multi-row
/// `UNWIND … AS row` statement PER LABEL, in place of the pre-fix helper's one
/// `conn.query` per node. The semantics are identical to the per-row helper it
/// replaces:
///
/// * an in-place upsert (`MERGE … SET`, never a node delete) so incoming
///   Calls/Uses from units OUTSIDE the re-emission target set survive;
/// * the Module planned-placeholder backstop — a `planned` row never re-marks a
///   realized Module (`ON MATCH` keeps a non-`planned` status untouched);
/// * the per-label property set `build_load_files` writes (Module: status;
///   Struct/Function: path/start/`end`/start_line/end_line/code_type/status;
///   File: start_line/end_line/code_type/status; UnresolvedTarget: category).
///
/// A node whose FQN is absent from `graph` is not upserted (the pre-fix guard).
/// Returns `(rows, statements)`: the row count (the caller's `nodes_upserted`
/// increment) and the DML statement count (0 for an empty group, else 1) for
/// the task-30 counter.
fn upsert_node(
    conn: &Connection,
    label: &str,
    fqns: &[String],
    graph: &Graph,
) -> anyhow::Result<(u64, u64)> {
    // One `{…}` map literal per row; values are inlined as Cypher literals (the
    // splicer already interpolates its own delta data).
    let mut rows: Vec<String> = Vec::with_capacity(fqns.len());
    for fqn in fqns {
        let Some(node) = graph.nodes.get(fqn) else {
            continue;
        };
        let status = lit(node.status.as_deref().unwrap_or(""));
        rows.push(match node.kind {
            NodeKind::Module => format!("{{fqn: {}, status: {status}}}", lit(fqn)),
            NodeKind::Struct | NodeKind::Function => {
                let (path, start, end, sl, el) = span(node);
                format!(
                    "{{fqn: {}, path: {}, start: {start}, end_pos: {end}, sline: {sl}, \
                     eline: {el}, ctype: {}, status: {status}}}",
                    lit(fqn),
                    lit(&path),
                    lit(&node.code_type),
                )
            }
            NodeKind::File => {
                let (_, _, _, sl, el) = span(node);
                format!(
                    "{{fqn: {}, sline: {sl}, eline: {el}, ctype: {}, status: {status}}}",
                    lit(fqn),
                    lit(&node.code_type),
                )
            }
            NodeKind::UnresolvedTarget => format!(
                "{{fqn: {}, category: {}}}",
                lit(fqn),
                lit(node.category.as_deref().unwrap_or(""))
            ),
            NodeKind::Language => format!("{{fqn: {}}}", lit(fqn)),
            _ => continue,
        });
    }
    if rows.is_empty() {
        return Ok((0, 0));
    }
    let list = rows.join(", ");
    let stmt = match label {
        // The `ON MATCH` branch is the planned-placeholder backstop in batched
        // form: a `planned` row leaves a realized (non-`planned`, non-NULL)
        // Module's status untouched; every other case sets the row's status
        // (the pre-fix `MERGE … SET n.status = …`).
        "Module" => format!(
            "UNWIND [{list}] AS row MERGE (n:Module {{fqn: row.fqn}}) \
             ON CREATE SET n.status = row.status \
             ON MATCH SET n.status = CASE WHEN row.status = 'planned' \
             AND (n.status IS NULL OR n.status <> 'planned') THEN n.status ELSE row.status END"
        ),
        "Struct" | "Function" => format!(
            "UNWIND [{list}] AS row MERGE (n:{label} {{fqn: row.fqn}}) \
             SET n.path = row.path, n.start = row.start, n.`end` = row.end_pos, \
             n.start_line = row.sline, n.end_line = row.eline, \
             n.code_type = row.ctype, n.status = row.status"
        ),
        "File" => format!(
            "UNWIND [{list}] AS row MERGE (n:File {{fqn: row.fqn}}) \
             SET n.start_line = row.sline, n.end_line = row.eline, \
             n.code_type = row.ctype, n.status = row.status"
        ),
        "UnresolvedTarget" => format!(
            "UNWIND [{list}] AS row MERGE (n:UnresolvedTarget {{fqn: row.fqn}}) \
             SET n.category = row.category"
        ),
        "Language" => format!("UNWIND [{list}] AS row MERGE (n:Language {{fqn: row.fqn}})"),
        _ => return Ok((0, 0)),
    };
    conn.query(&stmt)?;
    Ok((rows.len() as u64, 1))
}

/// The (path, start, end, start_line, end_line) location columns of a
/// Struct/Function, defaulted to empty/0 when it carries no location.
fn span(node: &crate::graph::Node) -> (String, u32, u32, u32, u32) {
    match node.location.as_ref() {
        Some(l) => (
            l.path.to_string_lossy().into_owned(),
            l.start,
            l.end,
            l.start_line,
            l.end_line,
        ),
        None => (String::new(), 0, 0, 0, 0),
    }
}

/// `DETACH DELETE`s every node of `label` in `fqns`.
fn detach_delete(conn: &Connection, label: &str, fqns: &[String]) -> anyhow::Result<()> {
    if fqns.is_empty() {
        return Ok(());
    }
    let set: BTreeSet<String> = fqns.iter().cloned().collect();
    conn.query(&format!(
        "MATCH (n:{label}) WHERE n.fqn IN [{}] DETACH DELETE n",
        literal_list(&set)
    ))?;
    Ok(())
}

/// Groups a labelled set by its label.
fn group_by_label(set: &BTreeMap<String, &'static str>) -> Vec<(&'static str, Vec<String>)> {
    let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (fqn, label) in set {
        groups.entry(label).or_default().push(fqn.clone());
    }
    groups.into_iter().collect()
}

/// Runs `RETURN count(*)` and returns the number (0 on a malformed result).
fn count(conn: &Connection, query: &str) -> anyhow::Result<i64> {
    let (_, rows) = query_rows(conn, query)?;
    Ok(rows
        .first()
        .and_then(|r| r.first())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0))
}
