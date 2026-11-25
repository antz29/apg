//! The redb substrate of the shared fact store: one typed multi-writer database
//! at `<git-common-dir>/apg/facts.db`, shared by every worktree and branch of
//! the repository.
//!
//! Opening ([`Store::open`]) first takes the external advisory writer gate
//! [`StoreLock`] on `facts.lock` beside the database, so an open behind another
//! process's live open/write transaction waits at most [`WRITER_WAIT_BUDGET`]
//! and then fails with the named [`contention_error`] rather than blocking
//! inside redb. With the gate held it sets [`ConcurrencyMode::MultiWriter`] and
//! an integrity check on open, and records [`FACTS_FORMAT_VERSION`] in the `meta`
//! table — redb 5.0.0 exposes no database `user_version`, so the meta row is
//! the format marker. A recorded format other than [`FACTS_FORMAT_VERSION`], an
//! integrity failure, or any other open failure drops the handle, removes the
//! database file (with its sidecars) and rebuilds it empty. Reads take no lock.
//! A caller opens the one write transaction it needs through
//! [`Store::begin_write`], which carries the same `facts.lock` for exactly that
//! transaction's lifetime and releases it at `commit`/drop — the gate is never
//! held while a frontend runs.
//!
//! The row shapes are fixed here because the phases 4 and 5 consumers may not
//! change them: every `refs`, `scans`, `deps`, `signatures` and `overloads` row
//! is scoped by [`WorktreeId`]; the units key is the five-part [`UnitKey`]
//! (format, config digest, language, repo-relative path, blob OID) with `inputs`
//! carried as a diagnostic column, never as part of the reuse predicate; and
//! the body tables move raw encoded bytes addressed by [`BodyId`], leaving the
//! body codec to [`crate::cache::body`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use redb::{
    ConcurrencyMode, Database, Key, ReadOnlyTable, ReadableDatabase, ReadableTable,
    ReadableTableMetadata, TableDefinition, TableError, TableHandle, Value,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::cache::body::BodyId;

/// The fact store's explicit format version: the rebuild predicate. This
/// generation carries the manifest-free per-worktree scan row and the per-path
/// refs writes, so a store written at format 1 is dropped and rebuilt on open —
/// the one full scan the `fact-format-change-rebuilds-the-store` requirement
/// sanctions for correctness. [`crate::cache::CacheKey`] folds this constant in
/// its `format` field, so no pre-change scan row or refs set is ever mixed with
/// the new encoding.
pub const FACTS_FORMAT_VERSION: u32 = 2;

/// How long [`Store::begin_write`] waits for a competing writer before failing
/// with [`contention_error`].
pub const WRITER_WAIT_BUDGET: Duration = Duration::from_secs(30);

/// The pause between `try_begin_write` attempts while waiting out a competing
/// writer.
const WRITER_WAIT_POLL: Duration = Duration::from_millis(50);

/// The external advisory lock file beside `facts.db`: the cross-process gate
/// redb's own open cannot provide.
pub const LOCK_FILE: &str = "facts.lock";

/// The lock path for a store at `db`: [`LOCK_FILE`] beside it in the same
/// directory (`<root>/facts.db` -> `<root>/facts.lock`).
pub fn lock_path(db: &Path) -> PathBuf {
    let dir = db.parent().unwrap_or_else(|| Path::new(""));
    dir.join(LOCK_FILE)
}

/// The `meta` table key holding [`FACTS_FORMAT_VERSION`].
const META_FORMAT_KEY: &str = "format";

/// One worktree's identity: a stable hash of its canonical checkout root,
/// derived by [`crate::cache::manifest`] and accepted here at every
/// worktree-scoped accessor. The store never derives it.
pub type WorktreeId = [u8; 16];

/// The five-part content-and-config identity of one cached fact unit. `inputs`
/// is deliberately absent: a unit is reused on this key alone, and `inputs` is
/// recorded with the row only as a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct UnitKey {
    pub format: u32,
    pub config_digest: String,
    pub lang: String,
    pub rel: String,
    pub oid: String,
}

/// One stored unit: the encoded body, the diagnostic inputs digest, and the
/// absolute scan root the body was written from (the reading worktree re-bases
/// the body's absolute paths off it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct UnitRow {
    pub body: BodyId,
    pub inputs: String,
    pub writer_root: String,
}

/// One language's module-scaffolding identity under a format + config digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ScaffoldingKey {
    pub format: u32,
    pub config_digest: String,
    pub lang: String,
}

/// The declared tables. Keys are canonical bytes: worktree-scoped rows key on
/// the 16-byte [`WorktreeId`] (refs prepend the `/`-joined repo-relative path),
/// typed keys are postcard-encoded, and every body or encoded value is bytes.
const META: TableDefinition<&str, u32> = TableDefinition::new("meta");
const BODIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("bodies");
const UNITS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("units");
const REFS: TableDefinition<&[u8], &str> = TableDefinition::new("refs");
const SCANS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("scans");
const SCAFFOLDING: TableDefinition<&[u8], &[u8]> = TableDefinition::new("scaffolding");
const DEPS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("deps");
const SIGNATURES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("signatures");
const OVERLOADS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("overloads");
const WORKTREES: TableDefinition<&[u8], &str> = TableDefinition::new("worktrees");
const INTENTS: TableDefinition<&[u8], u64> = TableDefinition::new("intents");

/// Every declared table's name — the named schema a fresh store materializes
/// as its rows land. Pure, so the schema can be asserted without opening a
/// database.
pub fn table_names() -> Vec<&'static str> {
    vec![
        META.name(),
        BODIES.name(),
        UNITS.name(),
        REFS.name(),
        SCANS.name(),
        SCAFFOLDING.name(),
        DEPS.name(),
        SIGNATURES.name(),
        OVERLOADS.name(),
        WORKTREES.name(),
        INTENTS.name(),
    ]
}

/// True when a stored database recording `recorded` must be discarded: the
/// format marker is absent (a fresh or foreign database) or carries another
/// version.
pub fn format_needs_rebuild(recorded: Option<u32>) -> bool {
    recorded != Some(FACTS_FORMAT_VERSION)
}

/// The named contention failure: the store file, and how long the caller waited
/// on the external `facts.lock` gate before giving up. Typed so callers can
/// distinguish contention ([`is_contention`]) from every other store failure.
#[derive(Debug)]
pub struct Contention {
    pub path: PathBuf,
    pub waited: Duration,
}

impl std::fmt::Display for Contention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "fact store `{}` is held by another writer; waited {:?} for the write lock (budget {WRITER_WAIT_BUDGET:?})",
            self.path.display(),
            self.waited
        )
    }
}

impl std::error::Error for Contention {}

/// The named contention error: another writer held the external `facts.lock`
/// gate past the budget.
pub fn contention_error(path: &Path, waited: Duration) -> anyhow::Error {
    anyhow::Error::new(Contention {
        path: path.to_path_buf(),
        waited,
    })
}

/// True when `err` is the named writer contention, not an ordinary store
/// failure.
pub fn is_contention(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Contention>().is_some()
}

/// True once the writer wait has spent its whole budget.
fn writer_wait_exhausted(waited: Duration) -> bool {
    waited >= WRITER_WAIT_BUDGET
}

/// The held external writer gate: an exclusive advisory lock on `facts.lock`,
/// released when the owned file is dropped (or explicitly unlocked).
pub struct StoreLock {
    file: std::fs::File,
    path: PathBuf,
}

impl StoreLock {
    /// Takes the external writer gate for the store at `db`, creating the
    /// lock file (and its directory) on demand. A live holder is waited out for
    /// at most [`WRITER_WAIT_BUDGET`], then failure is the named
    /// [`contention_error`]; every other I/O failure propagates.
    pub fn acquire(db: &Path) -> Result<StoreLock> {
        let path = lock_path(db);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating the fact-store directory `{}`", parent.display())
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening the fact-store lock `{}`", path.display()))?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(StoreLock { file, path }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    let waited = started.elapsed();
                    if writer_wait_exhausted(waited) {
                        return Err(contention_error(db, waited));
                    }
                    std::thread::sleep(WRITER_WAIT_POLL);
                }
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(err).with_context(|| {
                        format!("locking the fact-store lock `{}`", path.display())
                    });
                }
            }
        }
    }

    /// The lock file path this guard holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The opened fact store. Reads and writes happen through explicit
/// [`ReadTxn`]/[`WriteTxn`] handles so a caller can hold exactly one write
/// transaction across a record sequence; the handle itself holds no lock — the
/// external gate is taken during open and for a write transaction only.
pub struct Store {
    db: Database,
    path: PathBuf,
}

impl Store {
    /// Opens (or creates) the store at `path` under the external
    /// [`StoreLock`], creating its parent directory. A live holder is waited
    /// out for at most [`WRITER_WAIT_BUDGET`] and then fails with
    /// [`contention_error`]. A missing format marker, another format version,
    /// or any open/integrity failure discards the file and rebuilds it empty at
    /// [`FACTS_FORMAT_VERSION`]. The gate is released before the handle is
    /// returned; no write transaction is taken to open an already-valid store.
    pub fn open(path: &Path) -> Result<Store> {
        let lock = StoreLock::acquire(path)?;
        let db = if path.exists() {
            match Self::database(path, false) {
                Ok(db) => match recorded_format(&db) {
                    Ok(recorded) if !format_needs_rebuild(recorded) => db,
                    Ok(recorded) => {
                        drop(db);
                        Self::rebuild(
                            path,
                            &format!("the recorded facts format {recorded:?} is not current"),
                        )?
                    }
                    Err(err) => {
                        drop(db);
                        Self::rebuild(path, &format!("reading the recorded facts format: {err:#}"))?
                    }
                },
                Err(err) => Self::rebuild(path, &format!("opening the fact store: {err:#}"))?,
            }
        } else {
            Self::rebuild(path, "the fact store does not exist yet")?
        };
        // The cached handle holds no external lock: release the gate before
        // handing the store back.
        drop(lock);
        Ok(Store {
            db,
            path: path.to_path_buf(),
        })
    }

    /// Opens the redb database at `path` with the frozen builder settings.
    fn database(path: &Path, create: bool) -> Result<Database> {
        let mut builder = Database::builder();
        builder
            .set_concurrency_mode(ConcurrencyMode::MultiWriter)
            .set_check_integrity_on_open(true);
        let db = if create {
            builder.create(path)
        } else {
            builder.open(path)
        };
        db.with_context(|| format!("opening the fact store `{}`", path.display()))
    }

    /// Drops whatever is at `path`, creates an empty database and stamps the
    /// format marker. Returns the new handle.
    fn rebuild(path: &Path, reason: &str) -> Result<Database> {
        remove_db_files(path)
            .with_context(|| format!("rebuilding the fact store `{}`: {reason}", path.display()))?;
        let db = Self::database(path, true)
            .with_context(|| format!("recording the fact-store format: {reason}"))?;
        let txn = db
            .begin_write()
            .context("recording the fact-store format")?;
        txn.open_table(META)?
            .insert(META_FORMAT_KEY, FACTS_FORMAT_VERSION)?;
        txn.commit().context("recording the fact-store format")?;
        Ok(db)
    }

    /// Opens the one write transaction a caller holds across a record
    /// sequence under the external `facts.lock` gate, waiting out a competing
    /// writer for at most [`WRITER_WAIT_BUDGET`] and carrying the held gate in
    /// the [`WriteTxn`] until `commit`/drop. The bounded `try_begin_write` loop
    /// stays as defence in depth for direct in-process contenders. Any error
    /// other than contention propagates.
    pub fn begin_write(&self) -> Result<WriteTxn> {
        let lock = StoreLock::acquire(&self.path)?;
        let started = Instant::now();
        loop {
            match self.db.try_begin_write() {
                Ok(txn) => {
                    return Ok(WriteTxn { txn, _lock: lock });
                }
                Err(redb::TransactionError::WouldBlock) => {
                    let waited = started.elapsed();
                    if writer_wait_exhausted(waited) {
                        return Err(contention_error(&self.path, waited));
                    }
                    std::thread::sleep(WRITER_WAIT_POLL);
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("opening a write transaction on `{}`", self.path.display())
                    });
                }
            }
        }
    }

    /// Opens a read transaction (no write lock, no writer wait).
    pub fn read(&self) -> Result<ReadTxn> {
        let txn = self
            .db
            .begin_read()
            .with_context(|| format!("opening a read transaction on `{}`", self.path.display()))?;
        Ok(ReadTxn { txn })
    }
}

/// Removes `path` and its `-wal`/`-shm` sidecars, tolerating absence.
fn remove_db_files(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        sidecar(path, "-wal"),
        sidecar(path, "-shm"),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("removing `{}`", candidate.display()));
            }
        }
    }
    Ok(())
}

/// A sibling of `path` carrying `suffix` (`facts.db` -> `facts.db-wal`).
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The format marker a live database records, or `None` when it has no `meta`
/// table yet.
fn recorded_format(db: &Database) -> Result<Option<u32>> {
    let txn = db.begin_read().context("reading the fact-store format")?;
    match txn.open_table(META) {
        Ok(table) => Ok(table
            .get(META_FORMAT_KEY)
            .context("reading the fact-store format marker")?
            .map(|guard| guard.value())),
        Err(TableError::TableDoesNotExist(_)) => Ok(None),
        Err(err) => Err(err).context("opening the fact-store meta table"),
    }
}

/// Canonical postcard bytes of a typed key or row.
fn to_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(value).context("postcard-encoding a fact-store row")
}

/// Decodes canonical postcard bytes back into a typed key or row.
fn from_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    postcard::from_bytes(bytes).context("postcard-decoding a fact-store row")
}

/// The canonical units-table key.
fn unit_key(key: &UnitKey) -> Result<Vec<u8>> {
    to_bytes(key)
}

/// The canonical scaffolding-table key.
fn scaffolding_key(key: &ScaffoldingKey) -> Result<Vec<u8>> {
    to_bytes(key)
}

/// The canonical refs-table key: the 16-byte worktree id followed by the
/// repo-relative path, so one worktree's rows are a contiguous byte prefix.
fn ref_key(worktree: WorktreeId, rel: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(worktree.len() + rel.len());
    key.extend_from_slice(&worktree);
    key.extend_from_slice(rel.as_bytes());
    key
}

/// Decodes a 16-byte worktree id key.
fn decode_worktree(bytes: &[u8]) -> Result<WorktreeId> {
    bytes
        .try_into()
        .with_context(|| format!("a worktree key must be 16 bytes, found {}", bytes.len()))
}

/// Decodes a 32-byte body id key.
fn decode_body(bytes: &[u8]) -> Result<BodyId> {
    bytes
        .try_into()
        .with_context(|| format!("a body key must be 32 bytes, found {}", bytes.len()))
}

/// Opens a read table, treating a table a fresh store has not materialized yet
/// as empty.
fn read_table<K: Key + 'static, V: Value + 'static>(
    txn: &redb::ReadTransaction,
    definition: TableDefinition<K, V>,
) -> Result<Option<ReadOnlyTable<K, V>>> {
    match txn.open_table(definition) {
        Ok(table) => Ok(Some(table)),
        Err(TableError::TableDoesNotExist(_)) => Ok(None),
        Err(err) => Err(anyhow::Error::from(err)).context("opening a fact-store table"),
    }
}

/// One write transaction, held by the caller from the first write to
/// [`WriteTxn::commit`], carrying the external `facts.lock` gate for exactly
/// that lifetime (released at commit/drop). Every mutation is a method here so
/// the whole record sequence lands atomically.
pub struct WriteTxn {
    txn: redb::WriteTransaction,
    _lock: StoreLock,
}

impl WriteTxn {
    /// Persists one distinct encoded body under its content address.
    pub fn put_body(&self, id: BodyId, bytes: &[u8]) -> Result<()> {
        self.txn.open_table(BODIES)?.insert(id.as_slice(), bytes)?;
        Ok(())
    }

    /// Removes one body row.
    pub fn delete_body(&self, id: BodyId) -> Result<()> {
        self.txn.open_table(BODIES)?.remove(id.as_slice())?;
        Ok(())
    }

    /// Persists one fact unit under its five-part key, recording the
    /// diagnostic inputs digest and the writing checkout root beside the body.
    pub fn put_unit(
        &self,
        key: &UnitKey,
        body: BodyId,
        inputs: &str,
        writer_root: &str,
    ) -> Result<()> {
        let row = UnitRow {
            body,
            inputs: inputs.to_string(),
            writer_root: writer_root.to_string(),
        };
        self.txn
            .open_table(UNITS)?
            .insert(unit_key(key)?.as_slice(), to_bytes(&row)?.as_slice())?;
        Ok(())
    }

    /// Removes one unit row.
    pub fn delete_unit(&self, key: &UnitKey) -> Result<()> {
        self.txn
            .open_table(UNITS)?
            .remove(unit_key(key)?.as_slice())?;
        Ok(())
    }

    /// Replaces one worktree's whole `path -> blob OID` manifest.
    pub fn replace_refs(
        &self,
        worktree: WorktreeId,
        entries: &BTreeMap<String, String>,
    ) -> Result<()> {
        let mut table = self.txn.open_table(REFS)?;
        let mut stale: Vec<Vec<u8>> = Vec::new();
        for row in table.range(worktree.as_slice()..)? {
            let (key, _) = row?;
            let key = key.value();
            if !key.starts_with(&worktree) {
                break;
            }
            stale.push(key.to_vec());
        }
        for key in stale {
            table.remove(key.as_slice())?;
        }
        for (rel, oid) in entries {
            table.insert(ref_key(worktree, rel).as_slice(), oid.as_str())?;
        }
        Ok(())
    }

    /// Applies only the changed refs rows for one worktree: upserts each
    /// `path -> OID` and removes each vanished path. The worktree's rows stay
    /// contiguous; nothing else is touched.
    pub fn apply_refs(
        &self,
        worktree: WorktreeId,
        upserts: &BTreeMap<String, String>,
        removals: &BTreeSet<String>,
    ) -> Result<()> {
        let mut table = self.txn.open_table(REFS)?;
        for rel in removals {
            table.remove(ref_key(worktree, rel).as_slice())?;
        }
        for (rel, oid) in upserts {
            table.insert(ref_key(worktree, rel).as_slice(), oid.as_str())?;
        }
        Ok(())
    }

    /// Persists one worktree's current scan row (opaque encoded bytes).
    pub fn put_scan(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.txn
            .open_table(SCANS)?
            .insert(worktree.as_slice(), bytes)?;
        Ok(())
    }

    /// Persists one language's scaffolding body under its format+config+lang
    /// key.
    pub fn put_scaffolding(&self, key: &ScaffoldingKey, body: BodyId) -> Result<()> {
        self.txn
            .open_table(SCAFFOLDING)?
            .insert(scaffolding_key(key)?.as_slice(), body.as_slice())?;
        Ok(())
    }

    /// Removes one language's scaffolding row.
    pub fn delete_scaffolding(&self, key: &ScaffoldingKey) -> Result<()> {
        self.txn
            .open_table(SCAFFOLDING)?
            .remove(scaffolding_key(key)?.as_slice())?;
        Ok(())
    }

    /// Persists one worktree's file-dependency rows (opaque encoded bytes).
    pub fn put_deps(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.txn
            .open_table(DEPS)?
            .insert(worktree.as_slice(), bytes)?;
        Ok(())
    }

    /// Removes one worktree's file-dependency row.
    pub fn delete_deps(&self, worktree: WorktreeId) -> Result<()> {
        self.txn.open_table(DEPS)?.remove(worktree.as_slice())?;
        Ok(())
    }

    /// Persists one worktree's signature rows (opaque encoded bytes).
    pub fn put_signatures(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.txn
            .open_table(SIGNATURES)?
            .insert(worktree.as_slice(), bytes)?;
        Ok(())
    }

    /// Removes one worktree's signature row.
    pub fn delete_signatures(&self, worktree: WorktreeId) -> Result<()> {
        self.txn
            .open_table(SIGNATURES)?
            .remove(worktree.as_slice())?;
        Ok(())
    }

    /// Persists one worktree's overload index (opaque encoded bytes).
    pub fn put_overloads(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.txn
            .open_table(OVERLOADS)?
            .insert(worktree.as_slice(), bytes)?;
        Ok(())
    }

    /// Removes one worktree's overload row.
    pub fn delete_overloads(&self, worktree: WorktreeId) -> Result<()> {
        self.txn
            .open_table(OVERLOADS)?
            .remove(worktree.as_slice())?;
        Ok(())
    }

    /// Registers a worktree's canonical checkout root.
    pub fn register_worktree(&self, worktree: WorktreeId, root: &str) -> Result<()> {
        self.txn
            .open_table(WORKTREES)?
            .insert(worktree.as_slice(), root)?;
        Ok(())
    }

    /// Removes a worktree and every row scoped to it.
    pub fn remove_worktree(&self, worktree: WorktreeId) -> Result<()> {
        self.txn
            .open_table(WORKTREES)?
            .remove(worktree.as_slice())?;
        self.delete_deps(worktree)?;
        self.delete_signatures(worktree)?;
        self.delete_overloads(worktree)?;
        self.txn.open_table(SCANS)?.remove(worktree.as_slice())?;
        let refs = self.ref_keys(worktree)?;
        if !refs.is_empty() {
            let mut table = self.txn.open_table(REFS)?;
            for key in refs {
                table.remove(key.as_slice())?;
            }
        }
        Ok(())
    }

    /// The refs keys of one worktree (the contiguous prefix block).
    fn ref_keys(&self, worktree: WorktreeId) -> Result<Vec<Vec<u8>>> {
        let table = self.txn.open_table(REFS)?;
        let mut keys = Vec::new();
        for row in table.range(worktree.as_slice()..)? {
            let (key, _) = row?;
            let key = key.value();
            if !key.starts_with(&worktree) {
                break;
            }
            keys.push(key.to_vec());
        }
        Ok(keys)
    }

    /// Records one worktree's scan intent (unix seconds).
    pub fn put_intent(&self, worktree: WorktreeId, at: u64) -> Result<()> {
        self.txn
            .open_table(INTENTS)?
            .insert(worktree.as_slice(), at)?;
        Ok(())
    }

    /// Clears one worktree's scan intent.
    pub fn remove_intent(&self, worktree: WorktreeId) -> Result<()> {
        self.txn.open_table(INTENTS)?.remove(worktree.as_slice())?;
        Ok(())
    }

    /// The unit count visible to this transaction (its own pending writes
    /// included).
    pub fn len(&self) -> Result<usize> {
        let len = self.txn.open_table(UNITS)?.len()?;
        Ok(usize::try_from(len).unwrap_or(usize::MAX))
    }

    /// True when this transaction sees no stored unit.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Commits every write applied to this transaction.
    pub fn commit(self) -> Result<()> {
        self.txn.commit().context("committing the fact store")
    }
}

/// One read transaction over the last committed state.
pub struct ReadTxn {
    txn: redb::ReadTransaction,
}

impl ReadTxn {
    /// The stored bytes of one body, or `None` when it was never stored.
    pub fn get_body(&self, id: BodyId) -> Result<Option<Vec<u8>>> {
        let Some(table) = read_table(&self.txn, BODIES)? else {
            return Ok(None);
        };
        Ok(table
            .get(id.as_slice())?
            .map(|guard| guard.value().to_vec()))
    }

    /// True when the body is stored.
    pub fn body_exists(&self, id: BodyId) -> Result<bool> {
        Ok(self.get_body(id)?.is_some())
    }

    /// Every stored body id.
    pub fn iter_body_ids(&self) -> Result<Vec<BodyId>> {
        let Some(table) = read_table(&self.txn, BODIES)? else {
            return Ok(Vec::new());
        };
        let mut ids = Vec::new();
        for row in table.iter()? {
            let (key, _) = row?;
            ids.push(decode_body(key.value())?);
        }
        Ok(ids)
    }

    /// One stored unit's body, diagnostic inputs and writing root.
    pub fn get_unit(&self, key: &UnitKey) -> Result<Option<UnitRow>> {
        let Some(table) = read_table(&self.txn, UNITS)? else {
            return Ok(None);
        };
        let Some(row) = table.get(unit_key(key)?.as_slice())? else {
            return Ok(None);
        };
        Ok(Some(from_bytes(row.value())?))
    }

    /// True when the unit's five-part key is present, whatever its inputs.
    pub fn has_unit(&self, key: &UnitKey) -> Result<bool> {
        let Some(table) = read_table(&self.txn, UNITS)? else {
            return Ok(false);
        };
        Ok(table.get(unit_key(key)?.as_slice())?.is_some())
    }

    /// Every stored unit.
    pub fn iter_units(&self) -> Result<Vec<(UnitKey, UnitRow)>> {
        let Some(table) = read_table(&self.txn, UNITS)? else {
            return Ok(Vec::new());
        };
        let mut units = Vec::new();
        for row in table.iter()? {
            let (key, value) = row?;
            units.push((from_bytes(key.value())?, from_bytes(value.value())?));
        }
        Ok(units)
    }

    /// One worktree's `path -> blob OID` manifest.
    pub fn refs(&self, worktree: WorktreeId) -> Result<BTreeMap<String, String>> {
        Ok(self.iter_refs(worktree)?.into_iter().collect())
    }

    /// One worktree's manifest rows, in repo-relative path order.
    pub fn iter_refs(&self, worktree: WorktreeId) -> Result<Vec<(String, String)>> {
        let Some(table) = read_table(&self.txn, REFS)? else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        let base = worktree.len();
        for row in table.range(worktree.as_slice()..)? {
            let (key, value) = row?;
            let key = key.value();
            if !key.starts_with(&worktree) {
                break;
            }
            let rel = std::str::from_utf8(&key[base..])
                .context("a refs key carries a non-UTF-8 path")?
                .to_string();
            entries.push((rel, value.value().to_string()));
        }
        Ok(entries)
    }

    /// One worktree's current scan row bytes.
    pub fn get_scan(&self, worktree: WorktreeId) -> Result<Option<Vec<u8>>> {
        let Some(table) = read_table(&self.txn, SCANS)? else {
            return Ok(None);
        };
        Ok(table
            .get(worktree.as_slice())?
            .map(|guard| guard.value().to_vec()))
    }

    /// Every worktree's current scan row.
    pub fn iter_scans(&self) -> Result<Vec<(WorktreeId, Vec<u8>)>> {
        let Some(table) = read_table(&self.txn, SCANS)? else {
            return Ok(Vec::new());
        };
        let mut scans = Vec::new();
        for row in table.iter()? {
            let (key, value) = row?;
            scans.push((decode_worktree(key.value())?, value.value().to_vec()));
        }
        Ok(scans)
    }

    /// One language's scaffolding body under its format+config+lang key.
    pub fn get_scaffolding(&self, key: &ScaffoldingKey) -> Result<Option<BodyId>> {
        let Some(table) = read_table(&self.txn, SCAFFOLDING)? else {
            return Ok(None);
        };
        let Some(body) = table.get(scaffolding_key(key)?.as_slice())? else {
            return Ok(None);
        };
        Ok(Some(decode_body(body.value())?))
    }

    /// Every stored scaffolding row.
    pub fn iter_scaffolding(&self) -> Result<Vec<(ScaffoldingKey, BodyId)>> {
        let Some(table) = read_table(&self.txn, SCAFFOLDING)? else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::new();
        for row in table.iter()? {
            let (key, body) = row?;
            rows.push((from_bytes(key.value())?, decode_body(body.value())?));
        }
        Ok(rows)
    }

    /// One worktree's encoded file-dependency bytes.
    pub fn get_deps(&self, worktree: WorktreeId) -> Result<Option<Vec<u8>>> {
        let Some(table) = read_table(&self.txn, DEPS)? else {
            return Ok(None);
        };
        Ok(table
            .get(worktree.as_slice())?
            .map(|guard| guard.value().to_vec()))
    }

    /// One worktree's encoded signature bytes.
    pub fn get_signatures(&self, worktree: WorktreeId) -> Result<Option<Vec<u8>>> {
        let Some(table) = read_table(&self.txn, SIGNATURES)? else {
            return Ok(None);
        };
        Ok(table
            .get(worktree.as_slice())?
            .map(|guard| guard.value().to_vec()))
    }

    /// One worktree's encoded overload-index bytes.
    pub fn get_overloads(&self, worktree: WorktreeId) -> Result<Option<Vec<u8>>> {
        let Some(table) = read_table(&self.txn, OVERLOADS)? else {
            return Ok(None);
        };
        Ok(table
            .get(worktree.as_slice())?
            .map(|guard| guard.value().to_vec()))
    }

    /// One worktree's canonical checkout root.
    pub fn get_worktree(&self, worktree: WorktreeId) -> Result<Option<String>> {
        let Some(table) = read_table(&self.txn, WORKTREES)? else {
            return Ok(None);
        };
        Ok(table
            .get(worktree.as_slice())?
            .map(|guard| guard.value().to_string()))
    }

    /// Every registered worktree and its checkout root.
    pub fn iter_worktrees(&self) -> Result<Vec<(WorktreeId, String)>> {
        let Some(table) = read_table(&self.txn, WORKTREES)? else {
            return Ok(Vec::new());
        };
        let mut worktrees = Vec::new();
        for row in table.iter()? {
            let (key, root) = row?;
            worktrees.push((decode_worktree(key.value())?, root.value().to_string()));
        }
        Ok(worktrees)
    }

    /// One worktree's scan-intent timestamp (unix seconds).
    pub fn get_intent(&self, worktree: WorktreeId) -> Result<Option<u64>> {
        let Some(table) = read_table(&self.txn, INTENTS)? else {
            return Ok(None);
        };
        Ok(table.get(worktree.as_slice())?.map(|guard| guard.value()))
    }

    /// Every live scan intent.
    pub fn iter_intents(&self) -> Result<Vec<(WorktreeId, u64)>> {
        let Some(table) = read_table(&self.txn, INTENTS)? else {
            return Ok(Vec::new());
        };
        let mut intents = Vec::new();
        for row in table.iter()? {
            let (key, at) = row?;
            intents.push((decode_worktree(key.value())?, at.value()));
        }
        Ok(intents)
    }

    /// The stored unit count.
    pub fn len(&self) -> Result<usize> {
        let Some(table) = read_table(&self.txn, UNITS)? else {
            return Ok(0);
        };
        let len = table.len()?;
        Ok(usize::try_from(len).unwrap_or(usize::MAX))
    }

    /// True when the store holds no unit.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests;
