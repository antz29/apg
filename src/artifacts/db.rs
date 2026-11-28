use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use lbug::{Connection, Database, SystemConfig};

use crate::git;
#[cfg(test)]
use crate::layers::NodeProperties;
use crate::load;
use crate::schema::Record;
use crate::specs;
use crate::splice::seed::{cell, query_rows};

use super::project::{assembled_records, reingest_project_with, transient_delta};

/// The four Implementation labels a plan can declare as a **planned** node. A
/// planned placeholder carries `status = 'planned'`; when a branch scan realizes
/// the FQN as real code the row loses that marker (`status IS NULL`), and it
/// must never be detached by a metadata-mutation delete.
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

/// Acquires the exclusive cross-process lock that serializes a durable write
/// on the project: the **extended** spec/plan/review write lock.
///
/// The acquire is **blocking**: the exclusive `flock` waits until every other
/// holder has released it, so a concurrent writer queues behind this one
/// instead of failing with `EAGAIN`. That is what makes it the per-write
/// availability gate — `Session::open_for_write` takes it **before** opening
/// the database write-capable, and the `WriteGuard`'s drop releases it after
/// the caller closes the write-capable handle; the direct plan/review authoring
/// paths (and the JSONL funnel they write through) take the same lock for their
/// own sequence.
///
/// That single flock is what serializes the three contended locks a parallel
/// burst hits: the node-file read-modify-write, git's `.git/index.lock` (inside
/// `git::commit_files`), and the read-write `db.lbug` projection apply.
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
        // SAFETY: `f` is a just-created live file whose fd is valid for the call and kept in
        // `state.held` for its lifetime.
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
/// This is the **central mutation funnel** for the project's transient
/// bookkeeping — the plan store (`.trans/plans/<project>.jsonl`) and the five
/// feedback tier mirrors — plus the shared `_invariants.jsonl` ledger. Durable
/// `apg node` / `apg edge` mutations do NOT route through here: they are
/// node-file writes owned by
/// [`layers::write_project`](crate::layers::write_project). The funnel is
/// database-only — one atomic JSONL write, one commit for a durable target, one
/// projection transaction into `db.lbug` — with no write-back buffer and no
/// export step anywhere on the path, so the two gates beside each other cover
/// every write that reaches it:
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
/// single-file diffs, and the database's own `Scan` row is re-anchored to the
/// new state through that commit (DB and tree in sync by construction;
/// consecutive mutations do not each demand a rescan). Transient `.trans`
/// writes never commit:
/// `apg/.trans` is gitignored and transient by design.
/// An auto-commit failure degrades to a warning on stderr: the mutation already
/// landed, and the staleness gate will demand a scan before the next one (the
/// same degradation as a hand-committed change).
///
/// **Commit-then-project**: the durable/transient file
/// write and its commit land FIRST — the system-of-record durability point —
/// and only then is the exact projection delta applied to `db.lbug` in one
/// transaction. The delta is computed HERE, inside the funnel, from the
/// assembled transient record set before and after this write (no per-caller
/// threading): the delete set is exactly the removed ∪ changed FQNs plus the
/// sources of vanished edges ([`transient_delta`]), never a `<project>/` prefix.
/// A crash between the write and the projection is reproduced by the next
/// rebuild as the committed state, never the uncommitted projection. The rename
/// is still atomic (a sibling temp swapped over `path`), so a crash mid-write
/// never leaves half a JSONL.
pub fn write_jsonl_and_reingest(
    apg_root: &Path,
    path: &Path,
    project: &str,
    records: &[Record],
) -> anyhow::Result<()> {
    // Membership guard (R3): every write happens inside a project context.
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

    // 2. Auto-commit durable targets, then re-anchor the database's own `Scan`
    //    row. The re-anchor itself lands the post-commit git state on that row,
    //    so the projection below is database-only — no export step follows.
    if !path.starts_with(apg_root.join(specs::TRANS)) {
        match git::auto_commit(apg_root, path) {
            Ok(Some(_)) => {
                let state = git::git_state(apg_root);
                if let Err(e) = git::reanchor_scan_meta(apg_root, &state) {
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

/// One **removed durable identity** on the pending-removal ledger: the daemon-era
/// record of a removed `apg/layers/**` node. The table is retained so a database
/// left pending by the retired save model is detected rather than trusted; a
/// per-invocation write never inserts here. The DB table is `PendingRemoval`,
/// keyed on `fqn` alone; it is a node table but deliberately NOT a node label
/// (`load::tables::node_labels` omits it), so a detach delete never drops a
/// pending identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRemoval {
    pub fqn: String,
}

/// True when `fqn` resolves to a node in the live graph (any kind). Used by
/// the test suite and the tool surface; the shipping CLI paths test existence
/// via label queries.
fn node_exists(db: &Database, fqn: &str) -> bool {
    count(
        db,
        &format!("MATCH (n {{fqn: {}}}) RETURN count(*)", lit(fqn)),
    ) > 0
}

/// Runs `RETURN count(*)` and returns the number. Public so the e2e
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

/// The floor a `max_db_size` override must clear: 8 MiB. Below it the engine's
/// buffer manager cannot map a working database, so the override is refused
/// rather than silently clamped.
const MIN_MAX_DB_SIZE: u64 = 8 * 1024 * 1024;

/// Parse a byte-size override: a plain integer (`8388608`) or an integer with a
/// binary `K`/`M`/`G` suffix in either case (`64K`, `128M`, `1G`), which
/// multiplies by 1024, 1024² and 1024³ respectively.
fn parse_db_size(raw: &str) -> anyhow::Result<u64> {
    let s = raw.trim();
    let (digits, multiplier) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], 1024),
        Some('m' | 'M') => (&s[..s.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = digits.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "database max size `{raw}`: expected a byte size — a plain integer \
             (e.g. 8388608) or a binary K/M/G suffix (e.g. 8M, 1G)"
        )
    })?;
    n.checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("database max size `{raw}` overflows a byte count"))
}

/// Fold the optional `APG_DB_*` override inputs over an engine [`SystemConfig`],
/// purely: the inputs arrive as values, so this reads no environment, touches no
/// file and holds no global state.
///
/// `max_size` is a byte size (see [`parse_db_size`]) that must be **a power of
/// two** and **at least 8 MiB** (`8388608` B) — a value that is neither is
/// refused with an error naming the input, never clamped or dropped. `pool` sets
/// the buffer-pool byte size and `threads` the engine thread count as given.
/// With every input absent the base config is returned **unchanged**, including
/// `read_only` and every other field the caller set.
pub fn apply_db_overrides(
    base: SystemConfig,
    max_size: Option<&str>,
    pool: Option<u64>,
    threads: Option<u64>,
) -> anyhow::Result<SystemConfig> {
    if max_size.is_none() && pool.is_none() && threads.is_none() {
        return Ok(base);
    }
    let mut config = base;
    if let Some(raw) = max_size {
        let bytes = parse_db_size(raw)?;
        if bytes < MIN_MAX_DB_SIZE {
            anyhow::bail!(
                "database max size `{raw}` = {bytes} B is below the 8 MiB (8388608 B) floor"
            );
        }
        if !bytes.is_power_of_two() {
            anyhow::bail!("database max size `{raw}` = {bytes} B is not a power of two");
        }
        config = config.max_db_size(bytes);
    }
    if let Some(pool) = pool {
        config = config.buffer_pool_size(pool);
    }
    if let Some(threads) = threads {
        config = config.max_num_threads(threads);
    }
    Ok(config)
}

/// Read one `APG_DB_*` numeric override: absent is `None`, a plain unsigned
/// integer is the value, and anything else — a non-UTF-8 value included — is an
/// error naming the variable, never silently ignored.
fn env_u64(name: &str) -> anyhow::Result<Option<u64>> {
    match std::env::var(name) {
        Ok(raw) => raw.trim().parse().map(Some).map_err(|_| {
            anyhow::anyhow!("{name} `{raw}`: expected a plain unsigned integer (bytes)")
        }),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => anyhow::bail!("{name}: {err}"),
    }
}

/// The engine [`SystemConfig`] one database open uses: [`SystemConfig::default`]
/// with the site's `read_only` flag, then the fixture-sized
/// `APG_DB_MAX_SIZE` (byte size), `APG_DB_BUFFER_POOL` (bytes) and
/// `APG_DB_THREADS` (count) test overrides folded in through
/// [`apply_db_overrides`]. With all three unset the base config is returned
/// unchanged, so production behaviour is exactly the engine default.
///
/// These three variables are read **here and nowhere else** — no caller mutates
/// the process environment. They are test-only: an override that does not parse,
/// or a `max_size` below the 8 MiB floor or not a power of two, is a hard error
/// naming the input, never silently ignored or clamped.
pub fn system_config(read_only: bool) -> anyhow::Result<SystemConfig> {
    let max_size = match std::env::var("APG_DB_MAX_SIZE") {
        Ok(raw) => Some(raw),
        Err(std::env::VarError::NotPresent) => None,
        Err(err) => anyhow::bail!("APG_DB_MAX_SIZE: {err}"),
    };
    apply_db_overrides(
        SystemConfig::default().read_only(read_only),
        max_size.as_deref(),
        env_u64("APG_DB_BUFFER_POOL")?,
        env_u64("APG_DB_THREADS")?,
    )
}

/// Opens the database of the registered project `project` at `worktree` — the
/// only open path a project-bound caller may use. A missing database is that
/// project's problem (refused, naming the project, its worktree and the
/// database path), never a reason to read another checkout's database.
pub fn open_project_db(
    project: &str,
    worktree: &Path,
    read_only: bool,
) -> anyhow::Result<Database> {
    let db_path = worktree
        .join(specs::LAYOUT)
        .join(specs::TRANS)
        .join("db.lbug");
    if !db_path.exists() {
        anyhow::bail!(
            "refused: project `{project}` at worktree `{}` has no database `{}`. Fix: run `apg scan` inside `{}`.",
            worktree.display(),
            db_path.display(),
            worktree.display()
        );
    }
    Database::new(&db_path, system_config(read_only)?).map_err(|err| {
        anyhow::anyhow!(
            "project `{project}` at worktree `{}`: opening `{}`: {err}",
            worktree.display(),
            db_path.display()
        )
    })
}

/// Whether an open failure names the transient checkpoint-recovery class a
/// read-only open hits while a concurrent writer finishes its checkpoint — the
/// only failures [`ArtifactDb::open_read_only`] retries. Pure: it scans only the
/// error's whole message chain (`{err:#}`), so no I/O, clock or global state.
fn is_checkpoint_recovery_error(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}").to_lowercase();
    text.contains("checkpoint recovery is pending")
        || text.contains("cannot recover checkpoint wal")
        || (text.contains("checkpoint") && text.contains("shadow file"))
        || (text.contains("wal.checkpoint") && text.contains("past the end"))
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
        let db = Database::new(&db_path, system_config(false)?)?;
        Ok(ArtifactDb { db })
    }

    /// Opens the branch database in the **read-only** class — the constructor
    /// verdict paths use so a read takes no exclusive read-write class a
    /// concurrent reader or writer needs. The exclusive read-write class is
    /// reserved for writers; otherwise identical to [`open`](Self::open),
    /// including the missing-database refusal.
    ///
    /// A concurrent writer's checkpoint recovery makes the engine refuse a
    /// read-only open for the moment it runs, so the open is retried in small
    /// steps for a bounded ~5 s. Only that transient recovery class is retried;
    /// any other failure is returned at once, and the last recovery error is
    /// returned after the bound.
    pub fn open_read_only(apg_root: &Path) -> anyhow::Result<ArtifactDb> {
        let db_path = apg_root.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            anyhow::bail!(
                "{} does not exist — run `apg scan` first",
                db_path.display()
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match Database::new(&db_path, system_config(true)?) {
                Ok(db) => return Ok(ArtifactDb { db }),
                Err(err) => {
                    let err = anyhow::Error::from(err);
                    if !is_checkpoint_recovery_error(&err) || std::time::Instant::now() >= deadline
                    {
                        return Err(err);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// A fresh connection to the owned database (borrows `self`, so any
    /// returned rows must be consumed before the next call).
    pub fn conn(&self) -> anyhow::Result<Connection<'_>> {
        Ok(Connection::new(&self.db)?)
    }

    /// Runs a query and returns its formatted output. Used by the test suite
    /// (the e2e integration crates reach it as `apg::artifacts::ArtifactDb::q`);
    /// the shipping CLI paths use label-typed queries via
    /// [`count`](Self::count) or the query subcommand.
    pub fn q(&self, query: &str) -> anyhow::Result<String> {
        Ok(self.conn()?.query(query)?.to_string())
    }

    /// Existence of any node at `fqn` in the live graph. Used by the test suite
    /// and the tool surface; the shipping CLI paths test existence via label
    /// queries.
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
    /// placeholder awaiting realization — GraphModel-SPEC.md). Pending anchors
    /// are detected by `status: planned`, never a separate kind.
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

    /// The two code-reference universes `layers::validate_change_over` (and the
    /// direct `validate_change`) validate `implemented-by` targets against, read
    /// from the **live database this handle already holds** — the borrowed-handle
    /// twin of [`crate::artifacts::code_universes`], so the write-through never
    /// opens a second `db.lbug` handle nor reads `apg/.trans/graph.jsonl`:
    ///
    /// - `scanned` — the real code FQNs of the four Implementation labels
    ///   (`Module`/`File`/`Struct`/`Function`) whose `status` is not `planned`.
    /// - `planned` — the FQNs of those labels carrying `status = 'planned'` (a
    ///   plan-writer placeholder awaiting realization). The plan store's
    ///   `Record::PlannedNode` declarations are projected into exactly these rows
    ///   by the authoring paths, so the held DB reproduces
    ///   [`crate::artifacts::code_universes`]'s union without touching the
    ///   transient plan files.
    ///
    /// Unlike the free [`crate::artifacts::code_universes`] (which opens its own
    /// read-only handle), this borrows the caller's held handle, so the
    /// write-through reuses ONE open. `UnresolvedTarget` is
    /// deliberately excluded — an unresolved reference is not code.
    pub fn code_universes_from_db(&self) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
        let conn = self.conn()?;
        let mut scanned = BTreeSet::new();
        let mut planned = BTreeSet::new();
        for label in PLANNED_CODE_LABELS {
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

    /// DELETE-then-CREATE the single `Scan` row (fqn [`crate::schema::SCAN_HEAD`],
    /// `scan/HEAD`) on the **already-open** database, setting it to the same git
    /// state the caller just re-anchored the database's own `Scan` row to.
    ///
    /// This primitive is the write half of every durability point's
    /// reconciliation: a durable commit re-anchors the database's own `Scan` row
    /// through this primitive (`git::reanchor_scan_meta`, which opens the DB and
    /// writes the row), and this primitive brings that row to the identical
    /// state — the row it DELETE-then-CREATEs is the row
    /// [`crate::git::db_recorded_scan`] reads, so the database's account of
    /// itself can never disagree with the row written for it.
    ///
    /// The columns mirror the two existing writers exactly — the splice's step 9
    /// ([`crate::splice::apply`]) and [`crate::load::tables::build_load_files`]'s
    /// `Scan` table: `git_sha`/`content_key` are empty strings when `None`,
    /// `git_clean` renders `"true"`/`"false"` (empty when `None`), and
    /// `scanned_at` and `modules` are **preserved** from the row being replaced —
    /// a metadata mutation re-anchors, it never re-scans, so the scan's own
    /// timestamp and its recorded `--module` selection must survive.
    ///
    /// Runs on a fresh connection over the caller's handle, so the caller never
    /// reopens `db.lbug`.
    pub fn refresh_scan_row(
        &self,
        git_sha: Option<&str>,
        git_clean: Option<bool>,
        content_key: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn()?;
        let (_, rows) = query_rows(
            &conn,
            "MATCH (s:Scan) RETURN s.scanned_at AS scanned_at, s.modules AS modules",
        )?;
        let scanned_at = rows
            .first()
            .map(|row| cell(row, 0))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(crate::git::now_iso8601);
        let modules = rows.first().map(|row| cell(row, 1)).unwrap_or_default();
        conn.query("MATCH (s:Scan) DELETE s")?;
        conn.query(&format!(
            "CREATE (s:Scan {{fqn: {}, git_sha: {}, git_clean: {}, content_key: {}, scanned_at: {}, modules: {}}})",
            lit(crate::schema::SCAN_HEAD),
            lit(git_sha.unwrap_or("")),
            lit(&git_clean.map(|c| c.to_string()).unwrap_or_default()),
            lit(content_key.unwrap_or("")),
            lit(&scanned_at),
            lit(&modules),
        ))?;
        Ok(())
    }

    /// Insert-or-replace one identity on the **held handle**'s pending-removal
    /// ledger: `MERGE (n:PendingRemoval {fqn})`, keyed on the
    /// table's primary key, so recording the same identity twice is idempotent
    /// and never duplicates a row. Retained with the ledger for the retired save
    /// model — a per-invocation write never records here — and a database left
    /// pending by it is detected rather than trusted.
    ///
    /// Runs on a fresh connection over the caller's handle and touches no
    /// `apg/layers/**` file.
    pub fn record_pending_removal(&self, fqn: &str) -> anyhow::Result<()> {
        self.conn()?
            .query(&format!("MERGE (n:PendingRemoval {{fqn: {}}})", lit(fqn)))?;
        Ok(())
    }

    pub fn has_pending_state(&self) -> bool {
        let conn = match self.conn() {
            Ok(conn) => conn,
            Err(_) => return true,
        };
        let ledger: Vec<PendingRemoval> = match query_rows(
            &conn,
            "MATCH (n:PendingRemoval) RETURN n.fqn ORDER BY n.fqn",
        ) {
            Ok((_, rows)) => rows
                .into_iter()
                .map(|row| PendingRemoval { fqn: cell(&row, 0) })
                .collect(),
            Err(_) => return true,
        };
        pending_state_present(
            |table| {
                query_rows(
                    &conn,
                    &format!("MATCH (n:{table}) WHERE n.updated = 1 RETURN count(*)"),
                )
                .map(|(_, rows)| rows.first().is_none_or(|row| cell(row, 0) != "0"))
                .unwrap_or(true)
            },
            &ledger,
        )
    }
}

/// The **previous scan's database**, read once and carried forward by a rebuild:
/// the two code universes, the recorded Implementation kind of every code FQN,
/// and the `Agent` records the scan must re-emit. It owns **no** database
/// handle — [`PreviousScan::capture`] drops the one read-only handle it opens
/// before returning it — so the next scan is free to replace `db.lbug`.
///
/// Every field mirrors the previous database's rows:
#[derive(Debug, Clone, PartialEq)]
pub struct PreviousScan {
    /// The real code FQNs: the four Implementation labels whose `status` is not
    /// `planned` (see [`PLANNED_CODE_LABELS`]).
    pub scanned: BTreeSet<String>,
    /// The FQNs of those labels carrying `status = 'planned'` — declared code
    /// the previous scan had not yet seen realized.
    pub planned: BTreeSet<String>,
    /// Every code FQN's recorded kind, lowercased — `module`, `file`, `struct`
    /// or `function`, taken from the row's label (a planned row's kind is its
    /// declared label's kind too). The rebuild carries these so a realized
    /// planned node keeps its identity.
    pub kinds: BTreeMap<String, String>,
    /// The previous database's `Agent` rows as export records (`name` is the
    /// FQN with its `agent.` prefix stripped, matching
    /// [`crate::splice::seed`]'s record identity), the roster the rebuild must
    /// re-emit.
    pub agents: Vec<Record>,
    /// The content-identity key of the previous database's single `Scan` row —
    /// the tree the local `db.lbug` was built from — or `None` when the
    /// database carries no `Scan` row or an empty key (the shape
    /// [`crate::splice::seed::recorded_content_key`] reads).
    pub content_key: Option<String>,
}

impl PreviousScan {
    /// Capture the previous database's snapshot over **one** read-only handle on
    /// `apg/.trans/db.lbug`: the scanned and planned code universes (via
    /// [`ArtifactDb::code_universes_from_db`]), every code row's label as its
    /// kind, and every `Agent` row. This is the scan's snapshot of the database
    /// it is about to replace — the source for the previous-database
    /// read, splice eligibility, the kind map a rebuild carries for planned
    /// nodes, and the Agent records it re-emits.
    ///
    /// **Fails closed.** An absent database, an unopenable or unreadable one, a
    /// database carrying pending state (a dirty row or a pending-removal
    /// identity — a database left that way by the retired save model never seeds
    /// a rebuild), or
    /// any failed read yields `None`: never a partial snapshot and never a hard
    /// error, so the incremental/splice path cannot seed from an untrustworthy
    /// previous database.
    ///
    /// The handle is local to this call and is dropped before the caller's scan
    /// replaces the file; the returned struct holds no `Database`.
    pub fn capture(apg_root: &Path) -> Option<PreviousScan> {
        let db_path = apg_root.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            return None;
        }
        let db = Database::new(&db_path, system_config(true).ok()?).ok()?;
        let artifact = ArtifactDb { db };
        if artifact.has_pending_state() {
            return None;
        }
        let (scanned, planned) = artifact.code_universes_from_db().ok()?;
        let conn = artifact.conn().ok()?;
        let mut kinds = BTreeMap::new();
        for label in PLANNED_CODE_LABELS {
            let (_, rows) = query_rows(&conn, &format!("MATCH (n:{label}) RETURN n.fqn")).ok()?;
            for row in rows {
                let fqn = cell(&row, 0);
                if !fqn.is_empty() {
                    kinds.insert(fqn, label.to_ascii_lowercase());
                }
            }
        }
        let (_, rows) = query_rows(
            &conn,
            "MATCH (n:Agent) RETURN n.fqn, n.path, n.start_line, n.end_line ORDER BY n.fqn",
        )
        .ok()?;
        let mut agents = Vec::with_capacity(rows.len());
        for row in rows {
            let fqn = cell(&row, 0);
            let Some(name) = fqn.strip_prefix("agent.") else {
                continue;
            };
            agents.push(Record::Agent {
                name: name.to_string(),
                path: cell(&row, 1),
                start_line: cell(&row, 2).parse().unwrap_or(0),
                end_line: cell(&row, 3).parse().unwrap_or(0),
            });
        }
        let (_, rows) =
            query_rows(&conn, "MATCH (s:Scan) RETURN s.content_key AS content_key").ok()?;
        let content_key = rows.first().map(|r| cell(r, 0)).filter(|s| !s.is_empty());
        Some(PreviousScan {
            scanned,
            planned,
            kinds,
            agents,
            content_key,
        })
    }
}

/// Build the durable-authored edge [`Record`] for one `(a)-[r:TABLE]->(b)` row,
/// keyed by its rel table. The inverse of
/// [`edge_merge`](crate::artifacts::merge::edge_merge)'s table arm — every
/// `table` here is a durable authored edge kind, so the fallback is
/// unreachable.
#[cfg(test)]
pub(crate) fn durable_edge_record(
    table: &str,
    from: String,
    to: String,
    properties: NodeProperties,
) -> Record {
    match table {
        "Contains" => Record::Contains {
            from,
            to,
            properties,
        },
        "DrivenBy" => Record::DrivenBy {
            from,
            to,
            properties,
        },
        "SpecImplementedBy" => Record::SpecImplementedBy {
            from,
            to,
            properties,
        },
        "Calls" => Record::Calls {
            from,
            to,
            properties,
        },
        "Publishes" => Record::Publishes {
            from,
            to,
            properties,
        },
        "Subscribes" => Record::Subscribes {
            from,
            to,
            properties,
        },
        "Requires" => Record::Requires {
            from,
            to,
            properties,
        },
        "Reads" => Record::Reads {
            from,
            to,
            properties,
        },
        "Writes" => Record::Writes {
            from,
            to,
            properties,
        },
        "IsRole" => Record::IsRole {
            from,
            to,
            properties,
        },
        "Realises" => Record::Realises {
            from,
            to,
            properties,
        },
        "Uses" => Record::Uses {
            from,
            to,
            properties,
        },
        "Represents" => Record::Represents {
            from,
            to,
            properties,
        },
        "Details" => Record::Details {
            from,
            to,
            properties,
        },
        "AppliesTo" => Record::AppliesTo {
            from,
            to,
            properties,
        },
        other => unreachable!("no durable authored edge table `{other}`"),
    }
}

/// One durable node table's reconstruction shape: `(DB table label, non-FQN
/// columns in `create_schema` order, the body column, the metadata columns and
/// the node-file `properties` key each feeds)`.
///
/// `columns` includes the trailing `properties` serialized-properties column
/// (`properties_from_json` decodes it into the node file's full map); the
/// metadata columns are merged back over the decoded map so a table that
/// predates the column still reconstructs its typed keys.
type DurableNodeTable = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Every durable node table — the `apg/layers/**` node-file layers
/// (requirements, domain, solution, implementation, global) — with the metadata
/// columns a node file's `properties` map carries and the serialized-properties
/// column that carries the full map (including arbitrary keys and an `Entity`'s
/// `kind`). The transient `.trans` tables (`Feedback`, `Plan`, `PlanPhase`,
/// `Task`) are not node files and are absent.
const DURABLE_NODE_TABLES: &[DurableNodeTable] = &[
    (
        "Requirement",
        &["id", "title", "body", "feature", "properties"],
        "body",
        &[("id", "id"), ("feature", "feature")],
    ),
    ("Stakeholder", &["name", "body", "properties"], "body", &[]),
    ("User", &["name", "body", "properties"], "body", &[]),
    (
        "Note",
        &["body", "kind", "properties"],
        "body",
        &[("kind", "kind")],
    ),
    ("Constraint", &["name", "body", "properties"], "body", &[]),
    (
        "DomainGroup",
        &["name", "attribute", "root", "body", "properties"],
        "body",
        &[("attribute", "attribute"), ("root", "root")],
    ),
    ("Entity", &["name", "body", "properties"], "body", &[]),
    ("Value", &["name", "body", "properties"], "body", &[]),
    ("Service", &["name", "body", "properties"], "body", &[]),
    ("System", &["name", "body", "properties"], "body", &[]),
    (
        "Container",
        &["name", "kind", "body", "properties"],
        "body",
        &[("kind", "kind")],
    ),
    ("Component", &["name", "body", "properties"], "body", &[]),
    ("Person", &["name", "body", "properties"], "body", &[]),
    ("Actor", &["name", "body", "properties"], "body", &[]),
    ("Role", &["name", "body", "properties"], "body", &[]),
];

/// Decide whether a database carries **pending state** from pure inputs: one
/// per-table row answer over the durable node tables ([`DURABLE_NODE_TABLES`],
/// the `apg/layers/**` roster the retired save model marks `updated = 1`, walked
/// in its declared order so a table added to that roster is governed here by
/// construction) plus the pending-removal ledger.
///
/// Pending state is present iff the ledger holds at least one identity or at
/// least one durable node table reports a dirty row — the shape a database left
/// by the retired save model has exactly when it still owes a write or delete.
/// A per-invocation write never produces this state, so a current database is
/// always clean. The ledger is consulted
/// first: a pending deletion is pending state even when every durable row is
/// clean, and an empty ledger leaves the decision to the roster alone.
///
/// `row_present` is the caller's **per-table row answer** — the live query
/// decides what "a dirty row is present" means for one table label, and a fake
/// row source drives the same rule in tests — so the walk visits one label per
/// call, stopping at the first `true`. The rule itself opens no database,
/// touches no file and spawns no process; the handle-backed
/// `ArtifactDb::has_pending_state` and the path-level `db_has_pending_state`
/// both fold their row answers through it, so the two predicates agree by
/// construction.
pub fn pending_state_present<F>(mut row_present: F, pending_removals: &[PendingRemoval]) -> bool
where
    F: FnMut(&str) -> bool,
{
    if !pending_removals.is_empty() {
        return true;
    }
    DURABLE_NODE_TABLES
        .iter()
        .any(|&(table, _, _, _)| row_present(table))
}

/// Answer whether the database under an `apg/` layout root carries **pending
/// state** — the same question [`ArtifactDb::has_pending_state`] answers from a
/// held handle — with an **unreadable database reported as a read failure**
/// rather than as pending state. Three outcomes:
///
/// - `Ok(false)` — an **absent** `db.lbug`: there is nothing to discard, so
///   this is not an error and not a reason to refuse a scan;
/// - `Err` — the database cannot be read at all: a refused
///   [`system_config(true)`], a failed open, a refused connection, or a refused
///   pending-removal ledger read;
/// - `Ok(decision)` — the database is readable, and the ledger plus the
///   per-table row answers fold through the shared pure rule
///   [`pending_state_present`] — the same roster walk that rule drives for the
///   handle-backed sibling, never a second independent probe.
///
/// A **per-table probe error stays pending** (the `unwrap_or(true)` below): a
/// probe that errors means that table's *shape* differs (e.g. no `updated`
/// column), which the separate `validate_compatible` check labels. It does not
/// mean the database is unreadable, so only the open/connect/ledger failures
/// above are read failures. Dirty means exactly `updated = 1`; a row whose
/// `updated` is NULL is not pending. Opens no write handle and touches no
/// `apg/layers/**` file.
pub fn db_has_pending_state_result(apg_root: &Path) -> anyhow::Result<bool> {
    let db_path = apg_root.join(specs::TRANS).join("db.lbug");
    if !db_path.exists() {
        return Ok(false);
    }
    let db = Database::new(&db_path, system_config(true)?)?;
    let conn = Connection::new(&db)?;
    let (_, rows) = query_rows(
        &conn,
        "MATCH (n:PendingRemoval) RETURN n.fqn ORDER BY n.fqn",
    )?;
    let ledger: Vec<PendingRemoval> = rows
        .into_iter()
        .map(|row| PendingRemoval { fqn: cell(&row, 0) })
        .collect();
    Ok(pending_state_present(
        |table| {
            query_rows(
                &conn,
                &format!("MATCH (n:{table}) WHERE n.updated = 1 RETURN count(*)"),
            )
            .map(|(_, rows)| rows.first().is_none_or(|row| cell(row, 0) != "0"))
            .unwrap_or(true)
        },
        &ledger,
    ))
}

/// The fail-closed legacy projection of [`db_has_pending_state_result`]:
/// `true` for a pending database **and** for an unreadable one (`Err`), `false`
/// only for an absent one. Every reader that must never treat an unreadable
/// database as clean — `is_fresh`'s freshness evaluation, the scan and the
/// pipeline — keeps this answer; a caller that must
/// distinguish "unreadable now" from "pending" uses
/// [`db_has_pending_state_result`].
pub fn db_has_pending_state(apg_root: &Path) -> bool {
    db_has_pending_state_result(apg_root).unwrap_or(true)
}
