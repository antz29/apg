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
const CODE_GRAPH_LABELS: [&str; 7] = [
    "Module",
    "File",
    "Struct",
    "Function",
    "UnresolvedTarget",
    "Scan",
    "Language",
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

/// Runs `RETURN count(*)` and returns the number. Public so the relocated e2e
/// integration crate (`tests/artifacts_e2e.rs`) reaches it as
/// `apg::artifacts::count`.
pub fn count(db: &Database, q: &str) -> i64 {
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

    /// Runs a query and returns its formatted output. Used by the test suite
    /// (the relocated e2e crates reach it as `apg::artifacts::ArtifactDb::q`);
    /// the shipping CLI paths use label-typed queries via
    /// [`count`](Self::count) or the query subcommand.
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
pub fn edge_merge(r: &Record) -> Option<(&'static str, &str, &str)> {
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
pub fn node_label_fqn(r: &Record) -> Option<(&'static str, &str)> {
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
        // back to its prior state (phase-05 task-10). Compiled unconditionally
        // (a no-op unless a test installs a hook) so the relocated
        // `tests/artifacts_e2e.rs` integration crate can reach
        // `install_projection_hook`.
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
// back to the prior projection (phase-05 task-10). Compiled unconditionally
// (never installed outside tests, so the hook is a no-op) so the relocated
// `tests/artifacts_e2e.rs` integration crate can reach
// `install_projection_hook`.
type ProjectionHook = Box<dyn FnOnce() -> anyhow::Result<()>>;

thread_local! {
    static PROJECTION_HOOK: std::cell::RefCell<Option<ProjectionHook>> =
        const { std::cell::RefCell::new(None) };
}

pub fn install_projection_hook(hook: impl FnOnce() -> anyhow::Result<()> + 'static) {
    PROJECTION_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

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
mod tests;
