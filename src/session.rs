//! Per-invocation session handle on a worktree's graph database.
//!
//! Every production command opens a [`Session`] on `apg/.trans/db.lbug` for the
//! length of its invocation: **read-only** by default, so a reader takes no
//! exclusive database class a concurrent writer needs. A durable write upgrades
//! the same handle under the **blocking** worktree lock
//! ([`Session::open_for_write`] / [`Session::upgrade_to_write`]), runs the
//! node-file write, exactly one git commit and the database projection in that
//! one critical section ([`Session::write_through`]), then releases the lock and
//! drops back to read-only ([`Session::release_write`]).
//!
//! There is no daemon, no socket and no session lifecycle: the invocation is the
//! session, and it is the only opener of the worktree database.

use std::path::{Path, PathBuf};

use crate::artifacts::{ArtifactDb, SpecLockGuard, acquire_spec_lock};
use crate::layers;

/// A per-invocation handle on a worktree's graph database, opened **read-only**
/// by default.
///
/// Every production command holds one for the length of its invocation. A
/// durable write upgrades the handle under the blocking worktree lock, runs the
/// node-file write, commit and projection inside that critical section, and
/// releases it back to read-only; until then the read-only class takes no
/// exclusive read-write class a concurrent reader or writer needs.
pub struct Session {
    /// The layout root whose `apg/.trans/db.lbug` this handle holds — the
    /// write upgrade reopens it read-write and the release drops back here.
    apg_root: PathBuf,
    /// The held database handle: read-only between writes, write-capable for
    /// one [`Session::open_for_write`] (or [`Session::upgrade_to_write`])
    /// critical section.
    db: ArtifactDb,
}

/// A held write upgrade: the blocking worktree lock that serialises one
/// durable write's critical section. While it lives the [`Session`]'s database
/// handle is write-capable; dropping it releases the flock, and
/// `Session::release_write` drops the handle back to read-only.
#[must_use = "dropping the guard immediately releases the whole-write lock"]
pub struct WriteGuard {
    _lock: SpecLockGuard,
}

impl Session {
    /// Opens the worktree database of the layout root `apg_root` in the
    /// read-only class. A missing database is refused naming `apg scan` as the
    /// remedy, exactly as [`ArtifactDb::open_read_only`] does.
    pub fn open(apg_root: &Path) -> anyhow::Result<Session> {
        Ok(Session {
            apg_root: apg_root.to_path_buf(),
            db: ArtifactDb::open_read_only(apg_root)?,
        })
    }

    /// Opens the worktree database of the layout root `apg_root`
    /// **write-capable**, taking the blocking worktree lock FIRST.
    ///
    /// This is the entry point every durable writer takes: the exclusive `flock`
    /// is acquired before `db.lbug` is opened, so no read-only handle of ours is
    /// ever open across another writer's checkpoint recovery — a concurrent
    /// writer queues on the lock and then opens a database at a settled state,
    /// instead of racing recovery and failing ("Cannot open database in
    /// read-only mode while checkpoint recovery is pending"). A missing
    /// database is refused naming `apg scan`, exactly as [`ArtifactDb::open`]
    /// does.
    ///
    /// The returned [`WriteGuard`] holds the lock for the caller's critical
    /// section ([`write_through`](Self::write_through)). A per-invocation
    /// writer closes its handle and only then releases the lock — dropping the
    /// [`Session`] before the guard — because lbug's own database lock is
    /// non-blocking: a writer queued on the flock must not wake at release to
    /// find this process's handle still open.
    pub fn open_for_write(apg_root: &Path) -> anyhow::Result<(Session, WriteGuard)> {
        let lock = acquire_spec_lock(apg_root)?;
        let db = ArtifactDb::open(apg_root)?;
        Ok((
            Session {
                apg_root: apg_root.to_path_buf(),
                db,
            },
            WriteGuard { _lock: lock },
        ))
    }

    /// The held database handle, read-only between writes — the seam a
    /// per-invocation read ([`crate::cmd_query`], the freshness and scan-row
    /// readers) runs its queries on, without a daemon round trip.
    pub fn db(&self) -> &lbug::Database {
        &self.db.db
    }

    /// Upgrades the read-only handle to the write-capable class under the
    /// **blocking** extended worktree lock.
    ///
    /// The lock is taken before the database is reopened and it waits: a
    /// concurrent writer holds the same exclusive `flock`, so this queues
    /// behind it instead of failing with `EAGAIN`. The returned [`WriteGuard`]
    /// keeps the lock held for the caller's critical section, during which
    /// `Session::write_through` writes, commits and projects; the release
    /// drops the handle back to read-only.
    pub fn upgrade_to_write(&mut self) -> anyhow::Result<WriteGuard> {
        let lock = acquire_spec_lock(&self.apg_root)?;
        // Read-only opens take no lock, so the new write handle and the old
        // read-only one can coexist; assigning the field drops the read-only
        // handle once the write-capable one is open.
        self.db = ArtifactDb::open(&self.apg_root)?;
        Ok(WriteGuard { _lock: lock })
    }

    /// Writes a durable change's node files, commits it once, and projects it
    /// into the worktree database — the whole sequence inside the caller's held
    /// write critical section.
    ///
    /// `guard` is the [`WriteGuard`] from [`open_for_write`](Self::open_for_write)
    /// (or a later [`upgrade_to_write`](Self::upgrade_to_write)):
    /// passing it makes the critical section explicit, so the write runs only
    /// while the blocking worktree lock is held and the handle is
    /// write-capable. The sequence is the shared durable-mutation orchestrator
    /// ([`layers::write::write_project_with`]) — membership guard, staleness
    /// gate, full-change validation, one atomic `apg/layers/**` write plus
    /// exactly one git commit, and then the projection delta — with the
    /// projection re-ingested through this session's **held** handle
    /// ([`ArtifactDb::reingest_layers_on`]) instead of a second
    /// independently-opened one.
    ///
    /// The handle stays write-capable on return (the guard is still alive);
    /// dropping back to the read-only class belongs to `Session::release_write`.
    pub fn write_through(
        &mut self,
        _guard: &WriteGuard,
        writes: &[layers::NodeFile],
        deletes: &[PathBuf],
    ) -> anyhow::Result<()> {
        layers::write::write_project_with(
            &self.apg_root,
            writes,
            deletes,
            &self.db,
            &|deletes, records| self.db.reingest_layers_on(deletes, records),
        )
    }

    /// Drops the write-capable handle and its blocking worktree lock back to
    /// the read-only class — the counterpart of
    /// [`open_for_write`](Self::open_for_write) /
    /// [`upgrade_to_write`](Self::upgrade_to_write), run once the caller's write
    /// critical section ([`write_through`](Self::write_through)) completes.
    ///
    /// The read-only handle is opened while the lock is still held, so the
    /// database is never left write-capable without the exclusive `flock`;
    /// consuming the [`WriteGuard`] then releases the lock so the next writer,
    /// scan or merge can take it.
    pub fn release_write(&mut self, guard: WriteGuard) -> anyhow::Result<()> {
        let read_only = ArtifactDb::open_read_only(&self.apg_root)?;
        drop(guard);
        self.db = read_only;
        Ok(())
    }
}

/// Discards a `.trans` directory's derived database: `db.lbug` **and its
/// `.wal`/`.shm` sidecars** (the suffixes [`crate::splice::publish`] asserts
/// on), and nothing else. Best-effort — an absent artifact is not an error.
///
/// Removing only `db.lbug` is not enough: a `db.lbug.wal` left behind by an
/// unclean exit makes the next `Database::new("db.lbug")` panic with a
/// database-id mismatch, so the sidecars must go too. This is the ONE discard
/// used by every forced-rebuild path: `apg scan --force`, the scan pipeline's
/// pending-state discard and its full-load branch, and the merge path's
/// unlanded-rebuild recovery. No caller keeps a second deletion list.
///
/// There is no `graph.jsonl` export to discard: the derived index is
/// `db.lbug` and its sidecars alone. The node files under `apg/layers/**` and
/// every source file are never touched here; the caller rebuilds the database
/// from them.
pub(crate) fn discard_derived_index(trans_dir: &Path) {
    for name in ["db.lbug", "db.lbug.wal", "db.lbug.shm"] {
        let _ = std::fs::remove_file(trans_dir.join(name));
    }
}
