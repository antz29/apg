use std::collections::{BTreeSet, HashMap, HashSet};

use lbug::Connection;

use crate::layers::{NodeProperties, properties_json};
use crate::load;
use crate::schema::Record;

use super::db::{ArtifactDb, CODE_GRAPH_LABELS, PLANNED_CODE_LABELS, count, lit};

impl ArtifactDb {
    /// Deletes exactly the FQNs in `fqns`, together with their incident edges,
    /// guarded so a **realized** code FQN survives (phase-05 task-1).
    ///
    /// The name is retained from the pre-phase-05 prefix delete (`modifies`
    /// target `apg.artifacts.ArtifactDb.detach_delete_project`); the BEHAVIOR is
    /// the exact-FQN delta below, no longer a `<project>/`-prefix delete.
    ///
    /// The delete set a metadata mutation passes is its removed ∪ changed FQNs
    /// (see [`transient_delta`] and `layers::projection_deletes`), never a
    /// `<project>/` prefix: a planned code FQN carries no project prefix
    /// (`apg.session.Coordinator.start`), so a prefix delete leaves it behind.
    ///
    /// For the four Implementation labels the delete is guarded by
    /// `status = 'planned'`: after a branch scan realizes a planned FQN as real
    /// code there is no PlannedNode row left (the scan replaced it), so a naive
    /// `DETACH DELETE` by FQN would drop the REAL code node and its incident
    /// edges. Non-Implementation labels (Requirement, Plan, PlanPhase, Task,
    /// Feedback, …) delete by FQN alone — several legitimately carry their own
    /// `status` (`pending`/`done`/`open`/…), which must never be read as the
    /// code planned marker. `UnresolvedTarget`/`Scan` are code-graph nodes,
    /// never metadata, and are never touched.
    ///
    /// Runs on `conn` so the deletes share the caller's transaction. One query
    /// per label per FQN: the delta is small (a mutation touches a handful of
    /// FQNs), and a per-label delete is the only form that can apply the
    /// planned guard without a `labels(n)` predicate.
    pub fn detach_delete_project(
        &self,
        conn: &Connection,
        fqns: &BTreeSet<String>,
    ) -> anyhow::Result<()> {
        for fqn in fqns {
            for label in load::node_labels() {
                if CODE_GRAPH_LABELS.contains(label) {
                    continue;
                }
                conn.query(&format!(
                    "MATCH (n:{label}) WHERE n.fqn = {} DETACH DELETE n",
                    lit(fqn)
                ))?;
            }
            for label in PLANNED_CODE_LABELS {
                conn.query(&format!(
                    "MATCH (n:{label}) WHERE n.fqn = {} AND n.status = 'planned' DETACH DELETE n",
                    lit(fqn)
                ))?;
            }
        }
        Ok(())
    }

    /// Re-merges one node record: `MERGE (n:Label {fqn}) SET props` (an
    /// upsert — idempotent, and updates props when the node pre-exists).
    /// `number` is the one INT64 column; every other prop is a string literal.
    /// When `properties` is `Some` (a durable authored node record), the
    /// canonical JSON of its full properties map is written to the
    /// serialized-properties column — the same column the full-scan
    /// `build_load_files` writes — so the session's incremental reingest is as
    /// lossless as a fresh scan. Code and transient plan/feedback node records
    /// have no such column and pass `None`.
    /// Runs on `conn` so the write shares the caller's transaction.
    fn merge_node(
        &self,
        conn: &Connection,
        label: &str,
        fqn: &str,
        props: &[(&str, String)],
        properties: Option<&NodeProperties>,
    ) -> anyhow::Result<()> {
        let mut set = props
            .iter()
            .map(|(k, v)| {
                if *k == "number" {
                    format!("n.{k} = {v}")
                } else {
                    format!("n.{k} = {}", lit(v))
                }
            })
            .collect::<Vec<_>>();
        if let Some(properties) = properties {
            set.push(format!(
                "n.properties = {}",
                lit(&properties_json(properties))
            ));
        }
        let set = set.join(", ");
        conn.query(&format!(
            "MERGE (n:{label} {{fqn: {}}}) SET {set}",
            lit(fqn)
        ))?;
        Ok(())
    }

    /// Re-merges one edge record. Endpoint labels come from `known` (nodes in
    /// this record set) or the code graph. A dangling endpoint is skipped.
    /// Runs on `conn` so the write shares the caller's transaction.
    ///
    /// When `properties` is `Some` (a durable authored edge record), the
    /// canonical JSON of its full properties map is written to the rel table's
    /// serialized-properties column — both halves, since the pairing invariant
    /// stores one rel row — so the session's incremental reingest preserves
    /// edge properties exactly like a fresh scan's `build_load_files`. The
    /// transient `Reviews`/`Gates`/`Satisfies` rels have no such column and
    /// pass `None`.
    ///
    /// The schema-pair guard (R3/R4): the Cypher MERGE is issued only when the
    /// rel table actually declares this `(from, to)` label pair. LadybugDB
    /// throws a binder exception (`Query node b violates schema …`) for an
    /// undeclared pair — and because a re-ingest merges every project's
    /// records in ONE transaction, a single illegal pair anywhere (a legacy
    /// Note→Note Details edge, the R2 class) used to abort every
    /// write-through with an opaque binder error, even a perfectly legal
    /// note-add to another project (the cosanima-rename Spec/Decision mystery,
    /// R3). The scan load path already projects such pairs away
    /// (`build_load_files` buckets only declared pairs); the merge guard does
    /// the same, so the DB projection stays consistent with a fresh scan and a
    /// legal write-through is never hostage to unrelated poison records. The
    /// CLI-side `add_note` validation (R2) keeps new illegal pairs from being
    /// authored in the first place.
    fn merge_edge(
        &self,
        conn: &Connection,
        rel_table: &str,
        from: &str,
        to: &str,
        known: &HashMap<String, &'static str>,
        properties: Option<&NodeProperties>,
    ) -> anyhow::Result<()> {
        // Endpoint labels come from the record set being merged (`known`), the
        // code graph, or the live DB itself — a durable layer node (a
        // requirement, an entity, a constraint) persists across a transient
        // write-through (only `<project>/…` nodes are detached), so a Reviews
        // edge from `.trans` feedback to a durable node resolves its label
        // from the DB and merges (SPEC §5: feedback links to durable nodes
        // via Reviews edges).
        let la = known
            .get(from)
            .copied()
            .or_else(|| self.code_label(from))
            .or_else(|| self.node_label(from));
        let lb = known
            .get(to)
            .copied()
            .or_else(|| self.code_label(to))
            .or_else(|| self.node_label(to));
        if let (Some(a), Some(b)) = (la, lb) {
            if !rel_pair_allowed(rel_table, a, b) {
                return Ok(());
            }
            // Two-variable MATCH + MERGE rel: the endpoints already exist (node
            // records merged above, or code nodes in the graph). The one-shot
            // pattern MERGE `(a:.. {fqn})-[:R]->(b:.. {fqn})` fails when the
            // endpoints pre-exist (it re-attempts their creation → PK clash).
            // A durable authored edge binds the rel variable and SETs the
            // serialized-properties column; an absent (transient) column keeps
            // the plain two-endpoint MERGE.
            match properties {
                Some(properties) => conn.query(&format!(
                    "MATCH (a:{a} {{fqn: {}}}), (b:{b} {{fqn: {}}}) MERGE (a)-[r:{rel_table}]->(b) SET r.properties = {}",
                    lit(from),
                    lit(to),
                    lit(&properties_json(properties))
                ))?,
                None => conn.query(&format!(
                    "MATCH (a:{a} {{fqn: {}}}), (b:{b} {{fqn: {}}}) MERGE (a)-[:{rel_table}]->(b)",
                    lit(from),
                    lit(to)
                ))?,
            };
        }
        Ok(())
    }

    /// Re-ingests a set of records into the live DB (write-through, R5): nodes
    /// first (upserts), then edges (endpoints resolved against the code graph
    /// or the node set being merged). Every statement runs on `conn` so the
    /// caller can run the merge inside a single transaction — a failed edge
    /// merge then rolls back the node merges instead of leaving orphans.
    pub fn merge_records(&self, conn: &Connection, records: &[Record]) -> anyhow::Result<()> {
        // A PlannedNode record must never overwrite a **present** (realized)
        // Implementation node: the plan JSONL keeps its planned records until
        // apply, so a write-through AFTER a realization scan (a feedback
        // resolve, a plan note, a `plan done`) would otherwise re-mark the
        // scanned code `planned` and break the apply gate's realization
        // check. This mirrors the scanner-replace rule (ingest.rs): a present
        // node supersedes the planned record; the planned record is skipped.
        let realized: HashSet<String> = records
            .iter()
            .filter_map(|r| match r {
                Record::PlannedNode { fqn, .. } => Some(fqn.clone()),
                _ => None,
            })
            .filter(|fqn| {
                ["Function", "Struct", "File", "Module"].iter().any(|l| {
                    count(
                        &self.db,
                        &format!(
                            "MATCH (n:{l} {{fqn: {}}}) WHERE n.status IS NULL OR n.status <> 'planned' RETURN count(*)",
                            lit(fqn)
                        ),
                    ) > 0
                })
            })
            .collect();
        let mut known: HashMap<String, &'static str> = HashMap::new();
        for r in records {
            if let Some((label, fqn, props, properties)) = node_merge(r) {
                if matches!(r, Record::PlannedNode { .. }) && realized.contains(fqn) {
                    continue;
                }
                known.insert(fqn.to_string(), label);
                self.merge_node(conn, label, fqn, &props, properties)?;
            }
        }
        for r in records {
            if let Some((table, from, to, properties)) = edge_merge(r) {
                self.merge_edge(conn, table, from, to, &known, properties)?;
            }
        }
        Ok(())
    }
}

/// The node-table label and MERGE properties for a node record, plus the
/// serialized-properties column payload when the table has one.
///
/// The last element is `Some` for the durable authored node tables (which gain
/// the serialized-properties column, phase-4 task 12) and `None` for the code
/// node tables and the transient plan/feedback node tables, which have no such
/// column.
#[allow(clippy::type_complexity)]
fn node_merge(
    r: &Record,
) -> Option<(
    &'static str,
    &str,
    Vec<(&'static str, String)>,
    Option<&NodeProperties>,
)> {
    match r {
        Record::Requirement {
            fqn,
            id,
            title,
            body,
            feature,
            properties,
        } => Some((
            "Requirement",
            fqn,
            vec![
                ("id", id.clone()),
                ("title", title.clone()),
                ("body", body.clone()),
                ("feature", feature.clone()),
            ],
            Some(properties),
        )),
        Record::PlannedNode { fqn, kind, .. } => Some((
            match kind.as_str() {
                "module" => "Module",
                "file" => "File",
                "struct" => "Struct",
                "function" => "Function",
                other => {
                    panic!("planned_node kind must be module/file/struct/function, got `{other}`")
                }
            },
            fqn,
            vec![("status", "planned".to_string())],
            None,
        )),
        Record::Note {
            fqn,
            body,
            kind,
            properties,
        } => Some((
            "Note",
            fqn,
            vec![("body", body.clone()), ("kind", kind.clone())],
            Some(properties),
        )),
        Record::Feedback {
            fqn,
            body,
            status,
            disposition,
        } => Some((
            "Feedback",
            fqn,
            vec![
                ("body", body.clone()),
                ("status", status.clone()),
                ("disposition", disposition.clone()),
            ],
            None,
        )),
        Record::Plan {
            fqn,
            title,
            strategy,
        } => Some((
            "Plan",
            fqn,
            vec![("title", title.clone()), ("strategy", strategy.clone())],
            None,
        )),
        Record::PlanPhase {
            fqn,
            number,
            title,
            deliverable,
            status,
        } => Some((
            "PlanPhase",
            fqn,
            vec![
                ("number", number.to_string()),
                ("title", title.clone()),
                ("deliverable", deliverable.clone()),
                ("status", status.clone()),
            ],
            None,
        )),
        Record::Task {
            fqn,
            title,
            kind,
            tier,
            status,
            verb,
            target,
            new_fqn,
        } => Some((
            "Task",
            fqn,
            vec![
                ("title", title.clone()),
                ("kind", kind.clone()),
                ("tier", tier.clone()),
                ("status", status.clone()),
                ("verb", verb.clone()),
                ("target", target.clone()),
                ("new_fqn", new_fqn.clone()),
            ],
            None,
        )),
        Record::Stakeholder {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "Stakeholder",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::Entity {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "Entity",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::System {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "System",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::Container {
            fqn,
            name,
            kind,
            body,
            properties,
        } => Some((
            "Container",
            fqn,
            vec![
                ("name", name.clone()),
                ("kind", kind.clone()),
                ("body", body.clone()),
            ],
            Some(properties),
        )),
        Record::Component {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "Component",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::User {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "User",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::Group {
            fqn,
            name,
            attribute,
            root,
            body,
            properties,
        } => Some((
            "DomainGroup",
            fqn,
            vec![
                ("name", name.clone()),
                ("attribute", attribute.clone()),
                ("root", root.clone()),
                ("body", body.clone()),
            ],
            Some(properties),
        )),
        Record::Value {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "Value",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::Service {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "Service",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::Person {
            fqn,
            name,
            body,
            properties,
        } => Some((
            "Person",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
            Some(properties),
        )),
        Record::Constraint {
            fqn,
            name,
            body,
            attaches_to,
            properties,
        } => Some((
            "Constraint",
            fqn,
            vec![
                ("name", name.clone()),
                ("body", body.clone()),
                ("attaches_to", attaches_to.clone()),
            ],
            Some(properties),
        )),
        _ => None,
    }
}

/// The rel-table name and endpoints for an edge record, plus the rel's
/// serialized-properties column payload when the table has one.
///
/// `Calls`/`Uses` are shared code rel tables: the same record kind carries the
/// scanned Function→Function / Function→Struct / Struct→Struct edges and the
/// authored Service→Service `calls` / Person→System `uses` edges (the only
/// pairs `apg edge add` can author). The merge guard
/// ([`rel_pair_allowed`]) admits the authored pairs; the scanned pairs never
/// reach this merge (they come from the load path, not a record set).
///
/// The last element is `Some` for every durable authored rel table (the
/// serialized-properties column, phase-4 task 13) and `None` for the transient
/// `Reviews`/`Gates`/`Satisfies` rels, which carry no column.
pub fn edge_merge(r: &Record) -> Option<(&'static str, &str, &str, Option<&NodeProperties>)> {
    match r {
        Record::Contains {
            from,
            to,
            properties,
        } => Some(("Contains", from, to, Some(properties))),
        Record::Calls {
            from,
            to,
            properties,
        } => Some(("Calls", from, to, Some(properties))),
        Record::Uses {
            from,
            to,
            properties,
        } => Some(("Uses", from, to, Some(properties))),
        Record::Details {
            from,
            to,
            properties,
        } => Some(("Details", from, to, Some(properties))),
        Record::Reviews { from, to } => Some(("Reviews", from, to, None)),
        Record::DependsOn {
            from,
            to,
            properties,
        } => Some(("DependsOn", from, to, Some(properties))),
        Record::Gates { from, to } => Some(("Gates", from, to, None)),
        Record::Satisfies { from, to } => Some(("Satisfies", from, to, None)),
        Record::Drives {
            from,
            to,
            properties,
        } => Some(("Drives", from, to, Some(properties))),
        Record::Represents {
            from,
            to,
            properties,
        } => Some(("Represents", from, to, Some(properties))),
        Record::RealisedBy {
            from,
            to,
            properties,
        } => Some(("RealisedBy", from, to, Some(properties))),
        Record::SpecImplementedBy {
            from,
            to,
            properties,
        } => Some(("SpecImplementedBy", from, to, Some(properties))),
        Record::Publishes {
            from,
            to,
            properties,
        } => Some(("Publishes", from, to, Some(properties))),
        Record::Subscribes {
            from,
            to,
            properties,
        } => Some(("Subscribes", from, to, Some(properties))),
        _ => None,
    }
}

/// Whether the schema's rel table `table` declares an edge between the two
/// node labels. Consults [`load::rel_table_pairs`] — the same pair
/// enumeration that writes the load files and the `CREATE REL TABLE`
/// statements — so the merge guard cannot drift from the schema. An
/// undeclared pair is skipped (see `merge_edge`), never fed to LadybugDB as a
/// MERGE that would throw a binder exception.
pub(crate) fn rel_pair_allowed(table: &str, from: &str, to: &str) -> bool {
    load::rel_table_pairs()
        .iter()
        .any(|(t, a, b)| *t == table && *a == from && *b == to)
}

/// The fqn of a node record, if it is one.
pub fn node_fqn(r: &Record) -> Option<&str> {
    match r {
        Record::Requirement { fqn, .. }
        | Record::PlannedNode { fqn, .. }
        | Record::Note { fqn, .. }
        | Record::Feedback { fqn, .. }
        | Record::Plan { fqn, .. }
        | Record::PlanPhase { fqn, .. }
        | Record::Task { fqn, .. }
        | Record::Stakeholder { fqn, .. }
        | Record::Entity { fqn, .. }
        | Record::System { fqn, .. }
        | Record::Container { fqn, .. }
        | Record::Component { fqn, .. }
        | Record::User { fqn, .. }
        | Record::Group { fqn, .. }
        | Record::Value { fqn, .. }
        | Record::Service { fqn, .. }
        | Record::Person { fqn, .. }
        | Record::Constraint { fqn, .. } => Some(fqn),
        _ => None,
    }
}

/// The DB label and FQN of a node record — the exact pair [`merge_records`]
/// MERGEs. The projection-equals-sources check (phase-05 task-9) uses it to
/// derive the expected node set from the source record stream.
pub fn node_label_fqn(r: &Record) -> Option<(&'static str, &str)> {
    node_merge(r).map(|(label, fqn, _, _)| (label, fqn))
}

/// The endpoints of an edge record, if it is one.
// Kept as a shared record-rewrite primitive: its production callers are the
// incident-edge-stripping paths (`remove_node`), currently exercised by the
// test suite (the strict plan add surface no longer upserts).
#[allow(dead_code)]
pub fn edge_endpoints(r: &Record) -> Option<(&str, &str)> {
    match r {
        Record::Contains { from, to, .. }
        | Record::Calls { from, to, .. }
        | Record::Uses { from, to, .. }
        | Record::Details { from, to, .. }
        | Record::Reviews { from, to }
        | Record::DependsOn { from, to, .. }
        | Record::Gates { from, to }
        | Record::Satisfies { from, to }
        | Record::Drives { from, to, .. }
        | Record::Represents { from, to, .. }
        | Record::RealisedBy { from, to, .. }
        | Record::SpecImplementedBy { from, to, .. }
        | Record::Publishes { from, to, .. }
        | Record::Subscribes { from, to, .. } => Some((from, to)),
        _ => None,
    }
}

/// Removes an existing node record with `fqn` plus every edge incident to it
/// (idempotent authoring: `add` upserts by id, `rm` removes node + edges).
// Retained as the shared incident-edge-stripping primitive for the record
// surface (the plan rm cascade will reuse it); the strict add/update surface
// no longer upserts, so only the test suite drives it today.
#[allow(dead_code)]
pub fn remove_node(records: &mut Vec<Record>, fqn: &str) {
    records.retain(|r| match (node_fqn(r), edge_endpoints(r)) {
        (Some(n), _) => n != fqn,
        (None, Some((a, b))) => a != fqn && b != fqn,
        _ => true,
    });
}

/// The path `to → … → from` that would close a dependency cycle if the edge
/// `from → to` were added over the edges selected by `is_edge`, or `None` if
/// the graph stays acyclic. Used to reject requirement DependsOn and phase
/// Gates cycles at write time — a requirement that (transitively) depends on
/// itself makes "delivered when its dependencies are delivered" circular.
pub fn cycle_closing_path(
    records: &[Record],
    from: &str,
    to: &str,
    mut is_edge: impl FnMut(&Record) -> Option<(&str, &str)>,
) -> Option<Vec<String>> {
    if from == to {
        return Some(vec![from.to_string()]);
    }
    // BFS from `to`, following selected edges, looking for `from`.
    let mut parent: HashMap<&str, &str> = HashMap::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(to);
    while let Some(cur) = queue.pop_front() {
        for r in records {
            if let Some((a, b)) = is_edge(r)
                && a == cur
                && !parent.contains_key(b)
            {
                parent.insert(b, cur);
                queue.push_back(b);
            }
        }
    }
    if !parent.contains_key(from) {
        return None;
    }
    // Reconstruct `from → … → to`, then reverse to `to → … → from`.
    let mut path = vec![from.to_string()];
    let mut cur = from;
    while cur != to {
        let prev = parent.get(cur)?;
        path.push(prev.to_string());
        cur = prev;
    }
    path.reverse();
    Some(path)
}
