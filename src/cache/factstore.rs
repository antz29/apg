//! The shared content-addressed fact-store facade over the redb substrate
//! ([`crate::cache::store`]).
//!
//! The store is **one redb database per repository** — `<git-common-dir>/apg/facts.db`
//! beside the external `<git-common-dir>/apg/facts.lock` writer gate — shared by
//! every worktree and branch of the repository, so a fresh worktree reuses the
//! main checkout's units. [`FactStore::root`] is the store *directory*
//! (`<git-common-dir>/apg`): the phase-2 JSON records the phase-4 tasks have not
//! yet moved into redb (`manifest.json`, `scan.json`, the `index/` state) still
//! live there beside `facts.db`, and every call site (`pipeline`, `record`,
//! `warm`, `prepare`) passes that directory around.
//!
//! Opening is **lazy** and never takes a write transaction: [`FactStore::at`]
//! and [`FactStore::load`] only resolve the path, and the first read or write
//! opens the database. The open runs under the external `facts.lock` gate, so a
//! contended open waits a bounded time and then surfaces the named, typed
//! contention error ([`crate::cache::store::is_contention`]) instead of blocking
//! inside redb; the facade caches that failure and replays it as contention on
//! every later call. An ordinary open failure still degrades every read to a
//! cache miss (`None`/`false`/`0`) and surfaces on the write path. Reads run in a
//! read transaction over the last committed state, so a read during `record` sees
//! the previously committed rows — exactly what the delta scaffolding merge needs.
//! The first write ([`FactStore::put`] / [`FactStore::put_scaffolding`]) opens
//! **one** write transaction under the same gate that stays held — never across a
//! frontend run, and never on open or on a read — until
//! [`FactStore::save_index`] commits it.
//!
//! The legacy phase-2 `<store>/facts/` directory is retired once, on the first
//! successful open ([`retire_legacy_store`]): its bytes are reported and it is
//! removed best-effort. The checkout-local toolchain cache
//! (`<checkout>/apg/.trans/cache`, [`crate::cache::toolchain`]) is a different
//! directory and is never derived or touched here.

use std::cell::{OnceCell, Ref, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::cache::body;
use crate::cache::store::{
    Contention, FACTS_FORMAT_VERSION, ReadTxn, ScaffoldingKey, Store, UnitKey, WorktreeId,
    WriteTxn, contention_error,
};
use crate::cache::{CacheKey, FileFragment, ModuleScaffolding};
use crate::graph::NodeKind;
use crate::specs;

/// The subdirectory under the git common dir that hosts the repository's one
/// shared fact store.
pub const STORE_DIR: &str = "apg";
/// The redb database file inside the store directory: the repository's one
/// fact store, shared by every worktree and branch.
pub const FACTS_DB: &str = "facts.db";
/// **Legacy.** The phase-2 directory of JSON fact files beside `facts.db`. It is
/// retired on the first successful open; the name survives only so the
/// migration and its tests can refer to it.
pub const FACTS_DIR: &str = "facts";
/// **Legacy.** The phase-2 per-language scaffolding file name inside
/// [`FACTS_DIR`]. Module scaffolding now lives in the redb `scaffolding` table.
pub const SCAFFOLDING_FILE: &str = "scaffolding.json";

/// A cached open failure: enough to replay the typed contention error on every
/// later call, or an ordinary rendered message. A plain `String` is not enough —
/// it would flatten the named contention into an ordinary failure.
enum OpenFailure {
    Contention { path: PathBuf, waited: Duration },
    Other(String),
}

impl OpenFailure {
    fn from_error(err: anyhow::Error) -> OpenFailure {
        match err.downcast::<Contention>() {
            Ok(contention) => OpenFailure::Contention {
                path: contention.path,
                waited: contention.waited,
            },
            Err(err) => OpenFailure::Other(format!("{err:#}")),
        }
    }

    fn replay(&self) -> anyhow::Error {
        match self {
            OpenFailure::Contention { path, waited } => contention_error(path, *waited),
            OpenFailure::Other(message) => anyhow::anyhow!("{message}"),
        }
    }
}

/// The shared content-addressed fact store: the facade over the repository's one
/// redb database, rooted at the store directory `<git-common-dir>/apg`.
pub struct FactStore {
    /// The store **directory** (`<git-common-dir>/apg`); the database itself is
    /// `<root>/facts.db`.
    pub root: PathBuf,
    /// The lazily opened database, or the cached open failure — kept so a write
    /// surfaces it instead of silently dropping facts, and a contention replays
    /// as the typed, named error on every later call.
    db: OnceCell<Result<Store, OpenFailure>>,
    /// The one write transaction, opened by the first write and committed (and
    /// cleared) by [`FactStore::save_index`].
    write: RefCell<Option<WriteTxn>>,
}

impl FactStore {
    /// Resolves the store for the repository containing `apg_root`: the repo's
    /// **common** git dir (shared by linked worktrees) plus `apg`, opening
    /// `<root>/facts.db` and retiring the legacy `<root>/facts/` directory on
    /// the first successful open. A real open error — the named, typed
    /// contention included — propagates.
    pub fn resolve(apg_root: &Path) -> Result<FactStore> {
        let repo = git2::Repository::discover(apg_root)?;
        let root = common_git_dir(&repo).join(STORE_DIR);
        let store = FactStore::at(root);
        store.store()?;
        Ok(store)
    }

    /// The same store rooted at an explicit **store directory** (tests and
    /// callers that already resolved the common dir). The database inside it is
    /// opened lazily on first use; an open failure degrades reads to misses and
    /// surfaces on writes.
    pub fn at(root: PathBuf) -> FactStore {
        FactStore {
            root,
            db: OnceCell::new(),
            write: RefCell::new(None),
        }
    }

    /// Opens the store (retiring the legacy directory on the first successful
    /// open). An open failure is deliberately not propagated here: reads
    /// degrade to misses and the first write re-surfaces the error.
    pub fn load(self) -> FactStore {
        let _ = self.store();
        self
    }

    /// The opened database, opening it once on first use underneath the external
    /// store gate. A contended open caches the typed contention and replays it
    /// on every later call; any other failure caches its rendered message.
    fn store(&self) -> Result<&Store> {
        let db = self.root.join(FACTS_DB);
        let opened = self.db.get_or_init(|| {
            Store::open(&db)
                .inspect(|_| retire_legacy_store(&self.root))
                .map_err(OpenFailure::from_error)
        });
        match opened {
            Ok(store) => Ok(store),
            Err(failure) => Err(failure.replay()),
        }
    }

    /// A read transaction over the last committed state, or `None` when the
    /// store cannot be opened/read (a cache miss, never a write).
    fn read(&self) -> Option<ReadTxn> {
        self.store().ok()?.read().ok()
    }

    /// The one write transaction, opened on first use and held until
    /// [`FactStore::save_index`].
    fn write_txn(&self) -> Result<Ref<'_, WriteTxn>> {
        if self.write.borrow().is_none() {
            let txn = self.store()?.begin_write()?;
            *self.write.borrow_mut() = Some(txn);
        }
        Ok(Ref::map(self.write.borrow(), |slot| {
            slot.as_ref()
                .expect("the write transaction was just opened")
        }))
    }

    /// The five-part content-and-config identity of one cached unit: format,
    /// config digest, language, repo-relative path, and blob OID. The
    /// resolution `inputs` are a diagnostic column of the row, never part of
    /// this key.
    fn unit_key(lang: &str, rel: &str, oid: &str, cache_key: &CacheKey) -> UnitKey {
        UnitKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: cache_key.config.clone(),
            lang: lang.to_string(),
            rel: rel.to_string(),
            oid: oid.to_string(),
        }
    }

    /// One language's scaffolding identity: format, config digest, language.
    fn scaffolding_key(lang: &str, cache_key: &CacheKey) -> ScaffoldingKey {
        ScaffoldingKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: cache_key.config.clone(),
            lang: lang.to_string(),
        }
    }

    /// True when a unit for this byte identity exists **and** its recorded
    /// resolution-inputs digest equals `inputs`.
    pub fn has(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        inputs: &str,
        cache_key: &CacheKey,
    ) -> bool {
        let Some(read) = self.read() else {
            return false;
        };
        matches!(
            read.get_unit(&Self::unit_key(lang, rel, oid, cache_key)),
            Ok(Some(row)) if row.inputs == inputs
        )
    }

    /// Reads the reusable unit for `(lang, rel, oid)` under `cache_key` —
    /// `Some` only when the stored unit's resolution-inputs digest equals
    /// `inputs`. A byte-identical file whose inputs drifted is NOT reusable.
    pub fn reuse(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        inputs: &str,
        cache_key: &CacheKey,
    ) -> Option<(FileFragment, String)> {
        let read = self.read()?;
        let row = read
            .get_unit(&Self::unit_key(lang, rel, oid, cache_key))
            .ok()??;
        if row.inputs != inputs {
            return None;
        }
        let bytes = read.get_body(row.body).ok()??;
        let frag = body::decode::<FileFragment>(&bytes).ok()?;
        Some((frag, row.writer_root))
    }

    /// Reads the stored candidate unit for `(lang, rel, oid)` regardless of
    /// input drift, with the writing root — used to discover prior inputs.
    pub fn candidate(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        cache_key: &CacheKey,
    ) -> Option<(FileFragment, String)> {
        let read = self.read()?;
        let row = read
            .get_unit(&Self::unit_key(lang, rel, oid, cache_key))
            .ok()??;
        let bytes = read.get_body(row.body).ok()??;
        let frag = body::decode::<FileFragment>(&bytes).ok()?;
        Some((frag, row.writer_root))
    }

    /// True when the store holds a unit for `(lang, rel, oid)` under
    /// `cache_key`, whatever its inputs digest — [`Self::candidate`]'s
    /// selection without reading the body.
    pub fn has_unit(&self, lang: &str, rel: &str, oid: &str, cache_key: &CacheKey) -> bool {
        self.read()
            .map(|read| {
                read.has_unit(&Self::unit_key(lang, rel, oid, cache_key))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Writes a unit under its five-part key: the encoded body, addressed by
    /// content, plus the diagnostic inputs digest and the writing root. The
    /// write transaction is opened here on the first write and committed by
    /// [`Self::save_index`].
    pub fn put(
        &mut self,
        frag: &FileFragment,
        writer_root: &str,
        cache_key: &CacheKey,
    ) -> Result<()> {
        let encoded = body::encode(frag)?;
        let key = Self::unit_key(&frag.lang, &frag.rel_path, &frag.blob_oid, cache_key);
        let txn = self.write_txn()?;
        txn.put_body(encoded.id, &encoded.bytes)?;
        txn.put_unit(&key, encoded.id, &frag.inputs_digest(), writer_root)?;
        Ok(())
    }

    /// The number of stored units (tests), `0` when the store cannot be read.
    pub fn len(&self) -> usize {
        self.read().and_then(|read| read.len().ok()).unwrap_or(0)
    }

    /// True when the store holds no units (or cannot be read).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Commits the write transaction opened by the first write. With none held
    /// this writes nothing — but still confirms the store opens, so a failed
    /// open is never swallowed on the write path.
    pub fn save_index(&self) -> Result<()> {
        match self.write.borrow_mut().take() {
            Some(txn) => txn.commit(),
            None => self.store().map(|_| ()),
        }
    }

    /// Writes one language's module scaffolding under its format+config+lang
    /// key. It joins the same held write transaction as the per-file units, so
    /// a scaffolding write failure aborts the whole recording.
    pub fn put_scaffolding(
        &self,
        lang: &str,
        scaffolding: &ModuleScaffolding,
        cache_key: &CacheKey,
    ) -> Result<()> {
        let encoded = body::encode(scaffolding)?;
        let key = Self::scaffolding_key(lang, cache_key);
        let txn = self.write_txn()?;
        txn.put_body(encoded.id, &encoded.bytes)?;
        txn.put_scaffolding(&key, encoded.id)?;
        Ok(())
    }

    /// Writes every language's scaffolding from a
    /// [`ModuleScaffolding::extract`] map.
    pub fn put_scaffolding_all(
        &self,
        by_lang: &BTreeMap<String, ModuleScaffolding>,
        cache_key: &CacheKey,
    ) -> Result<()> {
        for (lang, scaffolding) in by_lang {
            self.put_scaffolding(lang, scaffolding, cache_key)?;
        }
        Ok(())
    }

    /// Reads a language's module scaffolding, or `None` when none was recorded
    /// under this cache key (a `1`-projection store, or a language never seen).
    pub fn scaffolding(&self, lang: &str, cache_key: &CacheKey) -> Option<ModuleScaffolding> {
        let read = self.read()?;
        let body_id = read
            .get_scaffolding(&Self::scaffolding_key(lang, cache_key))
            .ok()??;
        let bytes = read.get_body(body_id).ok()??;
        body::decode::<ModuleScaffolding>(&bytes).ok()
    }

    /// Replaces one worktree's whole `path -> blob OID` manifest. The rows are
    /// scoped by `worktree`, so another worktree's manifest is untouched; it
    /// joins the one held write transaction, committed by [`Self::save_index`].
    pub fn put_refs(&self, worktree: WorktreeId, entries: &BTreeMap<String, String>) -> Result<()> {
        self.write_txn()?.replace_refs(worktree, entries)
    }

    /// One worktree's `path -> blob OID` manifest; empty when the store cannot
    /// be read (a cache miss).
    pub fn refs(&self, worktree: WorktreeId) -> BTreeMap<String, String> {
        self.read()
            .and_then(|read| read.refs(worktree).ok())
            .unwrap_or_default()
    }

    /// Persists one worktree's opaque scan row. It joins the one held write
    /// transaction, committed by [`Self::save_index`]; the row is scoped by
    /// `worktree`, so another worktree's scan is untouched.
    pub fn put_scan(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.write_txn()?.put_scan(worktree, bytes)
    }

    /// One worktree's opaque scan row; `None` when none was recorded or the
    /// store cannot be read.
    pub fn scan(&self, worktree: WorktreeId) -> Option<Vec<u8>> {
        self.read()?.get_scan(worktree).ok()?
    }

    /// Every worktree's opaque scan row; empty when the store cannot be read.
    pub fn scans(&self) -> Vec<(WorktreeId, Vec<u8>)> {
        self.read()
            .and_then(|read| read.iter_scans().ok())
            .unwrap_or_default()
    }

    /// Persists one worktree's opaque file-dependency bytes. It joins the one
    /// held write transaction, committed by [`Self::save_index`]; the row is
    /// scoped by `worktree`.
    pub fn put_deps(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.write_txn()?.put_deps(worktree, bytes)
    }

    /// One worktree's opaque file-dependency bytes; `None` when none was
    /// recorded or the store cannot be read.
    pub fn deps(&self, worktree: WorktreeId) -> Option<Vec<u8>> {
        self.read()?.get_deps(worktree).ok()?
    }

    /// Persists one worktree's opaque signature bytes. It joins the one held
    /// write transaction, committed by [`Self::save_index`]; the row is scoped
    /// by `worktree`.
    pub fn put_signatures(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.write_txn()?.put_signatures(worktree, bytes)
    }

    /// One worktree's opaque signature bytes; `None` when none was recorded or
    /// the store cannot be read.
    pub fn signatures(&self, worktree: WorktreeId) -> Option<Vec<u8>> {
        self.read()?.get_signatures(worktree).ok()?
    }

    /// Persists one worktree's opaque overload-index bytes. It joins the one
    /// held write transaction, committed by [`Self::save_index`]; the row is
    /// scoped by `worktree`.
    pub fn put_overloads(&self, worktree: WorktreeId, bytes: &[u8]) -> Result<()> {
        self.write_txn()?.put_overloads(worktree, bytes)
    }

    /// One worktree's opaque overload-index bytes; `None` when none was
    /// recorded or the store cannot be read.
    pub fn overloads(&self, worktree: WorktreeId) -> Option<Vec<u8>> {
        self.read()?.get_overloads(worktree).ok()?
    }

    /// Registers one worktree's canonical checkout root. It joins the one held
    /// write transaction, committed by [`Self::save_index`]; only that
    /// worktree's registry row is written.
    pub fn register_worktree(&self, worktree: WorktreeId, root: &str) -> Result<()> {
        self.write_txn()?.register_worktree(worktree, root)
    }

    /// Every registered worktree and its checkout root; empty when the store
    /// cannot be read.
    pub fn worktrees(&self) -> Vec<(WorktreeId, String)> {
        self.read()
            .and_then(|read| read.iter_worktrees().ok())
            .unwrap_or_default()
    }

    /// Removes one worktree's registry row and every row scoped to it, leaving
    /// other worktrees' rows intact. It joins the one held write transaction,
    /// committed by [`Self::save_index`].
    pub fn remove_worktree(&self, worktree: WorktreeId) -> Result<()> {
        self.write_txn()?.remove_worktree(worktree)
    }
}

/// Retires the legacy phase-2 store directory (`<store>/facts/`) once, on the
/// first successful open: report the bytes it holds, then remove it
/// best-effort. A failure warns and continues — the redb store is authoritative
/// either way. The checkout-local toolchain cache (`apg/.trans/cache`) is a
/// different directory: the retired path is derived from the store directory
/// alone and can never name it.
fn retire_legacy_store(root: &Path) {
    let legacy = root.join(FACTS_DIR);
    if !legacy.is_dir() {
        return;
    }
    let bytes = dir_bytes(&legacy);
    match std::fs::remove_dir_all(&legacy) {
        Ok(()) => eprintln!(
            "[cache] retired the legacy fact store `{}` ({bytes} bytes reclaimed)",
            legacy.display()
        ),
        Err(err) => eprintln!(
            "warning: could not retire the legacy fact store `{}`: {err}",
            legacy.display()
        ),
    }
}

/// The bytes one directory tree holds (symlinks are not followed and count as
/// their own entry's metadata size).
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => total += dir_bytes(&entry.path()),
            Ok(_) => total += entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => {}
        }
    }
    total
}

/// The git **common** dir of a repository — for a linked worktree,
/// `<main>/.git`; for the main checkout, `<checkout>/.git`. Read from the
/// gitdir's `commondir` file when present (the linked-worktree case), else the
/// gitdir itself. Canonicalized when possible; falling back to the lexical
/// path. Every worktree of one checkout therefore resolves the same
/// `<git-common-dir>/apg/facts.db`.
pub fn common_git_dir(repo: &git2::Repository) -> PathBuf {
    let gitdir = repo.path();
    let rel = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match rel {
        Some(rel) => {
            let joined = gitdir.join(rel);
            std::fs::canonicalize(&joined).unwrap_or(joined)
        }
        None => std::fs::canonicalize(gitdir).unwrap_or_else(|_| gitdir.to_path_buf()),
    }
}

/// The absolute path a relative path names under `root` (`/`-joined).
pub fn abs_path_of(root: &str, rel: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

/// The checkout-relative path of an absolute path under `root`, or the path
/// unchanged when it is not under `root`.
pub fn rel_path_of(root: &Path, abs: &str) -> String {
    Path::new(abs)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs.to_string())
}

pub fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::File => "file",
        NodeKind::Struct => "struct",
        NodeKind::Function => "function",
        NodeKind::Agent => "agent",
        _ => "unresolved",
    }
}

/// The store root the scan records facts into for `apg_root`, or `None` when
/// `apg_root` is not inside a git repo (no shared store to resolve).
pub fn store_root_for(apg_root: &Path) -> Option<PathBuf> {
    FactStore::resolve(apg_root).ok().map(|s| s.root)
}

/// Convenience: the shared store root under a checkout's `apg/` layout
/// (`<git-common-dir>/apg`, holding `facts.db`).
pub fn store_root_under_apg(apg_root: &Path) -> Option<PathBuf> {
    let _ = specs::TRANS;
    store_root_for(apg_root)
}
