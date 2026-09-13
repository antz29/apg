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

/// The four Implementation labels a plan can declare as a **planned** node. A
/// planned placeholder carries `status = 'planned'`; when a branch scan realizes
/// the FQN as real code the row loses that marker (`status IS NULL`), and it
/// must never be detached by a metadata-mutation delete (phase-05 task-1).
const PLANNED_CODE_LABELS: [&str; 4] = ["Module", "File", "Struct", "Function"];

/// Code-graph labels that are never metadata: the four Implementation labels
/// (which can also hold a planned placeholder — see [`PLANNED_CODE_LABELS`]),
/// the scan control node, and unresolved references. A metadata delta must
/// never detach `UnresolvedTarget`/`Scan` by FQN — they are scanned code, not
/// spec/plan state.
const CODE_GRAPH_LABELS: [&str; 6] = [
    "Module",
    "File",
    "Struct",
    "Function",
    "UnresolvedTarget",
    "Scan",
];

/// The process-wide reentrant extended write lock (see `acquire_spec_lock`):
/// one `LOCK_EX` flock per lock-file path, held while any live guard exists and
/// released when the outermost guard drops (closing the fd).
static SPEC_LOCK: OnceLock<Mutex<SpecLockState>> = OnceLock::new();

/// The held flocks, keyed by lock-file path (a process can host several
/// fixtures/projects; a test process holds more than one at a time). Each entry
/// records the open `File` (the flock's open file description) and the nested
/// acquisition depth.
#[derive(Default)]
struct SpecLockState {
    held: HashMap<PathBuf, (File, usize)>,
}

/// A held acquisition of the extended write lock. Dropping the last live guard
/// for a lock file releases its flock (the `File` is removed, closing the fd);
/// nested acquisitions share the one fd, so a command that needs a second
/// acquisition (plan complete loading both plan and spec) never self-deadlocks.
#[must_use = "dropping the guard immediately releases the whole-durable-sequence lock"]
pub struct SpecLockGuard {
    lock_path: PathBuf,
}

impl Drop for SpecLockGuard {
    fn drop(&mut self) {
        let holder = SPEC_LOCK
            .get()
            .expect("a SpecLockGuard exists, so the state is initialized");
        let mut state = holder.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, depth)) = state.held.get_mut(&self.lock_path) {
            *depth -= 1;
            if *depth == 0 {
                // Dropping the File closes the fd, releasing the flock.
                state.held.remove(&self.lock_path);
            }
        }
    }
}

/// Acquires the exclusive cross-process lock that serializes a whole durable
/// sequence on the project: the **extended** spec/plan/review write lock.
///
/// The direct `apg node` / `apg edge` path takes it exactly once at the
/// `cmd_node`/`cmd_edge` dispatch entry, before any node-file read, and holds
/// it across validate → write → the one-commit-per-mutation git commit → the
/// write-through projection. That single flock is what serializes the three
/// contended locks a parallel burst hits: the node-file read-modify-write, git's
/// `.git/index.lock` (inside `git::commit_files`), and the read-write `db.lbug`
/// projection apply. The plan/review JSONL funnel takes the same lock, so the
/// direct path and the phase-03 session coordinator are mutually exclusive.
///
/// Reentrant within the process: a nested acquire returns a guard that shares
/// the already-held fd (incrementing the depth), so a command that acquires
/// twice does not deadlock; the flock is released only when the outermost guard
/// drops. The flock lives on `apg/.trans/specs.lock`.
pub fn acquire_spec_lock(apg_root: &Path) -> anyhow::Result<SpecLockGuard> {
    let holder = SPEC_LOCK.get_or_init(|| Mutex::new(SpecLockState::default()));
    let mut state = holder.lock().unwrap_or_else(|e| e.into_inner());
    let lock_path = apg_root.join(".trans").join("specs.lock");
    if let Some((_, depth)) = state.held.get_mut(&lock_path) {
        // Reentrant: share the existing fd, just add a nesting level.
        *depth += 1;
        return Ok(SpecLockGuard { lock_path });
    }
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = File::create(&lock_path)?;
    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&f), libc::LOCK_EX) };
        if rc != 0 {
            anyhow::bail!("could not acquire write lock {}", lock_path.display());
        }
    }
    state.held.insert(lock_path.clone(), (f, 1));
    Ok(SpecLockGuard { lock_path })
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
/// **Auto-commit** (R8): after the durable write lands (and before the
/// projection), a durable target (anything outside the gitignored `apg/.trans/`)
/// is committed on the project branch via git2 — one commit per mutation,
/// single-file diffs — and the staleness gate's recorded `scan_meta` is
/// re-anchored to the new state (DB and tree in sync by construction;
/// consecutive mutations do not each demand a rescan). Plan mutations never
/// commit: `apg/.trans` is gitignored and transient by design. An auto-commit
/// failure degrades to a warning on stderr: the mutation already landed, and
/// the staleness gate will demand a scan before the next one (the same
/// degradation as a hand-committed change).
///
/// **Commit-then-project** (phase-05 tasks 3/12): the durable/transient file
/// write and its commit land FIRST — the system-of-record durability point —
/// and only then is the exact projection delta applied to `db.lbug`. The delta
/// is computed HERE, inside the funnel, from the assembled transient record set
/// before and after this write (no per-caller threading): the delete set is
/// exactly the removed ∪ changed FQNs plus the sources of vanished edges
/// ([`transient_delta`]), never a `<project>/` prefix. A crash between the write
/// and the projection is reproduced by the next rebuild as the committed state,
/// never the uncommitted projection. The rename is still atomic (a sibling temp
/// swapped over `path`), so a crash mid-write never leaves half a JSONL.
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
    let has_db = apg_root.join(specs::TRANS).join("db.lbug").exists();
    if has_db && let Some(msg) = git::refusal_message(apg_root) {
        anyhow::bail!("{msg}");
    }

    // The exact delta, computed while the committed file still holds the old
    // content. `after` substitutes the in-memory records for this file; every
    // other transient file is read as-is. A durable target (the shared
    // `_invariants.jsonl` ledger) is not part of the transient set: merge
    // exactly the records written.
    let delta = if has_db && path.starts_with(apg_root.join(specs::TRANS)) {
        let before = assembled_records(apg_root, project, None)?;
        let after = assembled_records(apg_root, project, Some((path, records)))?;
        Some((transient_delta(&before, &after), after))
    } else if has_db {
        Some((BTreeSet::new(), records.to_vec()))
    } else {
        None
    };

    // 1. Durable file write (the commit): atomic temp + rename.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    specs::write_jsonl(&tmp, records)?;
    std::fs::rename(&tmp, path)?;

    // 2. Auto-commit durable targets, then re-anchor scan_meta.
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

    // 3. Projection delta AFTER the commit. A failure rolls the projection
    //    transaction back and reports failure; the committed file is the system
    //    of record and the next rebuild reproduces it.
    if let Some((deletes, after)) = delta {
        reingest_project_with(apg_root, &deletes, &after)?;
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

    /// Runs a query and returns its formatted output. Test-only: the shipping
    /// CLI paths use label-typed queries via [`count`](Self::count) or the
    /// query subcommand.
    #[cfg(test)]
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
///
/// `Calls`/`Uses` are shared code rel tables: the same record kind carries the
/// scanned Function→Function / Function→Struct / Struct→Struct edges and the
/// authored Service→Service `calls` / Person→System `uses` edges (the only
/// pairs `apg edge add` can author). The merge guard
/// ([`rel_pair_allowed`]) admits the authored pairs; the scanned pairs never
/// reach this merge (they come from the load path, not a record set).
pub(crate) fn edge_merge(r: &Record) -> Option<(&'static str, &str, &str)> {
    match r {
        Record::Contains { from, to } => Some(("Contains", from, to)),
        Record::Calls { from, to } => Some(("Calls", from, to)),
        Record::Uses { from, to } => Some(("Uses", from, to)),
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

/// The DB label and FQN of a node record — the exact pair [`merge_records`]
/// MERGEs. The projection-equals-sources check (phase-05 task-9) uses it to
/// derive the expected node set from the source record stream.
#[cfg(test)]
pub(crate) fn node_label_fqn(r: &Record) -> Option<(&'static str, &str)> {
    node_merge(r).map(|(label, fqn, _)| (label, fqn))
}

/// The endpoints of an edge record, if it is one.
// Kept as a shared record-rewrite primitive: its production callers are the
// incident-edge-stripping paths (`remove_node`), currently exercised by the
// test suite (the strict plan add surface no longer upserts).
#[allow(dead_code)]
pub fn edge_endpoints(r: &Record) -> Option<(&str, &str)> {
    match r {
        Record::Contains { from, to }
        | Record::Calls { from, to }
        | Record::Uses { from, to }
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

/// The exact set of node FQNs a transient metadata mutation must detach before
/// re-merging (phase-05 task-3): every FQN present before but gone now
/// (removed), every FQN whose node record changed (changed), and the source of
/// every edge that vanished — a MERGE-only re-merge never deletes a vanished
/// edge, so its surviving source is detached and re-merged with its current
/// out-edges. Detaching a node drops its incident edges; re-merging the full
/// post-mutation record set restores the current node and its current edges, so
/// the projection converges to the sources exactly.
fn transient_delta(before: &[Record], after: &[Record]) -> BTreeSet<String> {
    let before_nodes: HashMap<&str, &Record> = before
        .iter()
        .filter_map(|r| node_fqn(r).map(|f| (f, r)))
        .collect();
    let after_nodes: HashMap<&str, &Record> = after
        .iter()
        .filter_map(|r| node_fqn(r).map(|f| (f, r)))
        .collect();

    let mut deletes: BTreeSet<String> = BTreeSet::new();
    for (fqn, prior) in &before_nodes {
        match after_nodes.get(fqn) {
            // Unchanged (same node record) — leave it and its edges in place.
            Some(current) if current == prior => {}
            // Removed, or changed (its dropped incident edges must not survive).
            _ => {
                deletes.insert((*fqn).to_string());
            }
        }
    }

    // A vanished edge is only a MERGE away from surviving. Detach its source
    // (which is guaranteed to be part of the merged set — a transient record's
    // source is itself transient) so the stale out-edge drops and the re-merge
    // restores the current edge set.
    let edge_key = |r: &Record| {
        edge_merge(r).map(|(table, from, to)| (table.to_string(), from.to_string(), to.to_string()))
    };
    let before_edges: BTreeSet<(String, String, String)> =
        before.iter().filter_map(edge_key).collect();
    let after_edges: BTreeSet<(String, String, String)> =
        after.iter().filter_map(edge_key).collect();
    for (_, from, _) in before_edges.difference(&after_edges) {
        if after_nodes.contains_key(from.as_str()) {
            deletes.insert(from.clone());
        }
    }
    deletes
}

/// Applies the exact transient projection delta in ONE transaction: detach
/// exactly `deletes` (the removed ∪ changed FQNs plus vanished-edge sources),
/// then re-merge `records` — nodes first, then edges. A removed planned FQN
/// with no project prefix is deleted by exact FQN; an added node/edge is
/// MERGEd. A failure rolls the whole transaction back, so the DB keeps its
/// prior committed state.
fn reingest_project_with(
    apg_root: &Path,
    deletes: &BTreeSet<String>,
    records: &[Record],
) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    let conn = db.conn()?;
    conn.query("BEGIN TRANSACTION")?;
    let result = (|| -> anyhow::Result<()> {
        db.detach_delete_project(&conn, deletes)?;
        // Test-only injection: prove a mid-apply failure rolls the projection
        // back to its prior state (phase-05 task-10).
        #[cfg(test)]
        fire_projection_hook()?;
        db.merge_records(&conn, records)?;
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

// A test-only one-shot injection fired inside a transient projection apply,
// after the delete set is detached and before the records are re-merged.
// Tests use it to force a mid-apply failure and assert the transaction rolls
// back to the prior projection (phase-05 task-10).
#[cfg(test)]
type ProjectionHook = Box<dyn FnOnce() -> anyhow::Result<()>>;

#[cfg(test)]
thread_local! {
    static PROJECTION_HOOK: std::cell::RefCell<Option<ProjectionHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn install_projection_hook(hook: impl FnOnce() -> anyhow::Result<()> + 'static) {
    PROJECTION_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn fire_projection_hook() -> anyhow::Result<()> {
    let hook = PROJECTION_HOOK.with(|cell| cell.borrow_mut().take());
    match hook {
        Some(hook) => hook(),
        None => Ok(()),
    }
}

/// Assembles the record set a re-ingest merges: the project's transient
/// state — the plan store plus the five feedback tier mirrors (SPEC §5:
/// `.trans/plans/<project>.jsonl` and `.trans/<tier>/<project>.jsonl`).
/// Feedback on durable/code nodes lives in the mirrors, and every file shares
/// the project's `<project>/feedback-<n>` namespace, so a re-ingest after ANY
/// of them must merge ALL of them (the delta's delete set covers every changed
/// or removed FQN; a write-through that forgot the other mirrors would silently
/// erase their feedback from the DB). The committed spec/note
/// durable halves are gone — spec data lives in the `apg/layers` tree,
/// re-ingested separately. When `substitute` names one of the transient
/// files, it contributes `records` instead of its on-disk content.
fn assembled_records(
    apg_root: &Path,
    project: &str,
    substitute: Option<(&Path, &[Record])>,
) -> anyhow::Result<Vec<Record>> {
    let (sub_path, sub_records) = match substitute {
        Some((p, r)) => (Some(p), r),
        None => (None, &[][..]),
    };

    let mut records: Vec<Record> = Vec::new();
    for f in specs::project_transient_files(apg_root, project) {
        let sub_is_f = sub_path == Some(f.as_path());
        if sub_is_f || f.exists() {
            if sub_is_f {
                records.extend_from_slice(sub_records);
            } else {
                records.extend(specs::read_jsonl(&f)?);
            }
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
/// `implemented-by` targets against, read from the **live DB**: `scanned` =
/// every code-node FQN the last scan produced (Module/File/Struct/Function
/// without `status: planned`), `planned` = the planned-node FQNs still awaiting
/// realization (`status: planned`).
///
/// This opens `apg/.trans/db.lbug` read-write, so it must NOT be used on the
/// direct node/edge mutation path (phase-02 decoupled that path onto
/// [`code_universes_from_export`]); its remaining callers are the flock-guarded
/// plan paths (`plan_cmd`), where an exclusive DB read is already serialized.
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

/// The two code-reference universes `layers::validate_change` / `ingest_tree`
/// validate `implemented-by` targets against, resolved **without opening
/// `db.lbug`** (phase-02 DB decoupling):
///
/// - `scanned` — the real code FQNs from the `apg/.trans/graph.jsonl` export
///   (the sole code-identity source: a `module`/`file`/`struct`/`function`
///   record without `status: planned`). `graph.jsonl` is written only by a
///   scan, so a FQN absent from it is not called drift here while the export is
///   missing — the caller gates on the export (see `layers::validate_change`).
/// - `planned` — the FQNs the plan store declares still awaiting realization
///   (`Record::PlannedNode` in every `apg/.trans/plans/<project>.jsonl`), UNION
///   any `status: planned` code record already projected into the export. A
///   declared-but-unscanned FQN (e.g. `apg.session.Coordinator` before its scan)
///   therefore classifies Pending, never drift.
///
/// The export is parsed as generic JSON: only the four Implementation record
/// types and their `fqn`/`status` fields are consulted, and the `scan_meta`
/// control record on line 1 (or any spec/plan/edge record) is skipped.
pub fn code_universes_from_export(
    apg_root: &Path,
) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut scanned = BTreeSet::new();
    let mut planned = BTreeSet::new();

    let export = apg_root.join(specs::TRANS).join("graph.jsonl");
    if export.exists() {
        let text = std::fs::read_to_string(&export)?;
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                anyhow::anyhow!(
                    "{}:{}: bad graph.jsonl record: {e}",
                    export.display(),
                    i + 1
                )
            })?;
            let Some(kind) = value.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            if !matches!(kind, "module" | "file" | "struct" | "function") {
                continue;
            }
            let Some(fqn) = value.get("fqn").and_then(|f| f.as_str()) else {
                continue;
            };
            let status = value.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if status == "planned" {
                planned.insert(fqn.to_string());
            } else {
                scanned.insert(fqn.to_string());
            }
        }
    }

    // The plan store's planned-node declarations. A plan store FQN already
    // scanned stays in both sets; `classify_code_ref` prefers `scanned` (Real).
    for f in specs::plan_files(apg_root) {
        for record in specs::read_jsonl(&f)? {
            if let Record::PlannedNode { fqn, .. } = record {
                planned.insert(fqn);
            }
        }
    }

    Ok((scanned, planned))
}

/// Re-ingests the durable layers delta into the live DB after a node/edge
/// mutation: detaches exactly the mutated FQNs `deletes` (removed ∪ changed —
/// never a whole layer-dir prefix) and re-merges the caller-supplied `records`
/// (the `layers::ingest_tree` output) — nodes first, then edges, in one
/// transaction. A failed merge rolls back, so the DB keeps its prior committed
/// state.
pub fn reingest_layers(
    apg_root: &Path,
    deletes: &BTreeSet<String>,
    records: &[Record],
) -> anyhow::Result<()> {
    let db = ArtifactDb::open(apg_root)?;
    db.reingest_layers_on(deletes, records)
}

impl ArtifactDb {
    /// The write-through projection apply for the durable layers tree, run
    /// against an **already-held** database handle: detach exactly the mutated
    /// FQNs `deletes` ([`detach_delete_project`], with the planned-code guard) and
    /// re-merge the caller-supplied `records` (the `layers::ingest_tree`
    /// output) — nodes first, then edges, in one transaction. A failed merge
    /// rolls back, so the DB keeps its prior committed state.
    ///
    /// Split from the free [`reingest_layers`](crate::artifacts::reingest_layers)
    /// so the phase-03 session coordinator can amortize ONE DB open across N
    /// routed mutations: the coordinator owns the handle for the session's life
    /// and applies every mutation's projection delta synchronously through it —
    /// the open is amortized, visibility never is.
    pub fn reingest_layers_on(
        &self,
        deletes: &BTreeSet<String>,
        records: &[Record],
    ) -> anyhow::Result<()> {
        let conn = self.conn()?;
        conn.query("BEGIN TRANSACTION")?;
        let result = (|| -> anyhow::Result<()> {
            self.detach_delete_project(&conn, deletes)?;
            self.merge_records(&conn, records)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node, NodeKind};
    use crate::layers::{self, Layer};
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

    /// The plan re-merge write-through (`node_merge` → `MERGE SET`) carries the
    /// Task verb projection too: a re-ingested Task record lands its
    /// verb/target/new_fqn in the live DB, so the suite tools see the same
    /// columns the scan load path projects.
    #[test]
    fn write_through_projects_task_verb_fields() {
        let (apg_root, repo, _wt) = project_fixture("task-verb");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let recs = vec![
            Record::Plan {
                fqn: "foo/plan".into(),
                title: "Foo".into(),
                strategy: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: String::new(),
                status: "pending".into(),
            },
            Record::Contains {
                from: "foo/plan".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "Rename".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "renames".into(),
                target: "foo.Old".into(),
                new_fqn: "foo.New".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
        ];

        write_jsonl_and_reingest(&apg_root, &path, "foo", &recs).unwrap();

        let db = ArtifactDb::open(&apg_root).unwrap();
        let out = db
            .q("MATCH (t:Task) RETURN t.fqn, t.verb, t.target, t.new_fqn")
            .unwrap();
        assert!(
            out.contains("foo/plan.phase-01.task-1")
                && out.contains("renames")
                && out.contains("foo.Old")
                && out.contains("foo.New"),
            "write-through task verb projection: {out}"
        );
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

    /// The two record kinds the write-through re-merge used to drop through
    /// `_ => None`: authored `uses` (Person→System) and `calls`
    /// (Service→Service) map to their rel tables.
    #[test]
    fn edge_merge_maps_authored_uses_and_calls() {
        assert_eq!(
            edge_merge(&Record::Uses {
                from: "solution.person.alice".into(),
                to: "solution.system.portal".into(),
            }),
            Some(("Uses", "solution.person.alice", "solution.system.portal"))
        );
        assert_eq!(
            edge_merge(&Record::Calls {
                from: "domain.service.a".into(),
                to: "domain.service.b".into(),
            }),
            Some(("Calls", "domain.service.a", "domain.service.b"))
        );
    }

    /// The merge guard admits the two authored-only pairs through
    /// [`load::rel_table_pairs`] — the guard's only consumer.
    #[test]
    fn rel_pair_allowed_admits_authored_uses_and_calls() {
        assert!(rel_pair_allowed("Uses", "Person", "System"));
        assert!(rel_pair_allowed("Calls", "Service", "Service"));
    }

    /// `remove_node` strips incident authored `uses`/`calls` edges too — they
    /// were previously left behind because `edge_endpoints` did not recognize
    /// the two record kinds.
    #[test]
    fn remove_node_strips_incident_uses_and_calls_edges() {
        let mut records = vec![
            Record::Person {
                fqn: "solution.person.alice".into(),
                name: "alice".into(),
                body: String::new(),
            },
            Record::System {
                fqn: "solution.system.portal".into(),
                name: "portal".into(),
                body: String::new(),
            },
            Record::Uses {
                from: "solution.person.alice".into(),
                to: "solution.system.portal".into(),
            },
            Record::Service {
                fqn: "domain.service.a".into(),
                name: "a".into(),
                body: String::new(),
            },
            Record::Service {
                fqn: "domain.service.b".into(),
                name: "b".into(),
                body: String::new(),
            },
            Record::Calls {
                from: "domain.service.a".into(),
                to: "domain.service.b".into(),
            },
            // An unrelated node + edge that must survive both removals.
            Record::Note {
                fqn: "foo/note-1".into(),
                body: "background".into(),
                kind: "background".into(),
            },
            Record::Details {
                from: "foo/note-1".into(),
                to: "solution.system.portal".into(),
            },
        ];

        remove_node(&mut records, "solution.person.alice");
        remove_node(&mut records, "domain.service.a");

        let has_node = |fqn: &str| records.iter().any(|r| node_fqn(r) == Some(fqn));
        let has_edge = |from: &str, to: &str| {
            records
                .iter()
                .any(|r| edge_endpoints(r) == Some((from, to)))
        };
        assert!(!has_node("solution.person.alice"), "person must be removed");
        assert!(!has_node("domain.service.a"), "service must be removed");
        assert!(
            !has_edge("solution.person.alice", "solution.system.portal"),
            "the incident Uses edge must be removed with the person"
        );
        assert!(
            !has_edge("domain.service.a", "domain.service.b"),
            "the incident Calls edge must be removed with the service"
        );
        assert!(has_node("solution.system.portal"));
        assert!(has_node("domain.service.b"));
        assert!(has_edge("foo/note-1", "solution.system.portal"));
    }

    /// Phase-02 task-8: `code_universes_from_export` classifies Real
    /// (graph.jsonl) / Planned (plan store) / absent with NO `db.lbug` open.
    ///
    /// The fixture deliberately writes a BOGUS `db.lbug` (not a database): if
    /// the resolver opened it, decoding would fail — a passed test proves the
    /// DB was never touched. A declared-but-unscanned planned FQN (the
    /// `apg.session.Coordinator` example) classifies Planned, never drift.
    #[test]
    fn code_universes_from_export_classifies_real_planned_absent_without_db() {
        let dir = std::env::temp_dir().join(format!("apg-cu-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trans = dir.join(specs::TRANS);
        std::fs::create_dir_all(trans.join("plans")).unwrap();

        // graph.jsonl: a scan_meta lead, two real code nodes, and one node
        // already projected with `status: planned` (a planned declaration that
        // a scan carried through).
        let graph = [
            r#"{"type":"scan_meta","git_sha":"abc","git_clean":true,"scanned_at":"2026-09-07T00:00:00Z"}"#,
            r#"{"type":"module","fqn":"github.com/x/y"}"#,
            r#"{"type":"struct","fqn":"github.com/x/y.Store","path":"/abs/store.go","start":0,"end":1,"start_line":1,"end_line":1,"code_type":"src"}"#,
            r#"{"type":"function","fqn":"github.com/x/y.plannedFn","path":"/abs/store.go","start":0,"end":1,"start_line":1,"end_line":1,"code_type":"src","status":"planned"}"#,
        ]
        .join("\n");
        std::fs::write(trans.join("graph.jsonl"), format!("{graph}\n")).unwrap();

        // The plan store declares a FQN that has not been scanned yet.
        specs::write_jsonl(
            &trans.join("plans").join("foo.jsonl"),
            &[Record::PlannedNode {
                fqn: "apg.session.Coordinator".into(),
                kind: "struct".into(),
                name: "Coordinator".into(),
                parent: "apg.session".into(),
            }],
        )
        .unwrap();

        // A bogus DB — never opened by the resolver.
        std::fs::write(trans.join("db.lbug"), b"this is definitely not a db").unwrap();

        let (scanned, planned) = code_universes_from_export(&dir).unwrap();
        assert!(scanned.contains("github.com/x/y"));
        assert!(scanned.contains("github.com/x/y.Store"));
        assert!(
            !scanned.contains("github.com/x/y.plannedFn"),
            "a status:planned export record is not real code"
        );
        assert!(planned.contains("github.com/x/y.plannedFn"));
        assert!(planned.contains("apg.session.Coordinator"));

        // The three-way classification: Real / Planned (never Drift) / Drift.
        assert_eq!(
            crate::layers::classify_code_ref("github.com/x/y.Store", &scanned, &planned),
            crate::layers::CodeRefStatus::Real
        );
        assert_eq!(
            crate::layers::classify_code_ref("apg.session.Coordinator", &scanned, &planned),
            crate::layers::CodeRefStatus::Pending
        );
        assert_eq!(
            crate::layers::classify_code_ref("apg.gone.Nope", &scanned, &planned),
            crate::layers::CodeRefStatus::Drift
        );

        // graph.jsonl absent: the export contributes nothing; the plan store's
        // planned FQNs remain — the caller's graph.jsonl gate decides whether
        // code-FQN refs are validated at all.
        std::fs::remove_file(trans.join("graph.jsonl")).unwrap();
        let (scanned2, planned2) = code_universes_from_export(&dir).unwrap();
        assert!(scanned2.is_empty(), "no export ⇒ no scanned universe");
        assert!(planned2.contains("apg.session.Coordinator"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase-03 task-17: a routed mutation is written THROUGH — the durable
    /// write lands with exactly one commit and its projection delta is applied
    /// synchronously, so a mutation that reported success is already queryable
    /// by a separate routed reader, before session end. Nothing is buffered
    /// and there is no end-of-session flush.
    #[test]
    fn session_routed_write_is_committed_once_and_immediately_projected() {
        let (wt_apg, repo, wt) = project_fixture("session-write-through");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        let before = testutil::commit_count(&wt);
        let add = testutil::ApgCommand::new(&[
            "node",
            "add",
            "requirements",
            "requirement",
            "writethrough",
        ])
        .cwd(&wt)
        .env("HOME", home.to_str().unwrap())
        .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );

        // (a) Exactly one commit for the one logical mutation.
        assert_eq!(
            testutil::commit_count(&wt),
            before + 1,
            "one logical mutation → one commit"
        );

        // (b) The projection delta was applied synchronously: a NEW routed
        // reader (separate process) sees it BEFORE the session ends.
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.writethrough'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            q.status.success(),
            "routed read: {}",
            String::from_utf8_lossy(&q.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "the mutation must be queryable immediately, not at session end"
        );

        // (c) The durable node file is the system of record.
        assert!(
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "writethrough")
                .exists()
        );

        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-7: pin the three planned-node states against the exact-FQN
    /// projection delta.
    ///
    /// (1) A planned-only FQN with NO `<project>/` prefix disappears immediately
    /// when the plan's planned declaration is removed, and the code graph is
    /// otherwise untouched.
    /// (2) That FQN AFTER a scan realized it — the delete is guarded by
    /// `status = 'planned'`, so the REAL code node and its incident edges survive
    /// removing the plan's placeholder declaration.
    /// (3) An `implemented-by` to a declared-but-unscanned planned FQN stays
    /// Pending (never drift) once `code_universes_from_export` reads the plan
    /// store.
    #[test]
    fn planned_fqn_states_reflect_and_guard_realized_code() {
        let (apg_root, repo, _wt) = project_fixture("planned-states");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let planned = |fqn: &str, kind: &str, name: &str, parent: &str| Record::PlannedNode {
            fqn: fqn.to_string(),
            kind: kind.to_string(),
            name: name.to_string(),
            parent: parent.to_string(),
        };

        // The code-graph baseline: the real struct + the File→Struct Contains
        // edge. It must be untouched by every planned-declaration mutation.
        let code_contains = {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("github.com/x/y.Store"));
            count(
                &db.db,
                "MATCH (:File)-[:Contains]->(:Struct) RETURN count(*)",
            )
        };
        assert_eq!(code_contains, 1);

        // (1) A planned-only FQN with no project prefix.
        write_jsonl_and_reingest(
            &apg_root,
            &path,
            "foo",
            &[planned(
                "apg.session.Coordinator.start",
                "function",
                "start",
                "apg.session",
            )],
        )
        .unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("apg.session.Coordinator.start"));
            assert!(db.is_planned("apg.session.Coordinator.start"));
        }

        // Removing the declaration drops the un-prefixed planned FQN at once and
        // leaves the code graph alone.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &[]).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                !db.has_node("apg.session.Coordinator.start"),
                "a removed planned FQN without a project prefix must disappear immediately"
            );
            assert!(db.has_node("github.com/x/y.Store"));
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:File)-[:Contains]->(:Struct) RETURN count(*)"
                ),
                code_contains,
                "the code graph must be otherwise untouched"
            );
        }

        // (2) The FQN AFTER a scan realized it: declare a planned node at the
        // REAL struct's FQN. `merge_records` skips the placeholder (real code
        // wins), and removing the declaration must NOT detach the real node —
        // the status='planned' guard.
        write_jsonl_and_reingest(
            &apg_root,
            &path,
            "foo",
            &[planned(
                "github.com/x/y.Store",
                "struct",
                "Store",
                "github.com/x/y",
            )],
        )
        .unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(db.has_node("github.com/x/y.Store"));
            assert!(
                !db.is_planned("github.com/x/y.Store"),
                "the realized code node must keep status NULL"
            );
        }
        write_jsonl_and_reingest(&apg_root, &path, "foo", &[]).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert!(
                db.has_node("github.com/x/y.Store"),
                "removing a planned declaration must never drop the realized code node"
            );
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:File)-[:Contains]->(:Struct) RETURN count(*)"
                ),
                1,
                "the realized code node's incident edges must survive the guarded delete"
            );
        }

        // (3) An implemented-by to a declared-but-unscanned planned FQN is
        // Pending, never Drift — resolved from the plan store with no scan.
        let candidate = "apg.session.Coordinator.wait";
        write_jsonl_and_reingest(
            &apg_root,
            &path,
            "foo",
            &[planned(candidate, "function", "wait", "apg.session")],
        )
        .unwrap();
        let (scanned, planned_fqns) = code_universes_from_export(&apg_root).unwrap();
        assert_eq!(
            layers::classify_code_ref(candidate, &scanned, &planned_fqns),
            layers::CodeRefStatus::Pending,
            "a declared-but-unscanned planned FQN is pending, never drift"
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-8: the projection delta converges with no residue —
    /// add-then-remove and remove-then-add of the same node both converge, with
    /// no duplicate/stale rows and no orphan edges.
    #[test]
    fn projection_delta_converges_without_residue() {
        let (apg_root, repo, _wt) = project_fixture("delta-converge");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();

        let with_note2 = {
            let mut r = baseline.clone();
            r.push(Record::Note {
                fqn: "foo/note-2".into(),
                body: "second".into(),
                kind: "background".into(),
            });
            r.push(Record::Details {
                from: "foo/note-2".into(),
                to: "foo/plan".into(),
            });
            r
        };

        // Add: exactly one row and one edge.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &with_note2).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                1
            );
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:Note {fqn: 'foo/note-2'})-[:Details]->(:Plan) RETURN count(*)"
                ),
                1
            );
            assert_eq!(orphan_notes(&db), 0);
        }

        // A changed node (its body) is re-projected without duplicating rows or
        // dropping/re-adding its edges twice.
        let changed = {
            let mut r = with_note2.clone();
            for x in &mut r {
                if let Record::Note { fqn, body, .. } = x
                    && fqn == "foo/note-1"
                {
                    *body = "first (edited)".into();
                }
            }
            r
        };
        write_jsonl_and_reingest(&apg_root, &path, "foo", &changed).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-1'}) RETURN count(*)"),
                1,
                "a changed node must not leave a stale duplicate row"
            );
            assert_eq!(
                count(&db.db, "MATCH ()-[:Details]->() RETURN count(*)"),
                2,
                "both Details edges survive the changed-node re-merge"
            );
        }

        // Remove: the node and its edge are gone, no orphan/duplicate residue.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                0,
                "a removed node must not linger"
            );
            assert_eq!(
                count(&db.db, "MATCH ()-[:Details]->() RETURN count(*)"),
                1,
                "the removed node's edge must not linger"
            );
            assert_eq!(orphan_notes(&db), 0);
        }

        // Remove-then-add: re-adding converges to exactly one row/edge.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &with_note2).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                1,
                "remove-then-add must not duplicate the row"
            );
            assert_eq!(
                count(
                    &db.db,
                    "MATCH (:Note {fqn: 'foo/note-2'})-[:Details]->(:Plan) RETURN count(*)"
                ),
                1,
                "remove-then-add must not duplicate the edge"
            );
            assert_eq!(orphan_notes(&db), 0);
        }

        testutil::remove(&repo);
    }

    /// Phase-05 task-10 (int): a forced mid-apply re-ingest failure rolls the
    /// projection back to the prior state and the mutation reports failure. The
    /// durable file write landed FIRST (commit-then-project), so the committed
    /// JSONL holds the new state while the projection stays prior — and the
    /// next apply reproduces the committed state.
    #[test]
    fn projection_apply_failure_rolls_back_and_reports() {
        let (apg_root, repo, _wt) = project_fixture("projection-rollback");
        let path = specs::plan_jsonl_path(&apg_root, "foo");
        let baseline = baseline_records();
        write_jsonl_and_reingest(&apg_root, &path, "foo", &baseline).unwrap();

        let mut mutated = baseline.clone();
        mutated.push(Record::Note {
            fqn: "foo/note-2".into(),
            body: "second".into(),
            kind: "background".into(),
        });
        mutated.push(Record::Details {
            from: "foo/note-2".into(),
            to: "foo/plan".into(),
        });

        install_projection_hook(|| anyhow::bail!("forced mid-apply re-ingest failure"));
        let err = write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap_err();
        assert!(format!("{err:#}").contains("forced mid-apply"), "{err:#}");

        // The projection rolled back to the prior state.
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                0,
                "the failed mutation must not leave a projected row"
            );
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-1'}) RETURN count(*)"),
                1,
                "the prior projection must survive the rollback"
            );
            assert_eq!(orphan_notes(&db), 0);
        }
        // The durable write landed FIRST: the committed JSONL is the new state.
        assert_eq!(specs::read_jsonl(&path).unwrap(), mutated);
        let leftovers: Vec<_> = specs::jsonl_files(&apg_root.join(specs::TRANS).join("plans"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");

        // The failure was one-shot: the next apply reproduces the committed
        // state.
        write_jsonl_and_reingest(&apg_root, &path, "foo", &mutated).unwrap();
        {
            let db = ArtifactDb::open(&apg_root).unwrap();
            assert_eq!(
                count(&db.db, "MATCH (n:Note {fqn: 'foo/note-2'}) RETURN count(*)"),
                1,
                "the committed state must be reproducible"
            );
        }

        testutil::remove(&repo);
    }
}
