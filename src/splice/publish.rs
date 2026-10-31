//! The atomic-publish half of the splice (phase-03 task-3): checkpoint+close
//! the spliced copy and flip `db.lbug` into place with a same-filesystem
//! rename, its backup, and the directory fsync that makes it durable.

use std::path::{Path, PathBuf};

use lbug::{Connection, Database};

use crate::graph::Graph;

use super::seed::SeededDb;

// ---------------------------------------------------------------------------
// Atomic publish of the database (phase-03 task-3)
// ---------------------------------------------------------------------------
//
// The splice publishes ONE artifact: `apg/.trans/db.lbug` (the query index).
// The publish is ordered to make a partial failure impossible on any
// *reported* failure:
//
// 1. **Nothing published yet** — the delta has already been applied to the
//    seeded temp copy (task-2); an earlier failure (seed / delta) leaves the
//    previous `db.lbug` byte-identical.
// 2. **Checkpoint + close the spliced copy**, then fsync it. `CHECKPOINT`
//    flushes the WAL into the main file; dropping the handle closes it so no
//    `.wal`/`.shm` sidecar is left behind. A live handle would also keep the
//    temp inode open, making the rename publish the wrong file.
// 3. **Back up the target** in the same directory, then rename the temp over
//    `db.lbug`. The rename is all-or-nothing, so a failure leaves the
//    previous file in place.
// 4. **fsync the containing directory** so the rename is durable, then remove
//    the backup.
//
// The `graph.jsonl` export legs have retired, so both of `publish`'s trailing
// parameters — the P2-assembled `graph` and the `export` path — are unused
// (underscored); the next task in this phase removes them.

/// The `graph.jsonl` export path under an `apg/` layout root — the same
/// `.trans/` directory as [`db_path`], so the export rename is same-filesystem.
pub fn export_path(apg_root: &Path) -> PathBuf {
    apg_root.join(crate::specs::TRANS).join("graph.jsonl")
}

/// Publishes the splice's two artifacts atomically (phase-03 task-3).
///
/// Consumes the [`SeededDb`] (whose delta must already be applied) and the
/// P2-assembled in-memory `graph`. On success `target_path` holds the spliced
/// DB and `export` holds `graph`'s [`load::write_graph_jsonl`] rendering; on a
/// reported failure both targets are the previous bytes. `export` should be
/// [`export_path`] — a sibling of `target_path` in `.trans/`.
pub fn publish(seeded: SeededDb, _graph: &Graph, _export: &Path) -> anyhow::Result<()> {
    let SeededDb {
        db,
        temp_path,
        target_path,
    } = seeded;

    // (2) Checkpoint + close + fsync the spliced DB. Only now is the temp DB a
    // self-contained, durable file ready to be renamed into place.
    if let Err(e) = checkpoint_close_and_fsync(db, &temp_path) {
        remove_quietly(&temp_path);
        return Err(e);
    }

    // (3) Save a backup of the current target, then swap the temp over it.
    // Both are same-directory snapshots.
    let db_backup = match save_backup(&target_path) {
        Ok(b) => b,
        Err(e) => {
            remove_quietly(&temp_path);
            return Err(anyhow::anyhow!(
                "could not back up {} before the publish: {e}",
                target_path.display()
            ));
        }
    };

    if let Err(e) = std::fs::rename(&temp_path, &target_path) {
        // The rename is all-or-nothing, so nothing has been published;
        // drop the transient and leave the target at its previous bytes.
        remove_quietly(&temp_path);
        remove_quietly_opt(db_backup.as_deref());
        return Err(anyhow::anyhow!(
            "could not rename the spliced db over {}: {e}",
            target_path.display()
        ));
    }

    // (4) Make the rename durable, then remove the backup.
    for dir in publish_dirs(&target_path, &target_path) {
        fsync_dir(&dir)?;
    }
    remove_quietly_opt(db_backup.as_deref());
    Ok(())
}

/// Flushes the spliced DB's WAL into its main file and CLOSES it: run
/// `CHECKPOINT` on a fresh connection, drop the connection and the
/// [`Database`], prove no `.wal`/`.shm` sidecar survives, then fsync the
/// closed file.
fn checkpoint_close_and_fsync(db: Database, temp_path: &Path) -> anyhow::Result<()> {
    {
        let conn = Connection::new(&db)?;
        conn.query("CHECKPOINT")
            .map_err(|e| anyhow::anyhow!("CHECKPOINT of the spliced db failed: {e}"))?;
    }
    // A live handle pins the temp inode; the rename must publish the file this
    // handle wrote, so close before publishing.
    drop(db);

    for suffix in [".wal", ".shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", temp_path.display()));
        if sidecar.exists() {
            anyhow::bail!(
                "spliced db {} still has a {suffix} sidecar after CHECKPOINT+close — refusing to publish an unflushed database",
                temp_path.display()
            );
        }
    }
    fsync_file(temp_path)?;
    Ok(())
}

/// Saves `target` as a same-directory backup, returning its path (`None` when
/// `target` does not exist). A hard link is preferred: the closed previous
/// artifact is immutable, so the link is a true O(1) snapshot; a byte copy is
/// the fallback when the filesystem refuses a link.
fn save_backup(target: &Path) -> std::io::Result<Option<PathBuf>> {
    if !target.exists() {
        return Ok(None);
    }
    let backup = transient_sibling(target, "prev", "bak");
    let _ = std::fs::remove_file(&backup);
    match std::fs::hard_link(target, &backup) {
        Ok(()) => Ok(Some(backup)),
        Err(_) => {
            std::fs::copy(target, &backup)?;
            Ok(Some(backup))
        }
    }
}

/// A unique same-directory sibling of `target` for a transient `tag`/`ext`
/// file (the spliced database temp and its backup). Kept in `target`'s parent
/// so every rename this module performs is an atomic same-filesystem rename.
fn transient_sibling(target: &Path, tag: &str, ext: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_string());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(
        ".{file}.{tag}-{}-{nanos}.{ext}",
        std::process::id()
    ))
}

/// fsyncs `path` (opened for write for portable `sync_all`), forcing its bytes
/// to stable storage before a rename publishes it.
fn fsync_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()
}

/// fsyncs a directory entry, making a rename within it durable.
fn fsync_dir(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// The distinct parent directories the published database's rename touches
/// (one, in the normal `.trans/` layout).
fn publish_dirs(a: &Path, b: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for p in [a, b] {
        let d = p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

/// Best-effort removal of a transient file; a missing file is fine.
fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// [`remove_quietly`] over an optional path.
fn remove_quietly_opt(path: Option<&Path>) {
    if let Some(p) = path {
        remove_quietly(p);
    }
}
