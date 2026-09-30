use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use lbug::{Connection, Database, SystemConfig};

use crate::git;
use crate::load;
use crate::schema::Record;
use crate::specs;

use super::project::{assembled_records, reingest_project_with, transient_delta};

/// The four Implementation labels a plan can declare as a **planned** node. A
/// planned placeholder carries `status = 'planned'`; when a branch scan realizes
/// the FQN as real code the row loses that marker (`status IS NULL`), and it
/// must never be detached by a metadata-mutation delete (phase-05 task-1).
pub(crate) const PLANNED_CODE_LABELS: [&str; 4] = ["Module", "File", "Struct", "Function"];

/// Code-graph labels that are never metadata: the four Implementation labels
/// (which can also hold a planned placeholder — see [`PLANNED_CODE_LABELS`]),
/// the scan control node, and unresolved references. A metadata delta must
/// never detach `UnresolvedTarget`/`Scan` by FQN — they are scanned code, not
/// spec/plan state.
pub(crate) const CODE_GRAPH_LABELS: [&str; 7] = [
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
/// A live session takes it exactly once at `Coordinator::start`, before any
/// node-file read, and holds it for the session's whole life — across every
/// admitted mutation's buffered write plus the single commit at `apg session
/// save` and its projection. The direct plan/review authoring paths (and the
/// JSONL funnel they write through) take the same lock for their own sequence.
/// That single flock is what serializes the three contended locks a parallel
/// burst hits: the node-file read-modify-write, git's `.git/index.lock` (inside
/// `git::commit_files`), and the read-write `db.lbug` projection apply. Because
/// the session holds it for its life, it and any direct writer are mutually
/// exclusive.
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
/// projection), a durable target this funnel serves (anything outside the
/// gitignored `apg/.trans/`) is committed on the project branch via git2,
/// single-file diffs, and the staleness gate's recorded `scan_meta` is
/// re-anchored to the new state (DB and tree in sync by construction;
/// consecutive mutations do not each demand a rescan). Transient `.trans`
/// writes never commit: `apg/.trans` is gitignored and transient by design.
/// An auto-commit failure degrades to a warning on stderr: the mutation already
/// landed, and the staleness gate will demand a scan before the next one (the
/// same degradation as a hand-committed change).
///
/// Durable `apg node` / `apg edge` mutations do NOT pass through this funnel:
/// the live session admits them into its write-back buffer and projects each at
/// admission, and the whole buffered set becomes durable in exactly ONE commit
/// at `apg session save`. This funnel's per-write commit is therefore not the
/// durable-mutation contract.
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
}
