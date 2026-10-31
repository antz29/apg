use std::path::{Path, PathBuf};

use super::identity::repo_rel;
use super::state::{GitState, canonical_allow_missing, discover_repo};

// ---------------------------------------------------------------------------
// Durable-commit helpers (R8): stage a caller-supplied change set and make
// exactly one commit — the shared one-commit primitive underlying the JSONL
// funnel's single-file `auto_commit` and the session save flush — then
// re-anchor the stale gate's recorded scan_meta so DB and tree stay in sync by
// construction. Durable `apg node`/`apg edge` mutations are NOT committed per
// mutation: they are buffered and commit ONCE at `apg session save`.
// ---------------------------------------------------------------------------

/// The shared one-commit primitive: commits all `writes` (created/modified
/// paths) and `deletes` (removed paths) in one commit on the current branch of
/// the checkout containing `apg_root` with a caller-supplied message (git2 only
/// — the git CLI is never shelled out to). The multi-file generalization of
/// [`commit_file`]: writes are staged with `index.add_path` and deletes with
/// `index.remove_path` (`add_path` stats the file and cannot stage a deletion —
/// a removed path fails with a libgit2 `NotFound`), the trees are compared, and
/// a single commit is created when anything changed. The session save flush
/// reaches it through `layers::write::write_through_with_deletes`; the JSONL
/// funnel reaches the single-file path through [`auto_commit`].
///
/// The index write (`repo.index()` / `index.write()`, i.e. `.git/index.lock`)
/// is **already inside the caller's extended whole-durable-sequence flock**:
/// `cmd_node`/`cmd_edge` hold [`crate::artifacts::acquire_spec_lock`] across
/// validate → write → commit → projection, so no internal acquire is needed and
/// a parallel burst never contends on `.git/index.lock`. Deletion-capable
/// staging (`add_path` for writes, `remove_path` for deletes) is preserved.
///
/// Returns `Ok(Some(sha))` with the new HEAD sha when a commit was created, or
/// `Ok(None)` when the staged tree already matches HEAD (nothing to commit —
/// e.g. an idempotent re-write). Errors when any path sits outside the
/// checkout or git refuses the commit.
pub fn commit_files(
    apg_root: &Path,
    writes: &[&Path],
    deletes: &[&Path],
    msg: &str,
) -> anyhow::Result<Option<String>> {
    let repo = discover_repo(apg_root)?;
    let base = repo_rel(apg_root);
    // The mutation paths may be lexical (e.g. through a `/var` →
    // `/private/var` symlink), while the base is canonical: canonicalize the
    // deepest existing ancestor before stripping.
    let rel = |p: &Path| -> anyhow::Result<PathBuf> {
        let path = canonical_allow_missing(p);
        path.strip_prefix(&base)
            .map(|r| r.to_path_buf())
            .map_err(|_| {
                anyhow::anyhow!(
                    "cannot commit {}: it is outside the checkout {}",
                    path.display(),
                    base.display()
                )
            })
    };
    let write_rels: Vec<PathBuf> = writes
        .iter()
        .map(|p| rel(p))
        .collect::<anyhow::Result<_>>()?;
    let delete_rels: Vec<PathBuf> = deletes
        .iter()
        .map(|p| rel(p))
        .collect::<anyhow::Result<_>>()?;
    let head = repo
        .head()
        .map_err(|e| anyhow::anyhow!("cannot commit: no HEAD to commit on ({e})"))?;
    let head_commit = head
        .peel_to_commit()
        .map_err(|e| anyhow::anyhow!("cannot commit: {e}"))?;

    // Stage every write and delete and compare trees: an unchanged tree means
    // nothing to commit (an idempotent mutation re-wrote identical content).
    let mut index = repo.index()?;
    for rel in &write_rels {
        index.add_path(rel)?;
    }
    for rel in &delete_rels {
        // Drop the removed path from the index so `write_tree` records the
        // deletion. A path that is not in the index has nothing to stage
        // (e.g. an untracked file already gone from disk) — tolerate it.
        let _ = index.remove_path(rel);
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    if tree_id == head_commit.tree_id() {
        return Ok(None);
    }
    let tree = repo.find_tree(tree_id)?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("apg", "apg@localhost"))?;
    let oid = repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head_commit])?;
    Ok(Some(oid.to_string()))
}

/// `commit_files` for a single file — the single-file commit path.
pub fn commit_file(apg_root: &Path, path: &Path, msg: &str) -> anyhow::Result<Option<String>> {
    commit_files(apg_root, &[path], &[], msg)
}

/// The standard graph-mutation commit message for a set of touched paths:
/// [`auto_commit`]'s single-path format, joined for a multi-file mutation —
/// `apg: graph mutation (rel1, rel2, ...)`. Paths render checkout-relative
/// when possible (the same form [`auto_commit`] uses).
pub fn graph_mutation_message(apg_root: &Path, paths: &[&Path]) -> String {
    let base = repo_rel(apg_root);
    let rels: Vec<String> = paths
        .iter()
        .map(|p| {
            canonical_allow_missing(p)
                .strip_prefix(&base)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string())
        })
        .collect();
    format!("apg: graph mutation ({})", rels.join(", "))
}

/// `commit_file` with the standard graph-mutation message — the JSONL
/// funnel's commit for a durable (non-`.trans`) write
/// ([`write_jsonl_and_reingest`](crate::artifacts::write_jsonl_and_reingest)).
/// This is not the durable node/edge mutation path: a durable `apg node`/
/// `apg edge` mutation is buffered and commits once at `apg session save`.
pub fn auto_commit(apg_root: &Path, path: &Path) -> anyhow::Result<Option<String>> {
    commit_file(apg_root, path, &graph_mutation_message(apg_root, &[path]))
}

/// True when `apg_root` is inside a git repository (discoverable via git2).
/// [`layers::write_through`] uses it to skip the commit outside a repo — no
/// repo → no commit; the mutation still lands on disk.
// (Unused until write_project, phase-3 task-16, wires write_through.)
#[allow(dead_code)]
pub fn in_repo(apg_root: &Path) -> bool {
    discover_repo(apg_root).is_ok()
}

/// Re-anchors the staleness gate's recorded scan state after a durable commit
/// (the JSONL funnel's per-write commit, the direct write path's
/// [`write_through`](crate::layers::write::write_through), or the session save
/// flush): refreshes the worktree database's own `Scan` row to the post-commit
/// git state, so every durability point lands exactly one row in the index the
/// tree's identities live in and [`is_stale`](crate::git::is_stale) agrees with
/// the tree by construction. The code graph itself is untouched — the commit
/// carries exactly the mutated node/mirror content.
///
/// No `graph.jsonl` control line is read or rewritten. A session socket means
/// the session owns the database — a live session's `save` refreshes the row
/// through the handle it already holds (`Coordinator::save`), and an unclean
/// exit's leftover socket marks the database not fresh until a scan or
/// `session start` reclaims it — so this never opens a second handle there.
/// (The socket presence, not a liveness ping, is the in-process-safe test: the
/// single-threaded accept loop cannot answer its own ping while it is saving.)
/// An absent database has nothing to re-anchor and is likewise a no-op.
pub fn reanchor_scan_meta(apg_root: &Path, state: &GitState) -> anyhow::Result<()> {
    if crate::session::socket_path(apg_root).exists() {
        return Ok(());
    }
    let db_path = apg_root.join(crate::specs::TRANS).join("db.lbug");
    if !db_path.exists() {
        return Ok(());
    }
    crate::artifacts::ArtifactDb::open(apg_root)?.refresh_scan_row(
        state.sha.as_deref(),
        state.sha.as_ref().map(|_| state.clean),
        state.content_key.as_deref(),
    )
}
