//! Write-through authoring (SPEC R5) and the DB helpers the `apg spec` /
//! `apg plan` / `apg review` CLIs share: opening the live `apg/.trans/db.lbug`
//! read-write, resolving FQNs against the code graph, and re-ingesting a
//! project's spec/plan/note records via Cypher MERGE. Mutations never rebuild
//! the DB — the code graph is untouched; only the project's `…` nodes
//! are detached and re-merged from its JSONL files.

use std::collections::{BTreeSet, HashMap, HashSet};
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
/// This is the **central mutation funnel** (R4): every spec/plan/review/
/// invariant mutation — and every code-note ledger write (`add_note`'s
/// code-target branch) — routes through here, so the two gates beside each
/// other cover them all:
///
/// 1. **Membership guard** (R3): writes only happen inside a project context —
///    the project's worktree at `<main>/apg/.worktrees/<project>`, on the
///    project's branch. A refused mutation names which membership half failed
///    plus one fix line (exit 1 at the CLI). Reads are always unguarded; main
///    is never a mutation place. Universal-scope targets (the shared
///    `_invariants.jsonl` ledger) require any project context — scope is
///    orthogonal to mutation context (R3).
///
/// 2. **Refuse-on-stale gate** (agent-loop hardening): when a DB exists **and**
///    the DB is stale (`is_stale` — the tree moved on since the scan that
///    built it), the mutation bails *before* any JSONL write or re-ingest.
///    Missing-DB and non-git paths stay allowed (non-git paths cannot pass
///    the membership guard anyway).
///
/// **Auto-commit** (R8): after the write-through succeeds, a durable target
/// (anything outside the gitignored `apg/.trans/`) is committed on the
/// project branch via git2 — one commit per mutation, single-file diffs —
/// and the staleness gate's recorded `scan_meta` is re-anchored to the new
/// state (DB and tree in sync by construction; consecutive mutations do not
/// each demand a rescan). Plan mutations never commit: `apg/.trans` is
/// gitignored and transient by design. An auto-commit failure degrades to a
/// warning on stderr: the mutation already landed, and the staleness gate
/// will demand a scan before the next one (the same degradation as a
/// hand-committed change).
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
    // Membership guard (R3/R4): every write happens inside a project context.
    // Universal-scope targets (the `_invariants.jsonl` shared ledger) have no
    // project of their own — any project context satisfies the guard.
    let universal = path.file_name().is_some_and(|n| n == "_invariants.jsonl");
    if universal {
        git::require_project_context(apg_root)?;
    } else {
        git::require_membership(apg_root, project)?;
    }
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
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    } else {
        specs::write_jsonl(path, records)?;
    }
    // R8: auto-commit durable targets on the project branch; plan/review
    // targets under apg/.trans never commit. Re-anchor the recorded scan_meta
    // only when the commit actually moved the branch.
    if !path.starts_with(apg_root.join(specs::TRANS)) {
        match git::auto_commit(apg_root, path) {
            Ok(Some(_)) => {
                if let Err(e) = git::reanchor_scan_meta(apg_root, &git::git_state(apg_root)) {
                    eprintln!(
                        "apg: warning: could not re-anchor scan_meta after auto-commit: {e:#}"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "apg: warning: mutation landed but auto-commit failed ({e:#}): the staleness gate will demand a scan before the next mutation"
                );
            }
        }
    }
    Ok(())
}

pub struct ArtifactDb {
    pub db: Database,
}

/// True when `fqn` resolves to a node in the live graph (any kind). Used by
/// the test suite and the tool surface; `#[allow(dead_code)]` because the
/// shipping CLI paths test existence via label queries.
#[allow(dead_code)]
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

    /// Existence of any node at `fqn` in the live graph. Used by the test suite
    /// and the tool surface; `#[allow(dead_code)]` because the shipping CLI paths
    /// test existence via label queries.
    #[allow(dead_code)]
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

    /// The **Implementation** label of `fqn` — one of the four Implementation
    /// tier labels (Module/File/Struct/Function), or `None`. Unlike
    /// [`code_label`](Self::code_label) this deliberately EXCLUDES
    /// `UnresolvedTarget`: an unresolved reference is not real code. Used by
    /// the apply gate's planned-node realization check — a planned node is
    /// realized only when a scanned Implementation node occupies its FQN
    /// (PlanCompletion-SPEC.md; a planned FQN that happens to match an
    /// UnresolvedTarget is not code).
    pub fn impl_label(&self, fqn: &str) -> Option<&'static str> {
        for l in ["Function", "Struct", "File", "Module"] {
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

    /// True when `fqn` is a `planned` Implementation node (a plan-writer-authored
    /// placeholder awaiting realization — GraphModel-SPEC.md). The placeholder
    /// node is gone (PHASE_02); pending anchors are detected by `status: planned`,
    /// never a separate kind.
    /// The label of a `planned` Implementation node at `fqn`, or `None`.
    pub fn is_planned(&self, fqn: &str) -> bool {
        for l in ["Struct", "Function", "File", "Module"] {
            if count(
                &self.db,
                &format!(
                    "MATCH (n:{l} {{fqn: {}}}) WHERE n.status = 'planned' RETURN count(*)",
                    lit(fqn)
                ),
            ) > 0
            {
                return true;
            }
        }
        false
    }

    /// Deletes every node with fqn `<project>/…` and its incident
    /// edges. Used to reset a project's spec/plan/feedback state before
    /// re-merging its JSONL (code nodes are untouched). Runs on `conn` so the
    /// deletion shares the caller's transaction.
    pub fn detach_delete_project(&self, conn: &Connection, project: &str) -> anyhow::Result<()> {
        conn.query(&format!(
            "MATCH (n) WHERE n.fqn STARTS WITH {} DETACH DELETE n",
            lit(&format!("{project}/"))
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
            if let Some((label, fqn, props)) = node_merge(r) {
                if matches!(r, Record::PlannedNode { .. }) && realized.contains(fqn) {
                    continue;
                }
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
        Record::Stakeholder { fqn, name, body } => Some((
            "Stakeholder",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::Entity { fqn, name, body } => Some((
            "Entity",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::System { fqn, name, body } => Some((
            "System",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::Container {
            fqn,
            name,
            kind,
            body,
        } => Some((
            "Container",
            fqn,
            vec![
                ("name", name.clone()),
                ("kind", kind.clone()),
                ("body", body.clone()),
            ],
        )),
        Record::Component { fqn, name, body } => Some((
            "Component",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::User { fqn, name, body } => Some((
            "User",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::Group {
            fqn,
            name,
            attribute,
            root,
            body,
        } => Some((
            "DomainGroup",
            fqn,
            vec![
                ("name", name.clone()),
                ("attribute", attribute.clone()),
                ("root", root.clone()),
                ("body", body.clone()),
            ],
        )),
        Record::Value { fqn, name, body } => Some((
            "Value",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::Service { fqn, name, body } => Some((
            "Service",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::Person { fqn, name, body } => Some((
            "Person",
            fqn,
            vec![("name", name.clone()), ("body", body.clone())],
        )),
        Record::Constraint {
            fqn,
            name,
            body,
            attaches_to,
        } => Some((
            "Constraint",
            fqn,
            vec![
                ("name", name.clone()),
                ("body", body.clone()),
                ("attaches_to", attaches_to.clone()),
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
        Record::Satisfies { from, to } => Some(("Satisfies", from, to)),
        Record::Drives { from, to } => Some(("Drives", from, to)),
        Record::Represents { from, to } => Some(("Represents", from, to)),
        Record::RealisedBy { from, to } => Some(("RealisedBy", from, to)),
        Record::SpecImplementedBy { from, to } => Some(("SpecImplementedBy", from, to)),
        Record::Publishes { from, to } => Some(("Publishes", from, to)),
        Record::Subscribes { from, to } => Some(("Subscribes", from, to)),
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

/// The endpoints of an edge record, if it is one.
pub fn edge_endpoints(r: &Record) -> Option<(&str, &str)> {
    match r {
        Record::Contains { from, to }
        | Record::Details { from, to }
        | Record::Reviews { from, to }
        | Record::DependsOn { from, to }
        | Record::Gates { from, to }
        | Record::Satisfies { from, to }
        | Record::Drives { from, to }
        | Record::Represents { from, to }
        | Record::RealisedBy { from, to }
        | Record::SpecImplementedBy { from, to }
        | Record::Publishes { from, to }
        | Record::Subscribes { from, to } => Some((from, to)),
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

/// Assembles the record set a re-ingest merges: the project's transient plan
/// JSONL (the committed spec/note durable halves are gone — spec data lives in
/// the `apg/layers` tree, re-ingested separately). When `substitute` names the
/// plan file, it contributes `records` instead of its on-disk content
/// (write-through re-ingests the in-memory records before they are committed).
fn assembled_records(
    apg_root: &Path,
    project: &str,
    substitute: Option<(&Path, &[Record])>,
) -> anyhow::Result<Vec<Record>> {
    let (sub_path, sub_records) = match substitute {
        Some((p, r)) => (Some(p), r),
        None => (None, &[][..]),
    };
    let plan_path = specs::plan_jsonl_path(apg_root, project);

    let mut records: Vec<Record> = Vec::new();

    // The project's plan (transient; read from disk unless substituted). The
    // committed spec/note durable halves are gone — spec data lives in the
    // `apg/layers` tree (re-ingested separately via `reingest_layers`), not in
    // project-scoped JSONL.
    let sub_is_plan = sub_path == Some(plan_path.as_path());
    if sub_is_plan || plan_path.exists() {
        if sub_is_plan {
            records.extend_from_slice(sub_records);
        } else {
            records.extend(specs::read_jsonl(&plan_path)?);
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
            let (_, suffix) = f.split_once(&format!("/{prefix}"))?;
            suffix.parse::<u64>().ok()
        })
        .max()
        .map(|n| n + 1)
        .unwrap_or(1)
}

/// The two code-reference universes `layers::ingest_tree` validates
/// `implemented-by` targets against: `scanned` = every code-node FQN the last
/// scan produced (Module/File/Struct/Function without `status: planned`),
/// `planned` = the planned-node FQNs still awaiting realization (`status:
/// planned`). Read from the live DB, so a node/edge write re-merge can run the
/// code-ref check against the scanned graph.
pub fn code_universes(apg_root: &Path) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let db = ArtifactDb::open(apg_root)?;
    let conn = db.conn()?;
    let mut scanned = BTreeSet::new();
    let mut planned = BTreeSet::new();
    for label in ["Module", "File", "Struct", "Function"] {
        let result = conn.query(&format!("MATCH (n:{label}) RETURN n.fqn, n.status"))?;
        for row in result {
            let fqn = row.first().map(|v| v.to_string()).unwrap_or_default();
            let status = row.get(1).map(|v| v.to_string()).unwrap_or_default();
            if status == "planned" {
                planned.insert(fqn);
            } else {
                scanned.insert(fqn);
            }
        }
    }
    Ok((scanned, planned))
}

/// Re-ingests the durable layers tree into the live DB after a node/edge
/// mutation: detaches every node-file node (FQN prefix `<layer>.` for the five
/// durable layer dirs) and re-merges the caller-supplied `records` (the
/// `layers::ingest_tree` output) — nodes first, then edges, in one transaction.
/// A failed merge rolls back, so the DB keeps its prior committed state.
pub fn reingest_layers(apg_root: &Path, records: &[Record]) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    let conn = db.conn()?;
    conn.query("BEGIN TRANSACTION")?;
    let result = (|| -> anyhow::Result<()> {
        for layer_dir in [
            "requirements",
            "domain",
            "solution",
            "implementation",
            "global",
        ] {
            conn.query(&format!(
                "MATCH (n) WHERE n.fqn STARTS WITH '{}.' DETACH DELETE n",
                layer_dir
            ))?;
        }
        db.merge_records(&conn, records)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.query("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.query("ROLLBACK");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::load;
    use crate::testutil::{self, Repo};

    /// A real project context (R4): a git repo whose worktree `foo` on branch
    /// `foo` carries a real `apg/.trans/db.lbug` code graph plus a fresh
    /// scan_meta. Returns `(wt_apg_root, repo, wt_root)`. Tags are
    /// module-prefixed so parallel tests in other modules never collide on a
    /// temp dir.
    fn project_fixture(name: &str) -> (PathBuf, Repo, PathBuf) {
        let repo = Repo::new(&format!("artifacts-{name}"));
        let wt = repo.start_project("foo");
        db_at(&wt);
        testutil::write_scan_meta(
            &wt.join(specs::LAYOUT),
            Some(&repo.head_sha()),
            true,
            "2026-09-07T00:00:00Z",
        );
        (wt.join(specs::LAYOUT), repo, wt)
    }

    /// Builds a real DB + load files under `dir/apg` (used by `project_fixture`
    /// — the git-aware fixtures init the repo around the worktree first).
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

    /// The committed baseline records for the `foo` project: a transient plan
    /// with one phase and one healthy note (Details → Plan). The funnel's
    /// durable spec/note halves are gone — it now serves the `.trans` plan/
    /// review writers only.
    fn baseline_records() -> Vec<Record> {
        vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "Foo".into(),
                strategy: "G".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Note {
                fqn: "foo/note-1".into(),
                body: "first".into(),
                kind: "background".into(),
            },
            Record::Details {
                from: "foo/note-1".into(),
                to: "foo/plan".into(),
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

    /// Commits paths on the worktree's branch (git2) — for test setup that
    /// deliberately writes files outside the funnel, followed by a scan_meta
    /// re-anchor so the DB stays fresh.
    fn wt_commit_paths(wt: &Path, paths: &[&str], msg: &str) -> String {
        let repo = git2::Repository::open(wt).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(paths.iter().copied(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
            .unwrap()
            .to_string()
    }

    #[test]
    fn illegal_details_pair_is_projected_away_not_a_binder_error() {
        let (apg_root, repo, _wt) = project_fixture("orphan");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();

        // A healthy committed state, write-through.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("foo/plan"));
            assert!(db.has_node("foo/note-1"));
            assert_eq!(orphan_notes(&db), 0);
        }

        // A note whose Details edge targets another Note — a pair the Details
        // rel table does NOT declare. The schema-pair guard projects the
        // illegal pair away (the same bucketing the scan load path applies),
        // so the write-through succeeds and the DB projection matches what a
        // fresh scan would produce (the note node lands, the impossible edge
        // never materializes).
        let mut mutated = baseline.clone();
        mutated.push(Record::Note {
            fqn: "foo/note-2".into(),
            body: "poison".into(),
            kind: "background".into(),
        });
        mutated.push(Record::Details {
            from: "foo/note-2".into(),
            to: "foo/note-1".into(),
        });
        write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap();

        // The JSONL committed (the note record is durable truth).
        assert_eq!(specs::read_jsonl(&path).unwrap(), mutated);
        // No temp residue next to the committed JSONL.
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        // The DB projection: the note node lands, the undeclared Details edge
        // does not (identical to the scan load path's pair bucketing).
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/note-2"));
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note {fqn: 'foo/note-2'})-[:Details]->() RETURN count(*)")
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
            .query("MATCH (:Note {fqn: 'foo/note-1'})-[:Details]->(p:Plan) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out.lines().last() == Some("1"), "healthy edge: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    fn write_through_commits_jsonl_and_db() {
        let (apg_root, repo, _wt) = project_fixture("commit");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();

        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        // The JSONL matches the new records (a first write lands in the DB too,
        // even though the file did not exist before this call).
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/plan"));
        assert!(db.has_node("foo/plan.phase-01"));
        assert!(db.has_node("foo/note-1"));
        assert_eq!(orphan_notes(&db), 0);
        let out = db
            .conn()
            .unwrap()
            .query("MATCH (:Note)-[:Details]->(p:Plan) RETURN p.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/plan"), "details edge: {out}");
        drop(db);

        testutil::remove(&repo);
    }

    #[test]
    fn write_through_without_db_writes_jsonl() {
        let (apg_root, repo, _wt) = project_fixture("nodb");
        std::fs::remove_file(apg_root.join(specs::TRANS).join("db.lbug")).unwrap();
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();

        // No scan yet: the JSONL is the durable form; no re-ingest is attempted.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        assert!(!path.as_os_str().to_string_lossy().ends_with(".tmp"));

        testutil::remove(&repo);
    }

    #[test]
    fn stale_db_refuses_mutation_before_any_jsonl_write() {
        let (apg_root, repo, wt) = project_fixture("stale");
        // The DB records a clean scan at the first commit; then the branch
        // tree moves on to a second commit: stale.
        std::fs::write(wt.join("extra.txt"), "x").unwrap();
        wt_commit_paths(&wt, &["extra.txt"], "second");
        assert!(git::is_stale(&apg_root));

        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("graph is stale"), "refusal message: {msg}");
        assert!(msg.contains("run `apg scan` before mutating"), "{msg}");

        // The durable JSONL was never written (no partial mutation), and no
        // temp residue sits next to where it would have gone.
        assert!(!path.exists(), "refused mutation must not write JSONL");
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        testutil::remove(&repo);
    }

    #[test]
    fn fresh_git_db_allows_write_through() {
        let (apg_root, repo, _wt) = project_fixture("fresh");
        // The recorded scan matches the current tree exactly → fresh.
        assert!(!git::is_stale(&apg_root));

        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        // The write-through landed in both the durable JSONL and the live DB.
        assert_eq!(specs::read_jsonl(&path).unwrap(), recs);
        let db = ArtifactDb::open(&apg_root).unwrap();
        assert!(db.has_node("foo/plan"));
        assert!(db.has_node("foo/note-1"));
        assert_eq!(orphan_notes(&db), 0);
        drop(db);

        testutil::remove(&repo);
    }
}
