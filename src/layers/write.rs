//! The atomic multi-file write-through and the durable-mutation orchestrator
//! (SPEC §2.2/§4.1/§4.2): validate the complete change, write every affected
//! node file (and delete the removed ones) in one logical mutation, commit
//! once, and re-merge the exact delta into the live DB.
//!
//! Durable `apg node` / `apg edge` mutations require a live session and are
//! **not** committed per mutation: the session admits each into its write-back
//! buffer and only `apg session save` reaches [`write_through_with_deletes`] —
//! one atomic write of the whole buffered set plus exactly ONE git commit. The
//! per-mutation orchestrator ([`write_project`]) remains the direct/test path
//! for a single logical mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::artifacts;
use crate::git;
use crate::schema::Record;

use super::catalog::{LAYERS_DIR, LAYERS_TREE, Layer, StoragePolicy, TRANS_DIR};
use super::code_refs::validate_code_refs;
use super::node_file::{NodeFile, fqn};
use super::tree::ingest_nodes;
use super::validate::{
    PROP_ATTACHES_TO, check_edge_pairing, eval_constraint, parse_fqn, valid_name, validate_edges,
    validate_node,
};
use super::{layer_of, validate_assembled_rules};

// ---------------------------------------------------------------------------
// SPEC §4.1 — atomic multi-file write-through (phase-3 task-11)
// ---------------------------------------------------------------------------

/// Write a SET of node files atomically (SPEC §4.1 "renames / deletions are
/// atomic write-throughs"): one logical mutation updates ALL affected files —
/// the moved/removed file plus every referencing file whose edges/FQNs change
/// — and commits once. `writes` is the complete set of affected node files
/// (full new content for each); each path is derived from its own
/// layer/type/name, exactly as [`write_node`] derives it
/// (`<apg_root>/layers/<layer>/<type>/<name>.json`).
///
/// The complete proposed change is validated BEFORE anything is written, and
/// on any failure the previous state is restored — never leave mismatched
/// endpoint files. The pre-check here is **structural**, not semantic:
///
/// - a plans-layer node is refused (plans is transient — `.trans/plans/` only
///   — never a durable node-file layer);
/// - an allowlist-violating name is refused (reuse [`valid_name`] — never
///   sanitized);
/// - two `writes` entries colliding on the same path/FQN are refused.
///
/// The FULL semantic validation — [`validate_node`] uniqueness against the
/// whole store, [`check_edge_pairing`], the edge matrix ([`validate_edges`]) —
/// is the caller's job: `write_project` (task-16) runs validate_node/
/// validate_edges before this, and `ingest_tree` (task-15) runs pairing.
///
/// Atomicity: for every path, the pre-existing bytes are recorded
/// (`None` when the file did not exist) BEFORE any write; then all files are
/// written (parent dirs created). If ANY write fails, every path is restored —
/// write back the recorded bytes, or delete the file if it did not previously
/// exist — and the error is returned. After all writes succeed, the affected
/// paths are committed in a single commit; a commit failure rolls the writes
/// back too. Outside a git repo there is no commit — the mutation still lands
/// on disk (keeps non-git temp-dir tests working; real mutations run in a
/// project worktree).
// (Unused until write_project, phase-3 task-16, calls it.)
#[allow(dead_code)]
pub fn write_through(apg_root: &Path, writes: &[NodeFile]) -> anyhow::Result<()> {
    // --- Structural pre-check (before any filesystem touch) ---
    let mut paths: Vec<PathBuf> = Vec::with_capacity(writes.len());
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for node in writes {
        let layer = Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == node.layer)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
        if layer.storage() == StoragePolicy::TransientPlans {
            anyhow::bail!(
                "layer `plans` is transient (apg/.trans/plans/) — not a durable node-file layer"
            );
        }
        if !valid_name(&node.name) {
            anyhow::bail!(
                "node name `{}` is invalid — the name allowlist is [a-z0-9][a-z0-9-]* (refused, never sanitized)",
                node.name
            );
        }
        let path = apg_root
            .join(LAYERS_DIR)
            .join(layer.layer_dir())
            .join(&node.node_type)
            .join(format!("{}.json", node.name));
        if !seen.insert(path.clone()) {
            anyhow::bail!(
                "duplicate write: two entries target `{}` — one logical mutation touches each node file once",
                path.display()
            );
        }
        paths.push(path);
    }

    // Serialize every node file up front — a serialization failure is caught
    // before anything is written, so no rollback is needed for it.
    let serialized: Vec<String> = writes
        .iter()
        .map(serde_json::to_string_pretty)
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("serialize node file failed: {e}"))?;

    // Record the pre-existing bytes of every path BEFORE any write (None = the
    // file did not exist) — the snapshot a failure restores from.
    let mut prior: Vec<Option<Vec<u8>>> = Vec::with_capacity(paths.len());
    for path in &paths {
        prior.push(std::fs::read(path).ok());
    }

    // Write every file; on the first failure restore the whole set and return.
    for (i, path) in paths.iter().enumerate() {
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!(
                "create_dir_all {} failed: {e}",
                parent.display()
            ));
        }
        if let Err(e) = std::fs::write(path, &serialized[i]) {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!("write {} failed: {e}", path.display()));
        }
    }

    // Commit once — all affected paths together. Outside a git repo there is
    // no commit (the mutation still lands on disk); a commit failure rolls the
    // writes back so a failed mutation never leaves mismatched files.
    if git::in_repo(apg_root) {
        let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        let msg = git::graph_mutation_message(apg_root, &refs);
        match git::commit_files(apg_root, &refs, &[], &msg) {
            Ok(Some(_)) => {
                // Re-anchor the staleness gate's recorded scan_meta (mirrors
                // the JSONL funnel's commit path — DB and tree in sync by
                // construction). A re-anchor failure degrades to a warning.
                if let Err(e) = git::reanchor_scan_meta(apg_root, &git::git_state(apg_root)) {
                    eprintln!(
                        "apg: warning: could not re-anchor scan_meta after node-file commit: {e:#}"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                rollback(&paths, &prior);
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Restore the previous state of every path after a failed write: write back
/// the recorded bytes, or delete the file when it did not previously exist.
/// Best-effort — the caller returns the primary failure; a rollback step that
/// cannot be applied (e.g. a path blocked by a directory) is left as-is.
// (Unused until write_project, phase-3 task-16, wires write_through.)
#[allow(dead_code)]
fn rollback(paths: &[PathBuf], prior: &[Option<Vec<u8>>]) {
    for (path, prev) in paths.iter().zip(prior.iter()) {
        match prev {
            Some(bytes) => {
                let _ = std::fs::write(path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SPEC §4.1/§4.2 — the durable mutation orchestrator (phase-3 task-16)
// ---------------------------------------------------------------------------

/// Read every durable node file under `apg/layers/` into a [`NodeFile`] vector
/// (no validation) — the identity universe `validate_change` and `write_project`
/// resolve writes against. The FQN is derived from the path, never read.
///
/// Public so the relocated e2e crates reach it as
/// `apg::layers::read_existing_nodes` (e.g. the `testutil` crash-durability
/// test pairs the read store with [`check_edge_pairing`]).
pub fn read_existing_nodes(apg_root: &Path) -> anyhow::Result<Vec<NodeFile>> {
    let mut nodes: Vec<NodeFile> = Vec::new();
    for (layer_dir, types) in LAYERS_TREE {
        for node_type in *types {
            let dir = apg_root.join(LAYERS_DIR).join(layer_dir).join(node_type);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "json") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                let nf: NodeFile = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("{}: bad node file: {e}", path.display()))?;
                if nf.layer != *layer_dir || nf.node_type != *node_type || nf.name != name {
                    anyhow::bail!(
                        "node file {}: layer/type/name fields must match the path segments",
                        path.display()
                    );
                }
                nodes.push(nf);
            }
        }
    }
    nodes.sort_by(|a, b| (&a.layer, &a.node_type, &a.name).cmp(&(&b.layer, &b.node_type, &b.name)));
    Ok(nodes)
}

/// Derive the (layer, type, name) identity of a node file path under
/// `apg/layers/` (the inverse of [`write_node`]'s path derivation).
fn identity_from_path(apg_root: &Path, path: &Path) -> Option<(Layer, String, String)> {
    let rel = path.strip_prefix(apg_root.join(LAYERS_DIR)).ok()?;
    let mut comps = rel.components();
    let layer_dir = comps.next()?.as_os_str().to_str()?.to_string();
    let node_type = comps.next()?.as_os_str().to_str()?.to_string();
    let name = comps.next()?.as_os_str().to_str()?.to_string();
    let name = name.strip_suffix(".json")?.to_string();
    let layer = Layer::ALL.iter().find(|l| l.layer_dir() == layer_dir)?;
    Some((*layer, node_type, name))
}

/// Validate a complete proposed mutation BEFORE anything is written (SPEC
/// §4.1 "the complete proposed change is validated before anything is
/// written"): every written node passes [`validate_node`], every written node's
/// out-edge passes [`validate_edges`], the assembled post-mutation set
/// (existing nodes minus deleted/overwritten, plus the writes) satisfies the
/// property-aware §3.3 rules ([`validate_assembled_rules`]: acyclic
/// contains/depends-on trees; `Entity (kind: event)` publishes/subscribes
/// targets) and [`check_edge_pairing`], every written constraint passes
/// [`eval_constraint`]'s structure rules and, because a constraint's scope is
/// its layer (R2), declares no new/changed `attaches-to` reference, and — when
/// the `graph.jsonl` export exists —
/// every assembled `implemented-by` target is Real or Pending against the
/// exported scanned graph ([`validate_code_refs`]; a Drift target aborts before
/// the write). Validation never opens `db.lbug`. Pure read — no write.
pub fn validate_change(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> anyhow::Result<()> {
    // The direct path's effective base is the on-disk store; the validation
    // body is shared with the overlay path via [`validate_change_over`].
    let base = read_existing_nodes(apg_root)?;

    // The direct path's code-identity source is the `graph.jsonl` export
    // ([`code_universes_from_export`]); the session's admission supplies the
    // held `db.lbug`'s universes instead. When the export is absent, the
    // pre-decoupling behaviour recorded code-FQN refs UNVALIDATED (never
    // drift): seed `planned` with every `implemented-by` target in the base +
    // written set so each classifies Pending. The next scan re-validates.
    // (Mirrors `write_project_with`'s export-absent fallback.)
    let graph_jsonl = apg_root.join(TRANS_DIR).join("graph.jsonl");
    let (scanned, mut planned) = artifacts::code_universes_from_export(apg_root)?;
    if !graph_jsonl.exists() {
        for n in base.iter().chain(writes.iter()) {
            for oe in &n.out {
                if oe.kind == "implemented-by" {
                    planned.insert(oe.target.clone());
                }
            }
        }
    }

    validate_change_over(apg_root, &base, writes, deletes, &scanned, &planned)
}

/// Validate a complete proposed mutation against a caller-supplied **effective
/// base node set** instead of re-reading `apg/layers/**`. `base` is the
/// cumulative state the proposed `writes`/`deletes` apply over: the direct path
/// ([`validate_change`]) passes `read_existing_nodes(apg_root)`, while the
/// session's write-back buffer passes the overlay's `apply_to_base` (the
/// buffered writes staged over the on-disk store). `scanned`/`planned` are the
/// two code-FQN universes the `implemented-by` drift check resolves against,
/// supplied by the caller so this entry point performs no file I/O: the direct
/// path ([`validate_change`]) passes
/// [`code_universes_from_export`](crate::artifacts::code_universes_from_export)
/// (the `graph.jsonl` export), while the session's admission passes the held
/// database's
/// [`code_universes_from_db`](crate::artifacts::ArtifactDb::code_universes_from_db)
/// — neither opens a second handle nor reads `apg/.trans/graph.jsonl`. Every
/// check is otherwise identical to [`validate_change`] — per-node uniqueness,
/// the out-edge matrix
/// ([`validate_edges`]), the assembled §3.3 rules ([`validate_assembled_rules`]),
/// the `implemented-by` code-ref drift check ([`validate_code_refs`]), the
/// constraint structure + `attaches-to` rules ([`eval_constraint`]), and edge
/// pairing ([`check_edge_pairing`]) — so a buffered mutation validates against
/// the state an earlier unsaved mutation produced: an edge to a node added
/// earlier in the same run is accepted rather than rejected because it is absent
/// on disk. Pure read — no write.
pub fn validate_change_over(
    apg_root: &Path,
    base: &[NodeFile],
    writes: &[NodeFile],
    deletes: &[PathBuf],
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let mut existing: BTreeMap<String, NodeFile> = BTreeMap::new();
    for n in base {
        let f = fqn(layer_of(&n.layer), &n.node_type, &n.name);
        existing.insert(f, n.clone());
    }

    // Snapshot every existing constraint's `attaches-to` value BEFORE the
    // retain below drops written/deleted FQNs: after it, a written constraint
    // looks absent and has no prior node file to compare its `attaches-to`
    // against. The write-surface refusal (below) tells a NEW constraint — or a
    // rewrite that CHANGES the value — from a rewrite that leaves an existing
    // `attaches-to` value unchanged.
    let prior_attaches_to: BTreeMap<String, Option<String>> = existing
        .iter()
        .filter(|(_, n)| n.node_type == "constraint")
        .map(|(f, n)| (f.clone(), n.properties.get(PROP_ATTACHES_TO).cloned()))
        .collect();

    // Remove the deleted files and the overwritten files from the current set.
    let mut deleted_fqns: BTreeSet<String> = BTreeSet::new();
    for path in deletes {
        if let Some((layer, node_type, name)) = identity_from_path(apg_root, path) {
            deleted_fqns.insert(fqn(layer, &node_type, &name));
        }
    }
    let written_fqns: BTreeSet<String> = writes
        .iter()
        .map(|n| fqn(layer_of(&n.layer), &n.node_type, &n.name))
        .collect();
    existing.retain(|f, _| !deleted_fqns.contains(f) && !written_fqns.contains(f));

    // The uniqueness universe: everything currently present EXCEPT the nodes
    // this change (re)writes — so an overwrite does not trip its own uniqueness.
    let mut universe: BTreeSet<(Layer, String, String)> = existing
        .keys()
        .map(|f| {
            let (l, t, n) = parse_fqn(f).expect("existing FQN must parse");
            (l, t, n)
        })
        .collect();

    // Per-node + per-edge validation over the written set (a write that also
    // appears among the deletes is contradictory — refuse).
    for n in writes {
        let layer = layer_of(&n.layer);
        validate_node(layer, &n.node_type, &n.name, &n.properties, &universe)?;
        universe.insert((layer, n.node_type.clone(), n.name.clone()));
    }
    for n in writes {
        let from = fqn(layer_of(&n.layer), &n.node_type, &n.name);
        let edges: Vec<(&str, &str, &str)> = n
            .out
            .iter()
            .map(|oe| (oe.kind.as_str(), from.as_str(), oe.target.as_str()))
            .collect();
        validate_edges(&edges)?;
    }

    // The assembled post-mutation node set, FQN → node file (a write overrides
    // the current file) — the property-aware §3.3 rules and the code-ref drift
    // check need the full set, not just the writes.
    let mut assembled: BTreeMap<String, &NodeFile> =
        existing.iter().map(|(f, n)| (f.clone(), n)).collect();
    for n in writes {
        assembled.insert(fqn(layer_of(&n.layer), &n.node_type, &n.name), n);
    }

    // SPEC §3.3 property rules over the assembled set: contains/depends-on
    // trees acyclic, and publishes/subscribes targets are Entity (kind: event).
    // Both abort before anything is written.
    validate_assembled_rules(&assembled, &universe)?;

    // Code-ref drift (SPEC §4.1): an `implemented-by` target gone from the
    // scanned graph must abort BEFORE anything is written — otherwise the
    // file lands and commits and only the step-5 re-merge fails (a committed
    // partial mutation). The code-identity source is the caller-supplied
    // `scanned`/`planned` universes — the direct path resolves them from
    // `graph.jsonl`, the session from the `db.lbug` it already holds — so this
    // admission check performs NO file I/O and never opens `db.lbug`. A caller
    // with no code-identity source supplies universes that classify its refs
    // Pending, mirroring the export-absent fallback.
    let refs: Vec<&str> = assembled
        .values()
        .flat_map(|n| n.out.iter())
        .filter(|oe| oe.kind == "implemented-by")
        .map(|oe| oe.target.as_str())
        .collect();
    validate_code_refs(&refs, scanned, planned)?;

    // Constraint validation (R14 / R2): [`eval_constraint`] owns the structural
    // rules (name allowlist, type-in-layer, uniqueness), and the off-model
    // `attaches-to` property is refused HERE — a constraint's scope is its
    // layer, so a NEW constraint (or one whose `attaches-to` value CHANGES) may
    // not declare it. A rewrite that leaves an existing constraint's
    // `attaches-to` value unchanged passes, so the tree's attached constraints
    // stay authorable. Refused BEFORE anything is written.
    for n in writes {
        if n.node_type != "constraint" {
            continue;
        }
        let layer = layer_of(&n.layer);
        let mut own_universe = universe.clone();
        own_universe.remove(&(layer, n.node_type.clone(), n.name.clone()));
        eval_constraint(layer, &n.name, &n.properties, &own_universe)?;

        // The write-surface `attaches-to` refusal: refuse iff the written
        // constraint carries an `attaches-to` AND (its FQN is absent from the
        // pre-mutation snapshot — a NEW constraint — OR its value differs from
        // the snapshot's).
        if let Some(attaches_to) = n.properties.get(PROP_ATTACHES_TO) {
            let key = fqn(layer, &n.node_type, &n.name);
            match prior_attaches_to.get(&key) {
                Some(prior) if prior.as_deref() == Some(attaches_to.as_str()) => {}
                _ => anyhow::bail!(
                    "constraint `{key}` declares `{PROP_ATTACHES_TO}` ({attaches_to}) — a constraint's scope is its layer (R2), so a new or changed `attaches-to` is refused; author the constraint in the tier it binds"
                ),
            }
        }
    }

    // Pairwise symmetry over the assembled post-mutation set.
    let mut merged: Vec<NodeFile> = existing.into_values().collect();
    merged.extend(writes.iter().cloned());
    check_edge_pairing(&merged)?;
    Ok(())
}

/// Write a SET of node files atomically AND delete a set of node-file paths,
/// in one logical mutation (SPEC §4.1 "renames / deletions are atomic
/// write-throughs"): snapshot the prior state of every affected path, write
/// the writes, remove the deletes, and on any failure restore every path —
/// never leave mismatched endpoint files. Commits once. `write_through` (the
/// write-only primitive, task-11) delegates here with an empty delete list.
/// `apg session save` reaches this with the whole buffered set — one call, one
/// commit; the direct mutation path calls it once per logical mutation.
pub fn write_through_with_deletes(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> anyhow::Result<()> {
    // Structural pre-check: no plans layer, valid names, no duplicate write
    // path, and a path may not be both written and deleted.
    let mut write_paths: Vec<PathBuf> = Vec::with_capacity(writes.len());
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for node in writes {
        let layer = Layer::ALL
            .iter()
            .find(|l| l.layer_dir() == node.layer)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown layer `{}`", node.layer))?;
        if layer.storage() == StoragePolicy::TransientPlans {
            anyhow::bail!(
                "layer `plans` is transient (apg/.trans/plans/) — not a durable node-file layer"
            );
        }
        if !valid_name(&node.name) {
            anyhow::bail!(
                "node name `{}` is invalid — the name allowlist is [a-z0-9][a-z0-9-]* (refused, never sanitized)",
                node.name
            );
        }
        let path = apg_root
            .join(LAYERS_DIR)
            .join(layer.layer_dir())
            .join(&node.node_type)
            .join(format!("{}.json", node.name));
        if !seen.insert(path.clone()) {
            anyhow::bail!(
                "duplicate write: two entries target `{}` — one logical mutation touches each node file once",
                path.display()
            );
        }
        write_paths.push(path);
    }
    for d in deletes {
        if write_paths.contains(d) {
            anyhow::bail!(
                "a path cannot be both written and deleted in one mutation: {}",
                d.display()
            );
        }
    }

    // Serialize up front — a serialization failure is caught before any write.
    let serialized: Vec<String> = writes
        .iter()
        .map(serde_json::to_string_pretty)
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("serialize node file failed: {e}"))?;

    // Snapshot the prior state of every affected path (writes and deletes).
    let mut paths: Vec<PathBuf> = write_paths.clone();
    paths.extend(deletes.iter().cloned());
    let mut prior: Vec<Option<Vec<u8>>> = Vec::with_capacity(paths.len());
    for path in &paths {
        prior.push(std::fs::read(path).ok());
    }

    // Apply: write the writes, remove the deletes; restore everything on the
    // first failure.
    for (i, path) in write_paths.iter().enumerate() {
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!(
                "create_dir_all {} failed: {e}",
                parent.display()
            ));
        }
        if let Err(e) = std::fs::write(path, &serialized[i]) {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!("write {} failed: {e}", path.display()));
        }
    }
    for path in deletes {
        if let Err(e) = std::fs::remove_file(path) {
            rollback(&paths, &prior);
            return Err(anyhow::anyhow!("delete {} failed: {e}", path.display()));
        }
    }

    // Commit once. Outside a git repo there is no commit; a commit failure
    // rolls back so a failed mutation never leaves mismatched files.
    if git::in_repo(apg_root) {
        let write_refs: Vec<&Path> = write_paths.iter().map(|p| p.as_path()).collect();
        let delete_refs: Vec<&Path> = deletes.iter().map(|p| p.as_path()).collect();
        let all_refs: Vec<&Path> = write_refs
            .iter()
            .chain(delete_refs.iter())
            .copied()
            .collect();
        let msg = git::graph_mutation_message(apg_root, &all_refs);
        match git::commit_files(apg_root, &write_refs, &delete_refs, &msg) {
            // The durable write is now committed — the system-of-record
            // durability point. The staleness re-anchor and the projection are
            // the caller's (`write_project`), applied only AFTER this commit.
            Ok(Some(_)) | Ok(None) => {}
            Err(e) => {
                rollback(&paths, &prior);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// A test-only failure-injection point on the durable mutation path, at the
/// commit→project boundary (phase-05 task-14). The durable file write + its
/// single commit land FIRST; the projection apply follows. A test installs a
/// one-shot hook to force a failure at exactly one boundary — not a normal
/// projection error path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MutationBoundary {
    /// After validation, BEFORE the durable file write/commit: a failure here
    /// leaves the mutation in neither the durable store nor the projection.
    BeforeCommit,
    /// AFTER the durable file write + commit, BEFORE the projection apply: a
    /// failure here leaves the committed state durable while the projection
    /// stays prior — the next rebuild reproduces the committed state.
    BeforeProject,
}

type MutationHook = (MutationBoundary, Box<dyn FnOnce() -> anyhow::Result<()>>);

thread_local! {
    static MUTATION_HOOK: std::cell::RefCell<Option<MutationHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Install a one-shot hook that fires at `point` on the next `write_project`
/// in THIS thread (tests run one per thread, so a hook never leaks across
/// tests). A returned `Err` stands in for a crash at the boundary.
pub fn install_mutation_hook(
    point: MutationBoundary,
    hook: impl FnOnce() -> anyhow::Result<()> + 'static,
) {
    MUTATION_HOOK.with(|cell| *cell.borrow_mut() = Some((point, Box::new(hook))));
}

fn fire_mutation_hook(point: MutationBoundary) -> anyhow::Result<()> {
    let hook = MUTATION_HOOK.with(|cell| {
        let mut slot = cell.borrow_mut();
        match slot.as_ref() {
            Some((p, _)) if *p == point => slot.take().map(|(_, hook)| hook),
            _ => None,
        }
    });
    match hook {
        Some(hook) => hook(),
        None => Ok(()),
    }
}

/// The exact FQN delete set a durable node/edge mutation applies to the
/// projection (phase-05 tasks 4/6): every written FQN (removed ∪ changed —
/// detaching a written node drops its old incident edges, so the MERGE-only
/// re-merge cannot leave a vanished edge) plus every deleted FQN. Never a
/// whole layer-dir prefix.
fn projection_deletes(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for n in writes {
        out.insert(fqn(layer_of(&n.layer), &n.node_type, &n.name));
    }
    for path in deletes {
        if let Some((layer, node_type, name)) = identity_from_path(apg_root, path) {
            out.insert(fqn(layer, &node_type, &name));
        }
    }
    out
}

/// The injectable projection apply `write_project_with` threads through: given
/// the exact FQN delete set (removed ∪ changed) and the source records, apply
/// them to `db.lbug` in one transaction.
pub type ProjectionApply<'a> = &'a dyn Fn(&BTreeSet<String>, &[Record]) -> anyhow::Result<()>;

/// The durable-mutation orchestrator (SPEC §2.2/§4.1/§4.2): one logical
/// node/edge mutation — the full set of affected node files `writes` plus the
/// paths `deletes` to remove — guarded, validated, written atomically, and
/// re-merged into the live DB. Mirrors `artifacts::write_jsonl_and_reingest`'s
/// envelope for node files:
///
/// 1. **Membership guard** — node files have no project segment, so any
///    project context satisfies the guard (`git::require_project_context`).
/// 2. **Staleness gate** — refuse-on-stale when a DB exists, before any write.
/// 3. **Validate the complete change** ([`validate_change`]) before writing.
/// 4. **Atomic write + delete + single commit** ([`write_through_with_deletes`])
///    — the system-of-record durability point, and the ONLY step that controls
///    the flock-guaranteed single commit. On the direct (test/single-mutation)
///    path that commit is one logical mutation; under a live session the save
///    flush reaches step 4 with the whole buffered set and makes one commit for
///    all of it. No durable write is ever buffered: the files hit disk (and
///    git) before anything is projected.
/// 5. **Re-anchor the staleness gate's `scan_meta`** after the commit
///    (graph.jsonl only — never opens `db.lbug`).
/// 6. **Projection delta** — apply the exact durable delta
///    ([`projection_deletes`] = removed ∪ changed FQNs, guarded for planned
///    code FQNs) to the live DB, in one transaction, AFTER the durable commit
///    (commit-then-project). The same transaction re-MERGEs the worktree's
///    transient record set ([`append_transient_records`]: the plan store + the
///    five feedback tier mirrors) so a changed-FQN DETACH cannot drop a
///    pre-existing transient pairing (`Feedback -[:Reviews]-> <node>`).
///    Skipped when no DB exists (the files are the durable form); a re-ingest
///    failure leaves the committed durable state authoritative.
pub fn write_project(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
) -> anyhow::Result<()> {
    write_project_with(apg_root, writes, deletes, &|deletes, records| {
        artifacts::reingest_layers(apg_root, deletes, records)
    })
}

/// [`write_project`] with an injectable projection apply. The direct path uses
/// the default (open `db.lbug`, apply the delta, close); the phase-03 session
/// coordinator passes a closure that applies the delta through the DB handle it
/// already owns, so the session amortizes ONE open/parse across N mutations
/// while every mutation's projection delta still lands synchronously as the
/// mutation completes (the open is amortized, visibility never is).
pub fn write_project_with(
    apg_root: &Path,
    writes: &[NodeFile],
    deletes: &[PathBuf],
    project: ProjectionApply<'_>,
) -> anyhow::Result<()> {
    // 1. Membership guard (writes only happen inside a project context).
    git::require_project_context(apg_root)?;

    // 2. Staleness gate (refuse-on-stale when a DB exists).
    if apg_root.join(TRANS_DIR).join("db.lbug").exists()
        && let Some(msg) = git::refusal_message(apg_root)
    {
        anyhow::bail!("{msg}");
    }

    // 3. Validate the complete change before anything is written.
    validate_change(apg_root, writes, deletes)?;

    // Test-only injection at the BEFORE-COMMIT boundary (phase-05 task-14,
    // window 2): a failure here must leave the mutation in neither the durable
    // store nor the projection.
    fire_mutation_hook(MutationBoundary::BeforeCommit)?;

    // 4. Atomic write + delete + single commit — the durability point. This is
    //    the whole flock-held sequence's controlled commit (the direct path's
    //    one logical mutation; under a session the save flush reaches this via
    //    `write_through_with_deletes` with the whole buffered set).
    write_through_with_deletes(apg_root, writes, deletes)?;

    // 5. Re-anchor the staleness gate's recorded scan_meta AFTER the commit
    //    (mirrors the JSONL funnel's commit path: DB and tree in sync by
    //    construction, so consecutive node/edge mutations never trip the
    //    refuse-on-stale gate). graph.jsonl only — no db.lbug open. A re-anchor
    //    failure degrades to a warning — the mutation already landed. The
    //    captured state is reused below to reconcile the DB's own `Scan` row to
    //    the SAME state.
    let state = git::git_state(apg_root);
    if let Err(e) = git::reanchor_scan_meta(apg_root, &state) {
        eprintln!("apg: warning: could not re-anchor scan_meta after node-file commit: {e:#}");
    }

    // Test-only injection at the commit→project boundary (phase-05 task-14,
    // window 1): the durable write is committed by now; a failure here leaves
    // the projection prior and the committed state reproducible by a rebuild.
    fire_mutation_hook(MutationBoundary::BeforeProject)?;

    // 6. Projection delta — applied only AFTER the durable commit
    //    (commit-then-project). The effective node set is read back from the
    //    just-written tree and the FQN delete set is the same
    //    `projection_deletes`; the projection itself is the shared
    //    [`project_only`] entry point (also used by the session's
    //    projection-only path), so there is one projection implementation.
    //    The direct path owns the two caller-side responsibilities
    //    [`project_only`] used to carry: the no-DB skip (the projection applies
    //    only when a query index exists — the files are the durable form
    //    otherwise) and the code-identity source (the `graph.jsonl` export,
    //    with the export-absent fallback seeding every `implemented-by` target
    //    as planned so it records UNVALIDATED rather than aborting on a phantom
    //    drift; the next scan re-validates).
    let nodes = read_existing_nodes(apg_root)?;
    let deletes = projection_deletes(apg_root, writes, deletes);
    if apg_root.join(TRANS_DIR).join("db.lbug").exists() {
        let graph_jsonl = apg_root.join(TRANS_DIR).join("graph.jsonl");
        let (scanned, mut planned) = artifacts::code_universes_from_export(apg_root)?;
        if !graph_jsonl.exists() {
            // No code-identity source: the code-FQN refs are recorded
            // UNVALIDATED. Treat every implemented-by target as pending so the
            // projection re-merge records them instead of rejecting them as
            // drift. The direct path's effective set IS the on-disk store read
            // back above (the durable write already landed), so the write set
            // and the base set are one and the same.
            for n in &nodes {
                for oe in &n.out {
                    if oe.kind == "implemented-by" {
                        planned.insert(oe.target.clone());
                    }
                }
            }
        }
        project_only(apg_root, &nodes, &deletes, &scanned, &planned, project)?;
    }

    // 7. Reconcile the DB's own `Scan` row to the SAME re-anchored state that
    //    graph.jsonl's lead now names, so db_recorded_scan and recorded_scan can
    //    never disagree. The projection above opened and closed its own DB, so
    //    this opens a fresh handle (there is no held one on the direct path); a
    //    failure degrades to a warning like the re-anchor it mirrors — the
    //    durable write already landed and the next scan rebuilds.
    if apg_root.join(TRANS_DIR).join("db.lbug").exists() {
        match artifacts::ArtifactDb::open(apg_root) {
            Ok(db) => {
                if let Err(e) = db.refresh_scan_row(
                    state.sha.as_deref(),
                    state.sha.as_ref().map(|_| state.clean),
                    state.content_key.as_deref(),
                ) {
                    eprintln!(
                        "apg: warning: could not refresh the DB Scan row after node-file commit: {e:#}"
                    );
                }
            }
            Err(e) => {
                eprintln!("apg: warning: could not open the DB to refresh its Scan row: {e:#}");
            }
        }
    }
    Ok(())
}

/// The projection-only half of [`write_project_with`]'s step 6 — the entry
/// point the session's write-back buffer projects through at admission. Given
/// the caller-supplied effective in-memory node set `nodes`, the exact FQN
/// delete set `delete_fqns`, and the two caller-supplied code universes
/// `scanned`/`planned`, it builds the record stream from the in-memory nodes,
/// appends the worktree transient record set, and applies the delta through the
/// caller-supplied projection closure — with NO atomic node-file write, NO git
/// commit, and NO guard/staleness re-check (the session owns the DB and its
/// lifetime lock).
///
/// This entry point performs NO admission file I/O of its own: it never probes
/// `db.lbug`, never reads `apg/.trans/graph.jsonl`, and never re-reads
/// `apg/layers/**`. The `implemented-by` refs are the in-memory `nodes`'
/// out-edges; [`ingest_nodes`] classifies them against the supplied universes.
/// The caller owns the code-identity source: the direct path
/// ([`write_project_with`]) resolves `scanned`/`planned` from the `graph.jsonl`
/// export (seeding `planned` from the on-disk nodes' `implemented-by` refs when
/// the export is absent), while the session's admission passes the held
/// database's
/// [`code_universes_from_db`](crate::artifacts::ArtifactDb::code_universes_from_db).
/// The no-DB skip is likewise the caller's: `write_project_with` gates on
/// `db.lbug` (the session always holds a database).
///
/// `delete_fqns` carries the same FQN semantics as [`projection_deletes`]: every
/// touched/changed FQN (a DETACH drops its vanished incident edges) plus every
/// deleted FQN. It is supplied by the caller because the effective node set is
/// in memory: `projection_deletes` derives its set from paths + identity, which
/// does not apply to a buffered mutation.
///
/// The worktree transient record set is appended for the same reason
/// [`write_project_with`] appends it: a changed-FQN DETACH takes any incident
/// `Feedback -[:Reviews]-> <node>` edge with it, and a MERGE of the durable
/// records alone cannot put it back.
pub fn project_only(
    apg_root: &Path,
    nodes: &[NodeFile],
    delete_fqns: &BTreeSet<String>,
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
    project: ProjectionApply<'_>,
) -> anyhow::Result<()> {
    let mut records = ingest_nodes(nodes, scanned, planned)?;
    append_transient_records(apg_root, &mut records)?;
    project(delete_fqns, &records)?;
    Ok(())
}

/// The shared **plan-store-plus-tier-mirror** transient reader: appends the
/// worktree's transient record set to `records` — the plan store
/// (`.trans/plans/*.jsonl`) plus the five feedback tier mirrors
/// (`.trans/<tier>/*.jsonl`) — via the public enumerators
/// [`specs::plan_files`](crate::specs::plan_files) /
/// [`specs::trans_mirror_files`](crate::specs::trans_mirror_files).
///
/// It reads at most the file paths those enumerators return, and those
/// enumerate only **existing** files: an absent `.trans` is a benign no-op (an
/// empty append), while a present-but-malformed file is a **loud** error via
/// [`specs::read_jsonl`](crate::specs::read_jsonl) (never a silent skip). Every
/// file's records are collected before it returns, so a caller that re-ingests
/// only after this returns `Ok` gets all-or-nothing (nothing applied on any
/// malformed file).
///
/// Two callers: the **direct path** [`write_project_with`]'s assembly (it
/// re-merges the transient set so a changed-FQN DETACH cannot drop a
/// `Feedback -[:Reviews]-> <node>` pairing), and the **session-start seed**
/// (it projects the transient set into the held DB once at `session start`).
/// It is deliberately **NOT** part of the per-mutation admission path:
/// admission reads the held DB, never `.trans`.
pub(crate) fn append_transient_records(
    apg_root: &Path,
    records: &mut Vec<Record>,
) -> anyhow::Result<()> {
    for path in crate::specs::plan_files(apg_root)
        .into_iter()
        .chain(crate::specs::trans_mirror_files(apg_root))
    {
        records.extend(crate::specs::read_jsonl(&path)?);
    }
    Ok(())
}
