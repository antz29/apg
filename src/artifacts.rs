//! Write-through authoring (SPEC R5) and the DB helpers the `apg spec` /
//! `apg plan` / `apg review` CLIs share: opening the live `apg/.trans/db.lbug`
//! read-write, resolving FQNs against the code graph, and re-ingesting a
//! project's spec/plan/note records via Cypher MERGE. Mutations never rebuild
//! the DB — the code graph is untouched; only the project's `future/…` nodes
//! are detached and re-merged from its JSONL files.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use lbug::{Connection, Database, SystemConfig};

use crate::git;
use crate::load;
use crate::schema::Record;
use crate::specs;

/// The process-wide reentrant spec/plan/review write lock (see
/// `acquire_spec_lock`). Held for the life of the process, so the flock is
/// released when the CLI process exits.
static SPEC_LOCK: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// Acquires the exclusive cross-process lock that serializes spec/plan/review
/// read-modify-writes on the project's JSONL + DB. Called at the top of every
/// mutating command; the lock spans the whole load → modify → write_through
/// sequence, so concurrent tool calls (an agent issuing a batch in parallel)
/// never lose each other's edges. Reentrant within the process — a second
/// acquire is a no-op — so a command that loads both the plan and the spec
/// (plan complete) does not deadlock. The flock lives on `apg/.trans/specs.lock`
/// and is released at process exit.
pub fn acquire_spec_lock(apg_root: &Path) -> anyhow::Result<()> {
    let holder = SPEC_LOCK.get_or_init(|| Mutex::new(None));
    let mut held = holder.lock().unwrap();
    if held.is_some() {
        return Ok(());
    }
    let trans = apg_root.join(".trans");
    std::fs::create_dir_all(&trans)?;
    let lock_path = trans.join("specs.lock");
    let f = File::create(&lock_path)?;
    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&f), libc::LOCK_EX) };
        if rc != 0 {
            anyhow::bail!("could not acquire write lock {}", lock_path.display());
        }
    }
    *held = Some(f);
    Ok(())
}

/// Writes `records` to `path` and re-ingests the project into the live DB.
/// A missing DB (no scan yet) is not an error — the JSONL is the durable
/// form — but a re-ingest failure when the DB exists is a hard error: the
/// mutation must be visible in the query index, and silently dropping it is
/// what let the authoring agents believe writes had landed when they had not.
///
/// Refuse-on-stale gate (agent-loop hardening): when a DB exists **and** the
/// DB is stale (`is_stale` — the tree moved on since the scan that built it),
/// the mutation bails *before* any JSONL write or re-ingest. Every spec/plan/
/// review mutation funnels through here, so the single check covers them all.
/// Missing-DB and non-git paths stay allowed.
///
/// Atomic by design (D1): the JSONL is never committed before the DB merge
/// succeeds. The new records go to a sibling temp file first, the re-ingest
/// runs against the live DB from the in-memory `records` (the committed file
/// still holds the old content), and only then is the temp atomically renamed
/// over `path` — a single commit point for the durable JSONL and the query
/// index. On failure the temp is removed and the committed JSONL is untouched.
pub fn write_jsonl_and_reingest(
    apg_root: &Path,
    path: &Path,
    project: &str,
    records: &[Record],
) -> anyhow::Result<()> {
    if apg_root.join(specs::TRANS).join("db.lbug").exists() {
        if let Some(msg) = git::refusal_message(apg_root) {
            anyhow::bail!("{msg}");
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        specs::write_jsonl(&tmp, records)?;
        match reingest_project_with(apg_root, project, Some((path, records))) {
            Ok(()) => {
                std::fs::rename(&tmp, path)?;
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    } else {
        specs::write_jsonl(path, records)
    }
}

pub struct ArtifactDb {
    pub db: Database,
}

/// True when `fqn` resolves to a node in the live graph (any kind).
fn node_exists(db: &Database, fqn: &str) -> bool {
    count(
        db,
        &format!("MATCH (n {{fqn: {}}}) RETURN count(*)", lit(fqn)),
    ) > 0
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
    ParsedArgs { positional, flags }
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
        for l in ["Function", "Struct", "File", "Module", "UnresolvedTarget"] {
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

    /// The DB label of any node — code or spec/plan — or `None` when `fqn`
    /// does not resolve. Unlike [`code_label`](Self::code_label) (code tables
    /// only), this covers every node table; `add_note` uses it to decide
    /// whether a `--on` target is an allowable `Details` target (R2).
    pub fn node_label(&self, fqn: &str) -> Option<&'static str> {
        load::node_labels().iter().copied().find(|l| {
            count(
                &self.db,
                &format!("MATCH (n:{l} {{fqn: {}}}) RETURN count(*)", lit(fqn)),
            ) > 0
        })
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
        let via_file =
            "MATCH (m:Module)-[:Contains]->(:File)-[:Contains]->(n {fqn: X}) RETURN m.fqn";
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
    /// re-merging its JSONL (code nodes are untouched). Runs on `conn` so the
    /// deletion shares the caller's transaction.
    pub fn detach_delete_project(&self, conn: &Connection, project: &str) -> anyhow::Result<()> {
        conn.query(&format!(
            "MATCH (n) WHERE n.fqn STARTS WITH {} DETACH DELETE n",
            lit(&format!("future/{project}/"))
        ))?;
        Ok(())
    }

    /// Re-merges one node record: `MERGE (n:Label {fqn}) SET props` (an
    /// upsert — idempotent, and updates props when the node pre-exists).
    /// `number` is the one INT64 column; every other prop is a string literal.
    /// Runs on `conn` so the write shares the caller's transaction.
    fn merge_node(
        &self,
        conn: &Connection,
        label: &str,
        fqn: &str,
        props: &[(&str, String)],
    ) -> anyhow::Result<()> {
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
    /// The schema-pair guard (R3/R4): the Cypher MERGE is issued only when the
    /// rel table actually declares this `(from, to)` label pair. LadybugDB
    /// throws a binder exception (`Query node b violates schema …`) for an
    /// undeclared pair — and because a re-ingest merges every project's
    /// records in ONE transaction, a single illegal pair anywhere (a legacy
    /// Note→Future / Note→Note Details edge, the R2 class) used to abort every
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
    ) -> anyhow::Result<()> {
        let la = known.get(from).copied().or_else(|| self.code_label(from));
        let lb = known.get(to).copied().or_else(|| self.code_label(to));
        if let (Some(a), Some(b)) = (la, lb) {
            if !rel_pair_allowed(rel_table, a, b) {
                return Ok(());
            }
            // Two-variable MATCH + MERGE rel: the endpoints already exist (node
            // records merged above, or code nodes in the graph). The one-shot
            // pattern MERGE `(a:.. {fqn})-[:R]->(b:.. {fqn})` fails when the
            // endpoints pre-exist (it re-attempts their creation → PK clash).
            conn.query(&format!(
                "MATCH (a:{a} {{fqn: {}}}), (b:{b} {{fqn: {}}}) MERGE (a)-[:{rel_table}]->(b)",
                lit(from),
                lit(to)
            ))?;
        }
        Ok(())
    }

    /// Re-ingests a set of records into the live DB (write-through, R5): nodes
    /// first (upserts), then edges (endpoints resolved against the code graph
    /// or the node set being merged). Every statement runs on `conn` so the
    /// caller can run the merge inside a single transaction — a failed edge
    /// merge then rolls back the node merges instead of leaving orphans.
    pub fn merge_records(&self, conn: &Connection, records: &[Record]) -> anyhow::Result<()> {
        let mut known: HashMap<String, &'static str> = HashMap::new();
        for r in records {
            if let Some((label, fqn, props)) = node_merge(r) {
                known.insert(fqn.to_string(), label);
                self.merge_node(conn, label, fqn, &props)?;
            }
        }
        for r in records {
            if let Some((table, from, to)) = edge_merge(r) {
                self.merge_edge(conn, table, from, to, &known)?;
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
            vec![("number", number.to_string()), ("title", title.clone())],
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
        Record::NonGoal { fqn, body } => Some(("NonGoal", fqn, vec![("body", body.clone())])),
        Record::AcceptanceCriterion { fqn, body } => {
            Some(("AcceptanceCriterion", fqn, vec![("body", body.clone())]))
        }
        Record::VerificationItem { fqn, body } => {
            Some(("VerificationItem", fqn, vec![("body", body.clone())]))
        }
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
        Record::Plan {
            fqn,
            title,
            strategy,
        } => Some((
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
            kind,
            tier,
            status,
        } => Some((
            "Task",
            fqn,
            vec![
                ("title", title.clone()),
                ("kind", kind.clone()),
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

/// Whether the schema's rel table `table` declares an edge between the two
/// node labels. Consults [`load::rel_table_pairs`] — the same pair
/// enumeration that writes the load files and the `CREATE REL TABLE`
/// statements — so the merge guard cannot drift from the schema. An
/// undeclared pair is skipped (see `merge_edge`), never fed to LadybugDB as a
/// MERGE that would throw a binder exception.
fn rel_pair_allowed(table: &str, from: &str, to: &str) -> bool {
    load::rel_table_pairs()
        .iter()
        .any(|(t, a, b)| *t == table && *a == from && *b == to)
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

/// Re-ingests a project's spec + plan records (and all committed notes) into
/// the live DB after a write-through mutation (R5). The project's `future/…`
/// state is detached and rebuilt from its JSONL; code nodes are untouched.
///
/// Every spec project's records are merged, not just `project`'s: spec graphs
/// form one merged space, and a cross-project `DependsOn`/`SpecDependsOn` edge
/// targets a node living in another project's JSONL — re-ingesting only this
/// project's records would leave that endpoint out of `known` and the edge
/// silently skipped. Node merges are idempotent upserts, so the extra projects
/// are a no-op cost.
///
/// The detach + merge runs inside one transaction: a failed edge merge aborts
/// it, so the DB is never left partially re-merged (no orphan nodes) — the
/// prior committed state is preserved.
pub fn reingest_project(apg_root: &Path, project: &str) -> anyhow::Result<()> {
    reingest_project_with(apg_root, project, None)
}

/// Re-ingests with an optional substitute record set for one file `path`: the
/// in-memory `records` a write-through is about to commit. This lets the
/// re-ingest run BEFORE the new records hit the committed JSONL — the durable
/// file is only swapped in after the merge succeeds (D1), so a re-ingest
/// failure leaves the committed JSONL and the live DB both on the old state.
fn reingest_project_with(
    apg_root: &Path,
    project: &str,
    substitute: Option<(&Path, &[Record])>,
) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    let conn = db.conn()?;
    conn.query("BEGIN TRANSACTION")?;
    let result = (|| -> anyhow::Result<()> {
        db.detach_delete_project(&conn, project)?;
        let records = assembled_records(apg_root, project, substitute)?;
        db.merge_records(&conn, &records)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.query("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            // A failed query aborts the write transaction in the engine; roll
            // back so the DB keeps its prior committed state (no residue from
            // the failed mutation). The transaction may already be gone.
            let _ = conn.query("ROLLBACK");
            Err(e)
        }
    }
}

/// Assembles the full record set a re-ingest merges: every spec project's
/// JSONL (spec graphs form one merged space — see `reingest_project`), plus
/// `project`'s plan JSONL and every committed note ledger. When `substitute`
/// names a file, that file contributes `records` instead of its on-disk
/// content (write-through re-ingests the in-memory records before they are
/// committed). A substituted spec/plan file that is not yet on disk (first
/// write) still contributes its records.
fn assembled_records(
    apg_root: &Path,
    project: &str,
    substitute: Option<(&Path, &[Record])>,
) -> anyhow::Result<Vec<Record>> {
    let (sub_path, sub_records) = match substitute {
        Some((p, r)) => (Some(p), r),
        None => (None, &[][..]),
    };
    let specs_dir = apg_root.join("specs");
    let plan_path = specs::plan_jsonl_path(apg_root, project);

    let mut records: Vec<Record> = Vec::new();

    // Specs: every committed spec JSONL, with the substituted file replaced.
    let sub_is_spec = sub_path.is_some_and(|p| p.starts_with(&specs_dir));
    let mut subbed = false;
    for f in specs::jsonl_files(&specs_dir) {
        if sub_is_spec && sub_path == Some(f.as_path()) {
            records.extend_from_slice(sub_records);
            subbed = true;
        } else {
            records.extend(specs::read_jsonl(&f)?);
        }
    }
    if sub_is_spec && !subbed {
        records.extend_from_slice(sub_records);
    }

    // The project's plan (transient; read from disk unless substituted).
    let sub_is_plan = sub_path == Some(plan_path.as_path());
    if sub_is_plan || plan_path.exists() {
        if sub_is_plan {
            records.extend_from_slice(sub_records);
        } else {
            records.extend(specs::read_jsonl(&plan_path)?);
        }
    }

    // Committed note ledgers.
    for f in specs::jsonl_files(&apg_root.join("notes")) {
        if sub_path == Some(f.as_path()) {
            records.extend_from_slice(sub_records);
        } else {
            records.extend(specs::read_jsonl(&f)?);
        }
    }
    Ok(records)
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

/// The next free `annotations/<stem>/<n>` number for a code-note ledger,
/// scanning the ledger's own records. Each per-module ledger is its own
/// namespace, so note FQNs never collide across modules (a flat `annotations/N`
/// numbering would collide as soon as two modules each carry their first note).
pub fn next_free_annotation(records: &[Record], stem: &str) -> u64 {
    let prefix = format!("annotations/{stem}/");
    records
        .iter()
        .filter_map(|r| node_fqn(r))
        .filter_map(|f| f.strip_prefix(&prefix).map(str::to_owned))
        .filter_map(|s| s.parse::<u64>().ok())
        .max()
        .map(|n| n + 1)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::load;

    /// A temp `apg/` layout with a real `apg/.trans/db.lbug` holding a code
    /// graph (mirrors the spec_cmd fixture).
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("apg-artifacts-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        db_at(&dir);
        (dir.join("apg"), dir)
    }

    /// Builds a real DB + load files under `dir/apg` (used by `fixture` and by
    /// the git-aware staleness tests, which init a repo around the same
    /// layout first).
    fn db_at(dir: &Path) {
        std::fs::create_dir_all(dir.join("apg").join(specs::TRANS)).unwrap();
        std::fs::create_dir_all(dir.join("apg").join("specs")).unwrap();
        let mut g = Graph::default();
        g.nodes.insert(
            "github.com/x/y".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes.insert(
            "/abs/store.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: "/abs/store.go".into(),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 100,
                }),
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        g.nodes.insert(
            "github.com/x/y.Store".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: "/abs/store.go".into(),
                    start: 0,
                    end: 40,
                    start_line: 1,
                    end_line: 40,
                }),
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        g.contains
            .insert(("github.com/x/y".to_string(), "/abs/store.go".to_string()));
        g.contains.insert((
            "/abs/store.go".to_string(),
            "github.com/x/y.Store".to_string(),
        ));
        let ldir = dir.join("apg").join(specs::TRANS).join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        load::build_load_files(&g, &ldir).unwrap();
        let db = Database::new(
            dir.join("apg").join(specs::TRANS).join("db.lbug"),
            Default::default(),
        )
        .unwrap();
        let conn = Connection::new(&db).unwrap();
        load::create_schema(&conn).unwrap();
        load::copy_from(&conn, &ldir).unwrap();
        drop(conn);
        drop(db);
    }

    /// A temp dir with a real DB fixture inside a fresh git repo whose
    /// `apg/.trans/` is gitignored (so building the DB and writing graph.jsonl
    /// does not dirty the tree). Returns `(apg_root, dir, head_sha)`.
    fn git_fixture(name: &str) -> (PathBuf, PathBuf, String) {
        let dir =
            std::env::temp_dir().join(format!("apg-artifacts-git-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git_ok = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git_ok(&["init", "-q"]);
        git_ok(&["config", "user.email", "apg-test@example.com"]);
        git_ok(&["config", "user.name", "apg test"]);
        std::fs::write(dir.join(".gitignore"), "apg/.trans/\n").unwrap();
        git_ok(&["add", ".gitignore"]);
        git_ok(&["commit", "-q", "-m", "init"]);
        let sha = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&dir)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        db_at(&dir);
        (dir.join("apg"), dir, sha)
    }

    /// Writes a graph.jsonl whose line 1 records a scan at `sha`/`clean`
    /// (via the real export writer).
    fn write_scan_meta(apg_root: &Path, sha: &str, clean: bool) {
        let mut g = Graph::default();
        g.nodes.insert(
            crate::schema::SCAN_HEAD.to_string(),
            Node {
                kind: NodeKind::Scan,
                git_sha: Some(sha.to_string()),
                git_clean: Some(clean),
                scanned_at: Some("2026-09-07T00:00:00Z".to_string()),
                ..Node::default()
            },
        );
        load::write_graph_jsonl(&g, &apg_root.join(specs::TRANS).join("graph.jsonl")).unwrap();
    }

    /// The committed baseline records for the `foo` project: a spec with one
    /// requirement and one healthy note (Details → Spec).
    fn baseline_records() -> Vec<Record> {
        vec![
            Record::Spec {
                fqn: "future/foo/spec".into(),
                title: "Foo".into(),
                goal: "G".into(),
            },
            Record::Requirement {
                fqn: "future/foo/spec.R1".into(),
                id: "R1".into(),
                title: "Timer".into(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "future/foo/spec".into(),
                to: "future/foo/spec.R1".into(),
            },
            Record::Note {
                fqn: "future/foo/note-1".into(),
                body: "first".into(),
                kind: "background".into(),
            },
            Record::Details {
                from: "future/foo/note-1".into(),
                to: "future/foo/spec".into(),
            },
        ]
    }

    /// Note nodes with no incident `Details` edge — the orphan residue a failed
    /// write-through used to leave behind.
    fn orphan_notes(db: &ArtifactDb) -> i64 {
        let total = count(&db.db, "MATCH (n:Note) RETURN count(*)");
        let with_edge = count(
            &db.db,
            "MATCH (n:Note)-[:Details]->() RETURN count(DISTINCT n)",
        );
        total - with_edge
    }

    #[test]
    fn illegal_details_pair_is_projected_away_not_a_binder_error() {
        let (apg_root, dir) = fixture("orphan");
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();

        // A healthy committed state, write-through.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("future/foo/spec"));
            assert!(db.has_node("future/foo/note-1"));
            assert_eq!(orphan_notes(&db), 0);
        }

        // A note whose Details edge targets another Note — a pair the Details
        // rel table does NOT declare. Pre-R4 this made merge_edge throw a
        // LadybugDB binder exception mid-merge ("Query node b violates
        // schema"), which aborted the whole write-through and is the exact
        // failure class behind the cosanima-rename Spec/Decision mystery (R3).
        // The R4 schema-pair guard projects the illegal pair away — the same
        // bucketing the scan load path applies — so the write-through
        // succeeds, the JSONL is committed, and the DB projection matches what
        // a fresh scan would produce (the note node lands, the impossible edge
        // never materializes). The CLI-side add_note validation (R2) is what
        // keeps such records from being authored in the first place.
        let mut mutated = baseline.clone();
        mutated.push(Record::Note {
            fqn: "future/foo/note-2".into(),
            body: "poison".into(),
            kind: "background".into(),
        });
        mutated.push(Record::Details {
            from: "future/foo/note-2".into(),
            to: "future/foo/note-1".into(),
        });
        write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap();

        // The JSONL committed (the note record is durable truth).
        assert_eq!(specs::read_jsonl(&path).unwrap(), mutated);
        // No temp residue next to the committed JSONL.
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join("specs"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        // The DB projection: the note node lands, the undeclared Details edge
        // does not (identical to the scan load path's pair bucketing).
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("future/foo/note-2"));
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'future/foo/note-2'})-[:Details]->() RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("0"),
            "illegal pair must be projected away: {out}"
        );
        // The prior healthy edge survived the re-merge.
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'future/foo/note-1'})-[:Details]->(s:Spec) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "healthy edge: {out}");
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_through_is_atomic_when_reingest_fails_on_malformed_peer_file() {
        // The Phase-1 atomicity guarantee, with a genuine re-ingest failure
        // (the R2/R4 guards removed the binder-error injector): a malformed
        // peer spec file makes the re-ingest fail BEFORE any DB write, so the
        // committed JSONL and the live DB must both stay on the old state.
        let (apg_root, dir) = fixture("orphan-peer");
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();

        // A peer project's spec file goes malformed (a line that is not a
        // Record). Every re-ingest merges all projects' files, so this poisons
        // the assembled record set with a read error.
        let peer = specs::spec_jsonl_path(&apg_root, "peer");
        std::fs::write(&peer, "{\"type\":\"spec\",\"fqn\":").unwrap();

        // A legal note-add to foo now fails at the re-ingest read step.
        let mut mutated = baseline.clone();
        mutated.push(Record::Note {
            fqn: "future/foo/note-2".into(),
            body: "n".into(),
            kind: "background".into(),
        });
        mutated.push(Record::Details {
            from: "future/foo/note-2".into(),
            to: "future/foo/spec".into(),
        });
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap_err();
        assert!(
            format!("{err:#}").contains("bad record"),
            "expected the peer-file read error, got: {err:#}"
        );

        // (a) The committed JSONL still holds the OLD records; no temp residue.
        assert_eq!(specs::read_jsonl(&path).unwrap(), baseline);
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join("specs"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        // (b) The failed mutation left no residue in the live DB.
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(!db.has_node("future/foo/note-2"));
        assert_eq!(orphan_notes(&db), 0);
        assert!(db.has_node("future/foo/spec"));
        assert!(db.has_node("future/foo/note-1"));
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poison_details_edge_in_one_project_aborts_legal_write_through_elsewhere() {
        // R3 controlled reproduction (task 2.3). The mystery: adding a note
        // targeting `future/cosanima-rename/spec` (Spec) and
        // `future/cosanima-rename/spec.decision-D1` (Decision) threw a
        // LadybugDB binder exception, while IDENTICAL-label targets in other
        // projects (cosanima-mcp spec, cosanima-1.0 decision-D5) succeeded.
        //
        // The trigger is NOT the target label — Spec/Decision are legal
        // `Details` targets, and the `MATCH … MERGE` for them binds fine. The
        // re-ingest merges EVERY spec project's records in ONE transaction
        // (assembled_records + merge_records), so a single illegal edge pair
        // anywhere in the merged set — a Note → Future / Note → Note Details
        // edge, the R2 class — makes the binder throw and aborts the WHOLE
        // write-through, including perfectly legal note-adds to other projects.
        // PRE-FIX this test fails with the binder exception on the `docs`
        // write-through below; POST-FIX the illegal pair is skipped exactly
        // like the scan load path projects it away, and the legal note lands.
        let (apg_root, dir) = fixture("poison");

        // Project "rename" carries the legal targets from the mystery.
        let rename = vec![
            Record::Spec {
                fqn: "future/rename/spec".into(),
                title: "Rename".into(),
                goal: String::new(),
            },
            Record::Decision {
                fqn: "future/rename/spec.decision-D1".into(),
                id: "D1".into(),
                summary: "rename now".into(),
            },
            Record::Contains {
                from: "future/rename/spec".into(),
                to: "future/rename/spec.decision-D1".into(),
            },
        ];
        let rename_path = specs::spec_jsonl_path(&apg_root, "rename");
        write_jsonl_and_reingest(&apg_root, &rename_path, "rename", &rename).unwrap();

        // Project "docs" carries a POISON record: a Details edge whose
        // (Note, Future) pair the Details rel table does not declare. Such a
        // record cannot exist in the DB schema, so the merge_edge MERGE throws
        // a binder exception. The scan load path buckets the pair away
        // silently; the write-through re-ingest fed it to the DB.
        let docs = vec![
            Record::Spec {
                fqn: "future/docs/spec".into(),
                title: "Docs".into(),
                goal: String::new(),
            },
            Record::Future {
                fqn: "future/docs/migration-note".into(),
                kind: "other".into(),
                target: String::new(),
            },
            Record::Note {
                fqn: "future/docs/note-1".into(),
                body: "poison".into(),
                kind: "background".into(),
            },
            Record::Details {
                from: "future/docs/note-1".into(),
                to: "future/docs/migration-note".into(),
            },
        ];
        let docs_path = specs::spec_jsonl_path(&apg_root, "docs");
        // PRE-FIX (the R3 reproduction): this throws
        // `Binder exception: …` and, because the docs record set rides along
        // in every re-ingest, it poisoned note-adds to ANY project.
        write_jsonl_and_reingest(&apg_root, &docs_path, "docs", &docs).unwrap();

        // The legal note-add to rename must not be hostage to docs' poison:
        // `apg spec add rename note --on future/rename/spec`.
        let mut mutated = rename.clone();
        mutated.push(Record::Note {
            fqn: "future/rename/note-1".into(),
            body: "legal note".into(),
            kind: "background".into(),
        });
        mutated.push(Record::Details {
            from: "future/rename/note-1".into(),
            to: "future/rename/spec".into(),
        });
        write_jsonl_and_reingest(&apg_root, &rename_path, "rename", &mutated).unwrap();

        // The legal note landed in the JSONL and the live DB with its edge.
        assert!(specs::read_jsonl(&rename_path).unwrap().iter().any(|r| {
            matches!(r, Record::Details { from, to }
                if from == "future/rename/note-1" && to == "future/rename/spec")
        }));
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("future/rename/note-1"));
        let out = db
            .conn()
            .unwrap()
            .query(
                "MATCH (:Note {fqn: 'future/rename/note-1'})-[:Details]->(s:Spec) RETURN count(*)",
            )
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("1"),
            "legal details edge must land: {out}"
        );

        // The poison edge is projected away (the scan load path does the same
        // bucketing), so no binder error ever escapes — but the poison note's
        // edge must NOT be fabricated into the DB either.
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'future/docs/note-1'})-[:Details]->() RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(
            out.lines().last() == Some("0"),
            "poison edge must not materialize: {out}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_committed_state_attaches_notes_to_rename_spec_and_decision() {
        // R3 "why it now succeeds": the current committed spec JSONLs are
        // clean — the poison records that polluted the merged record set
        // during the 1.0 authoring loop were removed from the durable files
        // (their residues still sit in the live DB as edge-less orphans:
        // future/cosanima-1.0/note-7 and future/cosanima-docs/note-6). Run the
        // exact mystery commands against the committed state on a scratch
        // root: a note on `future/cosanima-rename/spec` (Spec) and on
        // `future/cosanima-rename/spec.decision-D1` (Decision) writes through
        // without a binder error.
        let (apg_root, dir) = fixture("rename-current");

        // Copy the repo's committed spec JSONLs into the scratch root. cargo
        // test runs from the crate root, where `apg/specs/` lives.
        let repo_specs = Path::new("apg/specs");
        assert!(repo_specs.is_dir(), "committed apg/specs must exist");
        for f in specs::jsonl_files(repo_specs) {
            let recs = specs::read_jsonl(&f).unwrap();
            let name = f.file_name().unwrap().to_owned();
            specs::write_jsonl(&apg_root.join("specs").join(name), &recs).unwrap();
        }

        // Establish the merged spec state in the scratch DB (one re-ingest
        // merges every project's files, like any write-through would).
        reingest_project(&apg_root, "cosanima-rename").unwrap();

        // The mystery command: add a note on the Spec AND the Decision.
        let path = specs::spec_jsonl_path(&apg_root, "cosanima-rename");
        let mut records = specs::read_jsonl(&path).unwrap();
        records.push(Record::Note {
            fqn: "future/cosanima-rename/note-11".into(),
            body: "repro".into(),
            kind: "background".into(),
        });
        records.push(Record::Details {
            from: "future/cosanima-rename/note-11".into(),
            to: "future/cosanima-rename/spec".into(),
        });
        records.push(Record::Details {
            from: "future/cosanima-rename/note-11".into(),
            to: "future/cosanima-rename/spec.decision-D1".into(),
        });
        write_jsonl_and_reingest(&apg_root, &path, "cosanima-rename", &records).unwrap();

        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("future/cosanima-rename/note-11"));
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'future/cosanima-rename/note-11'})-[:Details]->(t) RETURN t.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/cosanima-rename/spec"), "{out}");
        assert!(
            out.contains("future/cosanima-rename/spec.decision-D1"),
            "{out}"
        );
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_through_commits_jsonl_and_db() {
        let (apg_root, dir) = fixture("commit");
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();

        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        // The JSONL matches the new records (a first write lands in the DB too,
        // even though the file did not exist before this call).
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("future/foo/spec"));
        assert!(db.has_node("future/foo/spec.R1"));
        assert!(db.has_node("future/foo/note-1"));
        assert_eq!(orphan_notes(&db), 0);
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note)-[:Details]->(s:Spec) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec"), "details edge: {out}");
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_through_without_db_writes_jsonl() {
        let (apg_root, dir) = fixture("nodb");
        std::fs::remove_file(apg_root.join(specs::TRANS).join("db.lbug")).unwrap();
        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();

        // No scan yet: the JSONL is the durable form; no re-ingest is attempted.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        assert!(!path.as_os_str().to_string_lossy().ends_with(".tmp"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_db_refuses_mutation_before_any_jsonl_write() {
        let (apg_root, dir, sha0) = git_fixture("stale");
        // The DB records a clean scan at the *first* commit...
        write_scan_meta(&apg_root, &sha0, true);
        // ...but the tree has since moved on to a second commit: stale.
        let git_ok = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success());
        };
        git_ok(&["commit", "-q", "--allow-empty", "-m", "second"]);
        assert!(git::is_stale(&apg_root));

        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("graph is stale"), "refusal message: {msg}");
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");

        // The durable JSONL was never written (no partial mutation), and no
        // temp residue sits next to where it would have gone.
        assert!(!path.exists(), "refused mutation must not write JSONL");
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join("specs"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_git_db_allows_write_through() {
        let (apg_root, dir, sha) = git_fixture("fresh");
        // The recorded scan matches the current tree exactly → fresh.
        write_scan_meta(&apg_root, &sha, true);
        assert!(!git::is_stale(&apg_root));

        let path = specs::spec_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        // The write-through landed in both the durable JSONL and the live DB.
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("future/foo/spec"));
        assert!(db.has_node("future/foo/note-1"));
        assert_eq!(orphan_notes(&db), 0);
        drop(db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn annotation_fqns_are_per_ledger_namespaced() {
        // Two modules' ledgers each number from 1 — the FQNs must stay unique
        // (a flat `annotations/N` numbering collides on graph merge).
        let mod_a = vec![
            Record::Note {
                fqn: "annotations/github.com_flow_a/1".into(),
                body: "a1".into(),
                kind: "background".into(),
            },
            Record::Note {
                fqn: "annotations/github.com_flow_a/2".into(),
                body: "a2".into(),
                kind: "background".into(),
            },
        ];
        let mod_b = vec![Record::Note {
            fqn: "annotations/github.com_flow_b/1".into(),
            body: "b1".into(),
            kind: "background".into(),
        }];
        assert_eq!(next_free_annotation(&mod_a, "github.com_flow_a"), 3);
        assert_eq!(next_free_annotation(&mod_b, "github.com_flow_b"), 2);
        // Fresh ledger starts at 1.
        assert_eq!(next_free_annotation(&[], "_root"), 1);
    }
}
