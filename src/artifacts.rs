//! Write-through authoring (SPEC R5) and the DB helpers the `apg spec` /
//! `apg plan` / `apg review` CLIs share: opening the live `apg/.trans/db.lbug`
//! read-write, resolving FQNs against the code graph, and re-ingesting a
//! project's spec/plan/note records via Cypher MERGE. Mutations never rebuild
//! the DB — the code graph is untouched; only the project's `future/…` nodes
//! are detached and re-merged from its JSONL files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lbug::{Connection, Database, SystemConfig};

use crate::schema::Record;
use crate::specs;

pub struct ArtifactDb {
    pub db: Database,
}

/// True when `fqn` resolves to a node in the live graph (any kind).
fn node_exists(db: &Database, fqn: &str) -> bool {
    count(db, &format!("MATCH (n {{fqn: {}}}) RETURN count(*)", lit(fqn))) > 0
}

/// Runs `RETURN count(*)` and returns the number.
fn count(db: &Database, q: &str) -> i64 {
    Connection::new(db)
        .and_then(|c| c.query(q))
        .map(|r| {
            r.to_string()
                .lines()
                .last()
                .and_then(|l| l.trim().parse().ok())
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Single-quotes a value for a Cypher string literal (escapes `\`, `'`).
pub fn lit(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A tiny option parser for the spec/plan/review subcommands. Positional args
/// (not starting with `--`) and repeatable flags (`--flag value`, boolean when
/// no value follows).
pub struct ParsedArgs {
    pub positional: Vec<String>,
    pub flags: HashMap<String, Vec<String>>,
}

pub fn parse_args(args: &[String]) -> ParsedArgs {
    let mut positional = Vec::new();
    let mut flags: HashMap<String, Vec<String>> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(name) = args[i].strip_prefix("--") {
            let mut vals = Vec::new();
            while i + 1 < args.len() && !args[i + 1].starts_with("--") {
                vals.push(args[i + 1].clone());
                i += 1;
            }
            flags.entry(name.to_string()).or_default().extend(vals);
        } else {
            positional.push(args[i].clone());
        }
        i += 1;
    }
    ParsedArgs {
        positional,
        flags,
    }
}

impl ParsedArgs {
    /// All values of a repeatable flag (empty when absent).
    pub fn all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }
    /// The single value of a flag, or `None`.
    pub fn get(&self, name: &str) -> Option<String> {
        self.flags.get(name).and_then(|v| v.first().cloned())
    }
    /// True when a boolean flag is present.
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
}

impl ArtifactDb {
    pub fn open(apg_root: &Path) -> anyhow::Result<ArtifactDb> {
        let db_path = apg_root.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            anyhow::bail!(
                "{} does not exist — run `apg scan` first",
                db_path.display()
            );
        }
        let db = Database::new(&db_path, SystemConfig::default())?;
        Ok(ArtifactDb { db })
    }

    /// A fresh connection to the owned database (borrows `self`, so any
    /// returned rows must be consumed before the next call).
    pub fn conn(&self) -> anyhow::Result<Connection<'_>> {
        Ok(Connection::new(&self.db)?)
    }

    /// Runs a query and returns its formatted output.
    pub fn q(&self, query: &str) -> anyhow::Result<String> {
        Ok(self.conn()?.query(query)?.to_string())
    }

    pub fn has_node(&self, fqn: &str) -> bool {
        node_exists(&self.db, fqn)
    }

    /// The code-graph label of `fqn` (Function/Struct/File/Module/
    /// UnresolvedTarget), or `None` when it is not a code node.
    pub fn code_label(&self, fqn: &str) -> Option<&'static str> {
        for l in [
            "Function",
            "Struct",
            "File",
            "Module",
            "UnresolvedTarget",
        ] {
            if count(
                &self.db,
                &format!("MATCH (n:{l} {{fqn: {}}}) RETURN count(*)", lit(fqn)),
            ) > 0
            {
                return Some(l);
            }
        }
        None
    }

    /// True when `fqn` is a `Future` node (an explicit, author-declared
    /// placeholder; never auto-created at authoring time).
    pub fn is_future(&self, fqn: &str) -> bool {
        count(
            &self.db,
            &format!("MATCH (n:Future {{fqn: {}}}) RETURN count(*)", lit(fqn)),
        ) > 0
    }

    /// Resolves an anchor target (R7/R8): a resolved code FQN or an existing
    /// `future/…` FQN. Anything else is an error.
    pub fn resolve_anchor(&self, fqn: &str) -> anyhow::Result<()> {
        if self.code_label(fqn).is_some() || self.is_future(fqn) {
            Ok(())
        } else {
            anyhow::bail!(
                "anchor target `{fqn}` is neither a resolved code node nor an existing `future/…` FQN (declare future code with `apg spec add future` first)"
            )
        }
    }

    /// The owning module of a code node (via the Contains Module→File→node
    /// chain), for note-ledger routing.
    pub fn owning_module(&self, fqn: &str) -> Option<String> {
        // File targets sit directly under a module; structs/functions under a
        // file. Try the direct chain first, then the file-mediated one.
        let direct = "MATCH (m:Module)-[:Contains]->(n {fqn: X}) RETURN m.fqn";
        let via_file = "MATCH (m:Module)-[:Contains]->(:File)-[:Contains]->(n {fqn: X}) RETURN m.fqn";
        for q in [direct, via_file] {
            let q = q.replace("X", &lit(fqn));
            if let Ok(s) = self.q(&q) {
                let r = s;
                let s = r.to_string();
                if let Some(line) = s.lines().last() {
                    let line = line.trim();
                    if !line.is_empty() && line != "m.fqn" {
                        return Some(line.to_string());
                    }
                }
            }
        }
        None
    }

    /// The `apg/notes/<module>.jsonl` file a note on `target_fqn` routes to.
    pub fn note_file(&self, apg_root: &Path, target_fqn: &str) -> PathBuf {
        let stem = self
            .owning_module(target_fqn)
            .map(|m| m.replace('/', "_"))
            .unwrap_or_else(|| "_root".to_string());
        apg_root.join("notes").join(format!("{stem}.jsonl"))
    }

    /// Deletes every node with fqn `future/<project>/…` and its incident
    /// edges. Used to reset a project's spec/plan/feedback state before
    /// re-merging its JSONL (code nodes are untouched).
    pub fn detach_delete_project(&self, project: &str) -> anyhow::Result<()> {
        self.conn()?.query(&format!(
            "MATCH (n) WHERE n.fqn STARTS WITH {} DETACH DELETE n",
            lit(&format!("future/{project}/"))
        ))?;
        Ok(())
    }

    /// Re-merges one node record: `MERGE (n:Label {fqn}) SET props` (an
    /// upsert — idempotent, and updates props when the node pre-exists).
    /// `number` is the one INT64 column; every other prop is a string literal.
    fn merge_node(&self, label: &str, fqn: &str, props: &[(&str, String)]) -> anyhow::Result<()> {
        let set = props
            .iter()
            .map(|(k, v)| {
                if *k == "number" {
                    format!("n.{k} = {v}")
                } else {
                    format!("n.{k} = {}", lit(v))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.conn()?.query(&format!(
            "MERGE (n:{label} {{fqn: {}}}) SET {set}",
            lit(fqn)
        ))?;
        Ok(())
    }

    /// Re-merges one edge record. Endpoint labels come from `known` (nodes in
    /// this record set) or the code graph. A dangling endpoint is skipped.
    fn merge_edge(
        &self,
        rel_table: &str,
        from: &str,
        to: &str,
        known: &HashMap<String, &'static str>,
    ) -> anyhow::Result<()> {
        let la = known
            .get(from)
            .copied()
            .or_else(|| self.code_label(from));
        let lb = known.get(to).copied().or_else(|| self.code_label(to));
        if let (Some(a), Some(b)) = (la, lb) {
            // Two-variable MATCH + MERGE rel: the endpoints already exist (node
            // records merged above, or code nodes in the graph). The one-shot
            // pattern MERGE `(a:.. {fqn})-[:R]->(b:.. {fqn})` fails when the
            // endpoints pre-exist (it re-attempts their creation → PK clash).
            self.conn()?.query(&format!(
                "MATCH (a:{a} {{fqn: {}}}), (b:{b} {{fqn: {}}}) MERGE (a)-[:{rel_table}]->(b)",
                lit(from),
                lit(to)
            ))?;
        }
        Ok(())
    }

    /// Re-ingests a set of records into the live DB (write-through, R5): nodes
    /// first (upserts), then edges (endpoints resolved against the code graph
    /// or the node set being merged).
    pub fn merge_records(&self, records: &[Record]) -> anyhow::Result<()> {
        let mut known: HashMap<String, &'static str> = HashMap::new();
        for r in records {
            if let Some((label, fqn, props)) = node_merge(r) {
                known.insert(fqn.to_string(), label);
                self.merge_node(label, fqn, &props)?;
            }
        }
        for r in records {
            if let Some((table, from, to)) = edge_merge(r) {
                self.merge_edge(table, from, to, &known)?;
            }
        }
        Ok(())
    }
}

/// The node-table label and MERGE properties for a node record.
#[allow(clippy::type_complexity)]
fn node_merge(r: &Record) -> Option<(&'static str, &str, Vec<(&'static str, String)>)> {
    match r {
        Record::Spec { fqn, title, goal } => Some((
            "Spec",
            fqn,
            vec![("title", title.clone()), ("goal", goal.clone())],
        )),
        Record::Requirement {
            fqn,
            id,
            title,
            body,
            feature,
        } => Some((
            "Requirement",
            fqn,
            vec![
                ("id", id.clone()),
                ("title", title.clone()),
                ("body", body.clone()),
                ("feature", feature.clone()),
            ],
        )),
        Record::Phase { fqn, number, title } => Some((
            "Phase",
            fqn,
            vec![
                ("number", number.to_string()),
                ("title", title.clone()),
            ],
        )),
        Record::Decision { fqn, id, summary } => Some((
            "Decision",
            fqn,
            vec![("id", id.clone()), ("summary", summary.clone())],
        )),
        Record::Future { fqn, kind, target } => Some((
            "Future",
            fqn,
            vec![("kind", kind.clone()), ("target", target.clone())],
        )),
        Record::NonGoal { fqn, body } => {
            Some(("NonGoal", fqn, vec![("body", body.clone())]))
        }
        Record::AcceptanceCriterion { fqn, body } => Some((
            "AcceptanceCriterion",
            fqn,
            vec![("body", body.clone())],
        )),
        Record::VerificationItem { fqn, body } => Some((
            "VerificationItem",
            fqn,
            vec![("body", body.clone())],
        )),
        Record::Note { fqn, body, kind } => Some((
            "Note",
            fqn,
            vec![("body", body.clone()), ("kind", kind.clone())],
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
        )),
        Record::Plan { fqn, title, strategy } => Some((
            "Plan",
            fqn,
            vec![("title", title.clone()), ("strategy", strategy.clone())],
        )),
        Record::PlanPhase {
            fqn,
            number,
            title,
            deliverable,
        } => Some((
            "PlanPhase",
            fqn,
            vec![
                ("number", number.to_string()),
                ("title", title.clone()),
                ("deliverable", deliverable.clone()),
            ],
        )),
        Record::Task {
            fqn,
            title,
            tier,
            status,
        } => Some((
            "Task",
            fqn,
            vec![
                ("title", title.clone()),
                ("tier", tier.clone()),
                ("status", status.clone()),
            ],
        )),
        _ => None,
    }
}

/// The rel-table name and endpoints for an edge record.
fn edge_merge(r: &Record) -> Option<(&'static str, &str, &str)> {
    match r {
        Record::Contains { from, to } => Some(("Contains", from, to)),
        Record::Details { from, to } => Some(("Details", from, to)),
        Record::Reviews { from, to } => Some(("Reviews", from, to)),
        Record::DependsOn { from, to } => Some(("DependsOn", from, to)),
        Record::Gates { from, to } => Some(("Gates", from, to)),
        Record::SpecDepends { from, to } => Some(("SpecDependsOn", from, to)),
        Record::Anchors { from, to } => Some(("Anchors", from, to)),
        Record::Implements { from, to } => Some(("Implements", from, to)),
        Record::Satisfies { from, to } => Some(("Satisfies", from, to)),
        Record::Builds { from, to } => Some(("Builds", from, to)),
        _ => None,
    }
}

/// The fqn of a node record, if it is one.
pub fn node_fqn(r: &Record) -> Option<&str> {
    match r {
        Record::Spec { fqn, .. }
        | Record::Requirement { fqn, .. }
        | Record::Phase { fqn, .. }
        | Record::Decision { fqn, .. }
        | Record::Future { fqn, .. }
        | Record::NonGoal { fqn, .. }
        | Record::AcceptanceCriterion { fqn, .. }
        | Record::VerificationItem { fqn, .. }
        | Record::Note { fqn, .. }
        | Record::Feedback { fqn, .. }
        | Record::Plan { fqn, .. }
        | Record::PlanPhase { fqn, .. }
        | Record::Task { fqn, .. } => Some(fqn),
        _ => None,
    }
}

/// The endpoints of an edge record, if it is one.
pub fn edge_endpoints(r: &Record) -> Option<(&str, &str)> {
    match r {
        Record::Contains { from, to }
        | Record::Details { from, to }
        | Record::Reviews { from, to }
        | Record::DependsOn { from, to }
        | Record::Gates { from, to }
        | Record::SpecDepends { from, to }
        | Record::Anchors { from, to }
        | Record::Implements { from, to }
        | Record::Satisfies { from, to }
        | Record::Builds { from, to } => Some((from, to)),
        _ => None,
    }
}

/// Removes an existing node record with `fqn` plus every edge incident to it
/// (idempotent authoring: `add` upserts by id, `rm` removes node + edges).
pub fn remove_node(records: &mut Vec<Record>, fqn: &str) {
    records.retain(|r| match (node_fqn(r), edge_endpoints(r)) {
        (Some(n), _) => n != fqn,
        (None, Some((a, b))) => a != fqn && b != fqn,
        _ => true,
    });
}

/// Removes every edge record incident to `fqn`, leaving the node record.
pub fn remove_incident_edges(records: &mut Vec<Record>, fqn: &str) {
    records.retain(|r| match edge_endpoints(r) {
        Some((a, b)) => a != fqn && b != fqn,
        _ => true,
    });
}

/// Re-ingests a project's spec + plan records (and all committed notes) into
/// the live DB after a write-through mutation (R5). The project's `future/…`
/// state is detached and rebuilt from its JSONL; code nodes are untouched.
pub fn reingest_project(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    db.detach_delete_project(project)?;
    let mut records: Vec<Record> = Vec::new();
    let spec_path = specs::spec_jsonl_path(apg_root, project);
    if spec_path.exists() {
        records.extend(specs::read_jsonl(&spec_path)?);
    }
    let plan_path = specs::plan_jsonl_path(apg_root, project);
    if plan_path.exists() {
        records.extend(specs::read_jsonl(&plan_path)?);
    }
    for f in specs::jsonl_files(&apg_root.join("notes")) {
        records.extend(specs::read_jsonl(&f)?);
    }
    db.merge_records(&records)?;
    Ok(())
}

/// The next free `feedback-<n>` / `note-<n>` number for a project, scanning
/// the given records (fqn suffix after `feedback-`/`note-`).
pub fn next_free(records: &[Record], kind: &str) -> u64 {
    let prefix = format!("{kind}-");
    records
        .iter()
        .filter_map(|r| node_fqn(r))
        .filter_map(|f| {
            let (_, tail) = f.split_once("future/")?;
            let (_, suffix) = tail.split_once(&format!("/{prefix}"))?;
            suffix.parse::<u64>().ok()
        })
        .max()
        .map(|n| n + 1)
        .unwrap_or(1)
}