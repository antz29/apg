//! Per-invocation session handle on a worktree's graph database.
//!
//! Every production command opens a [`Session`] on `apg/.trans/db.lbug` for the
//! length of its invocation: **read-only** by default, so a reader takes no
//! exclusive database class a concurrent writer needs. A durable write opens
//! the handle write-capable under the **blocking** worktree lock
//! ([`Session::open_for_write`]), runs the node-file write, exactly one git
//! commit and the database projection in that one critical section
//! ([`Session::write_through`]), then closes the handle and only then releases
//! the lock.
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
/// durable write constructs its session with [`open_for_write`](Self::open_for_write)
/// instead — the blocking worktree lock is taken first — and runs the node-file
/// write, commit and projection inside that critical section, closing the
/// handle before the lock is released; the read-only class takes no exclusive
/// read-write class a concurrent reader or writer needs.
pub struct Session {
    /// The layout root whose `apg/.trans/db.lbug` this handle holds and whose
    /// `apg/layers/**` write [`Session::write_through`] commits.
    apg_root: PathBuf,
    /// The held database handle: read-only from [`Session::open`], write-capable
    /// in a [`Session::open_for_write`] critical section.
    db: ArtifactDb,
}

/// A held write lock: the blocking worktree lock that serialises one durable
/// write's critical section. While it lives the [`Session`]'s database handle is
/// write-capable; dropping it releases the flock. The caller closes the
/// [`Session`] first, so a write-capable handle never outlives the lock.
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

    /// Writes a durable change's node files, commits it once, and projects it
    /// into the worktree database — the whole sequence inside the caller's held
    /// write critical section.
    ///
    /// `guard` is the [`WriteGuard`] from [`open_for_write`](Self::open_for_write):
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
    /// The handle stays write-capable on return (the guard is still alive); the
    /// caller closes the session before dropping the guard.
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
