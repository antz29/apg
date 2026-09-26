//! The atomic-publish half of the splice (phase-03 task-3): checkpoint+close
//! the spliced copy, write the new `graph.jsonl`, and flip both artifacts with
//! same-filesystem renames, backups, and rollback.

use std::path::{Path, PathBuf};

use lbug::{Connection, Database};

use crate::graph::Graph;
use crate::load;

use super::seed::SeededDb;

// ---------------------------------------------------------------------------
// Atomic publish of the two artifacts (phase-03 task-3)
// ---------------------------------------------------------------------------
//
// The splice produces TWO coupled artifacts and both must flip together:
// `apg/.trans/db.lbug` (the query index) and `apg/.trans/graph.jsonl` (the
// sole code-identity/freshness/validation source — `git.rs` and
// `artifacts.rs`). A new DB beside a stale export (or vice versa) is a
// corrupt pair, so the publish is ordered to make that impossible on any
// *reported* failure:
//
// 1. **Nothing published yet** — the delta has already been applied to the
//    seeded temp copy (task-2); an earlier failure (seed / delta / export
//    build) leaves the previous `db.lbug` byte-identical.
// 2. **Checkpoint + close the spliced copy**, then fsync it. `CHECKPOINT`
//    flushes the WAL into the main file; dropping the handle closes it so no
//    `.wal`/`.shm` sidecar is left behind. A live handle would also keep the
//    temp inode open, making the rename publish the wrong file.
// 3. **Build the new export from the P2-assembled in-memory [`Graph`]** via
//    the existing tested [`load::write_graph_jsonl`] into a same-directory
//    temp, then fsync it. Never a DB→jsonl projection: `write_graph_jsonl`
//    single-sources the export format.
// 4. **Back up both targets** in the same directory, then swap: rename the
//    temp DB over `db.lbug`, then the temp export over `graph.jsonl`.
// 5. **fsync the containing directory** so both renames are durable, then
//    remove the backups.
//
// **Rollback**: if the `graph.jsonl` rename fails after the `db.lbug` rename
// succeeded, restore `db.lbug` from its backup so BOTH targets return to the
// previous state. If that restore itself fails, leave the backups in place
// and fail loudly with the manual recovery path — never a silently mixed
// pair.

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
pub fn publish(seeded: SeededDb, graph: &Graph, export: &Path) -> anyhow::Result<()> {
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

    // (3) Build the export from the in-memory graph into a same-directory temp
    // and fsync it. No target is touched on a build failure.
    let export_temp = transient_sibling(export, "graph", "tmp");
    let built = (|| -> anyhow::Result<()> {
        fire_publish_hook(PublishStage::BeforeExportWrite)?;
        load::write_graph_jsonl(graph, &export_temp)?;
        fsync_file(&export_temp)?;
        Ok(())
    })();
    if let Err(e) = built {
        remove_quietly(&export_temp);
        remove_quietly(&temp_path);
        return Err(e);
    }

    // (4) Save backups of both current targets, then swap DB first, export
    // second. `db_backup`/`export_backup` are same-directory snapshots.
    let db_backup = match save_backup(&target_path) {
        Ok(b) => b,
        Err(e) => {
            remove_quietly(&export_temp);
            remove_quietly(&temp_path);
            return Err(anyhow::anyhow!(
                "could not back up {} before the publish: {e}",
                target_path.display()
            ));
        }
    };
    let export_backup = match save_backup(export) {
        Ok(b) => b,
        Err(e) => {
            remove_quietly(&export_temp);
            remove_quietly(&temp_path);
            remove_quietly_opt(db_backup.as_deref());
            return Err(anyhow::anyhow!(
                "could not back up {} before the publish: {e}",
                export.display()
            ));
        }
    };

    if let Err(e) = std::fs::rename(&temp_path, &target_path) {
        // The DB rename is all-or-nothing, so nothing has been published;
        // drop every transient and leave both targets at their previous bytes.
        remove_quietly(&export_temp);
        remove_quietly(&temp_path);
        remove_quietly_opt(db_backup.as_deref());
        remove_quietly_opt(export_backup.as_deref());
        return Err(anyhow::anyhow!(
            "could not rename the spliced db over {}: {e}",
            target_path.display()
        ));
    }

    // The DB is now new. Swap the export; on failure roll the DB back so the
    // pair is never new-DB + stale-export.
    let swapped = (|| -> anyhow::Result<()> {
        fire_publish_hook(PublishStage::BeforeExportRename)?;
        std::fs::rename(&export_temp, export)?;
        Ok(())
    })();
    if let Err(e) = swapped {
        return rollback_export_swap(
            db_backup.as_deref(),
            &target_path,
            export_backup.as_deref(),
            &export_temp,
            e,
        );
    }

    // (5) Make both renames durable, then remove the backups.
    for dir in publish_dirs(&target_path, export) {
        fsync_dir(&dir)?;
    }
    remove_quietly_opt(db_backup.as_deref());
    remove_quietly_opt(export_backup.as_deref());
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

/// The export rename failed after the DB was already swapped: restore the DB
/// from its backup so BOTH targets return to the previous state. If the
/// restore itself fails, leave the backups in place and fail loudly with the
/// manual recovery path (never a new DB paired with a stale export).
fn rollback_export_swap(
    db_backup: Option<&Path>,
    target_path: &Path,
    export_backup: Option<&Path>,
    export_temp: &Path,
    cause: anyhow::Error,
) -> anyhow::Result<()> {
    remove_quietly(export_temp);
    let Some(backup) = db_backup else {
        // The seed requires a previous DB, so there is normally always a
        // backup; defensive only.
        remove_quietly_opt(export_backup);
        return Err(cause);
    };
    match std::fs::rename(backup, target_path) {
        Ok(()) => {
            // The export target was never swapped (its rename is
            // all-or-nothing), so the previous export is still in place and
            // `export_backup` is a redundant copy.
            remove_quietly_opt(export_backup);
            for dir in publish_dirs(target_path, target_path) {
                let _ = fsync_dir(&dir);
            }
            Err(cause.context(
                "the graph.jsonl swap failed after the db.lbug swap; db.lbug was rolled back to its previous bytes",
            ))
        }
        Err(restore_err) => anyhow::bail!(
            "publish failed ({cause:#}) and rolling {} back from its backup failed ({restore_err}); \
             the previous db.lbug is preserved at {} and the previous graph.jsonl at {} — recover with \
             `mv '{}' '{}'` and re-run the scan",
            target_path.display(),
            backup.display(),
            export_backup
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string()),
            backup.display(),
            target_path.display(),
        ),
    }
}

/// A unique same-directory sibling of `target` for a transient `tag`/`ext`
/// file (publish temps, backups). Kept in `target`'s parent so every rename
/// this module performs is an atomic same-filesystem rename.
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

/// The distinct parent directories of the two published targets (one, in the
/// normal `.trans/` layout).
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

// A one-shot injection fired at a chosen publish stage, so the
// rollback/early-failure paths can be exercised deterministically (the
// `fire_projection_hook` pattern in `artifacts.rs`). Compiled unconditionally
// (never installed outside tests, so the hook is a no-op) so the relocated
// `tests/splice_e2e.rs` integration crate can reach `install_publish_hook`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PublishStage {
    /// Before the new export temp is written — models an export-build failure.
    BeforeExportWrite,
    /// After the DB swap, before the export swap — models the partial-failure
    /// rollback trigger.
    BeforeExportRename,
}

type PublishHook = Box<dyn FnOnce() -> anyhow::Result<()>>;

thread_local! {
    static PUBLISH_HOOK: std::cell::RefCell<Option<(PublishStage, PublishHook)>> =
        const { std::cell::RefCell::new(None) };
}

pub fn install_publish_hook(
    stage: PublishStage,
    hook: impl FnOnce() -> anyhow::Result<()> + 'static,
) {
    PUBLISH_HOOK.with(|c| *c.borrow_mut() = Some((stage, Box::new(hook))));
}

fn fire_publish_hook(stage: PublishStage) -> anyhow::Result<()> {
    PUBLISH_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.as_ref().is_some_and(|(s, _)| *s == stage) {
            match slot.take() {
                Some((_, hook)) => hook(),
                None => Ok(()),
            }
        } else {
            Ok(())
        }
    })
}
