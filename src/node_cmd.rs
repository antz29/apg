//! `apg node` / `apg edge` — the durable mutation surface of the node-file
//! model (apg-projects SPEC §2.2/§4.1). Generic, type-as-argument: every write
//! routes through [`layers::write_project`] (guard → stale → validate → atomic
//! write → DB re-merge). These commands never touch the legacy `apg/specs/`,
//! `_invariants.jsonl`, or `apg/notes/` paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::artifacts::{ParsedArgs, acquire_spec_lock, parse_args};
use crate::layers::{self, InEdge, Layer, NodeFile, OutEdge, fqn};
use crate::schema::Record;
use crate::session;
use crate::specs;

fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

/// `--property k=v` (repeatable) → the node-file `properties` metadata map.
fn parse_properties(p: &ParsedArgs) -> BTreeMap<String, String> {
    let mut props = BTreeMap::new();
    for kv in p.all("property") {
        if let Some((k, v)) = kv.split_once('=') {
            props.insert(k.to_string(), v.to_string());
        }
    }
    props
}

/// `--unset-property k` (repeatable) → the property keys to delete.
fn parse_unset_properties(p: &ParsedArgs) -> BTreeSet<String> {
    p.all("unset-property").into_iter().collect()
}

/// The shared property-edit helper for the update surfaces: MERGE
/// `--property k=v` (overwrite only the passed keys) and `--unset-property k`
/// (delete exactly the named keys) over `base`, preserving every other key.
/// Omitting `--unset-property` never drops a key. Reused by `node update` and
/// `edge update`.
fn edit_properties(base: &BTreeMap<String, String>, p: &ParsedArgs) -> BTreeMap<String, String> {
    layers::merge_properties(base, &parse_properties(p), &parse_unset_properties(p))
}

/// Resolve a layer dir name to its [`Layer`] (closed catalog). Public so the
/// relocated `tests/node_cmd_e2e.rs` integration crate reaches it as
/// `apg::node_cmd::resolve_layer`.
pub fn resolve_layer(dir: &str) -> anyhow::Result<Layer> {
    Layer::ALL
        .iter()
        .find(|l| l.layer_dir() == dir)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("unknown layer `{dir}`"))
}

pub fn cmd_node(args: &[String]) -> anyhow::Result<()> {
    let Some(_sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg node <add|update|rm> …");
    };
    let apg_root = require_apg_root()?;
    // Transparent routing (phase-03): when a session is live it owns the DB AND
    // performs the whole durable write in receive order, so the mutation is
    // forwarded and the flock is never acquired here (the session holds it for
    // its life). A failed forward is an ERROR — there is no silent mid-flight
    // fallback, which could double-apply a mutation that already landed. With
    // no live session we take the serialized direct path below.
    if session::live_session(&apg_root) {
        let out = session::Coordinator::forward_mutation(&apg_root, "node", args)?;
        println!("{out}");
        return Ok(());
    }
    // The extended whole-durable-sequence flock: acquired exactly ONCE here,
    // before any node-file read, and held through add/update/rm's
    // validate → write → single commit → projection. This is the single
    // acquisition site for the node path (`node_add`/`node_update`/`node_rm`
    // never acquire internally, so there is no double-lock). The flock is
    // released when this command returns (the guard drops).
    let _lock = acquire_spec_lock(&apg_root)?;
    let change = build_change(&apg_root, "node", args)?;
    apply_change(&apg_root, change)
}

pub fn cmd_edge(args: &[String]) -> anyhow::Result<()> {
    let Some(_sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg edge <add|update|rm> …");
    };
    let apg_root = require_apg_root()?;
    // Transparent routing (phase-03) — identical to `cmd_node`: forward to the
    // live single writer, else the serialized direct path; never a silent
    // fallback.
    if session::live_session(&apg_root) {
        let out = session::Coordinator::forward_mutation(&apg_root, "edge", args)?;
        println!("{out}");
        return Ok(());
    }
    // The extended whole-durable-sequence flock: acquired exactly ONCE here,
    // before ANY source-file read, and held through add/update/rm's
    // validate → write → single commit → projection. This is the single
    // acquisition site for the edge path (`edge_add`/`edge_update`/`edge_rm`
    // never acquire internally, so there is no double-lock).
    let _lock = acquire_spec_lock(&apg_root)?;
    let change = build_change(&apg_root, "edge", args)?;
    apply_change(&apg_root, change)
}

/// A complete logical mutation: the node files to write, the paths to delete,
/// and the human message the command prints. Building the change only READS the
/// store (existence checks + RMW); applying it is the durable sequence. The
/// phase-03 session coordinator builds a change and applies it itself (single
/// writer), so this one builder serves both the direct and the routed path.
pub struct Change {
    pub writes: Vec<NodeFile>,
    pub deletes: Vec<PathBuf>,
    pub message: String,
}

/// Build the complete change for one `node`/`edge` mutation (the shared
/// read-modify-write the direct command and the session coordinator both run).
pub fn build_change(apg_root: &Path, kind: &str, args: &[String]) -> anyhow::Result<Change> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg {kind} <add|update|rm> …");
    };
    let rest = &args[1..];
    match (kind, sub) {
        ("node", "add") => node_add_change(apg_root, rest, &layers::LayersOverlay::new()),
        ("node", "update") => node_update_change(apg_root, rest, &layers::LayersOverlay::new()),
        ("node", "rm") => node_rm_change(apg_root, rest),
        ("edge", "add") => edge_add_change(apg_root, rest),
        ("edge", "update") => edge_update_change(apg_root, rest),
        ("edge", "rm") => edge_rm_change(apg_root, rest),
        (_, other) => anyhow::bail!("unknown apg {kind} subcommand: {other}"),
    }
}

/// Persist a built change through the direct path and print its message.
fn apply_change(apg_root: &Path, change: Change) -> anyhow::Result<()> {
    layers::write_project(apg_root, &change.writes, &change.deletes)?;
    println!("{}", change.message);
    Ok(())
}

/// `apg node add <layer> <type> <name> [--body B] [--property k=v]*` — refuses
/// when the FQN already exists (existence is never an implicit upsert; a
/// re-add full-replaces the file and drops its edges).
fn node_add_change(
    apg_root: &Path,
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node add <layer> <type> <name> [--body …] [--property k=v]*");
    }
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);
    // Existence check + write are inside the whole-durable-sequence flock held
    // by `cmd_node` (the single acquisition site) — no internal acquire, so the
    // read-modify-write is serialized and `node add` never double-locks.
    layers::refuse_if_present(
        overlay.exists(apg_root, layer, &pos[1], &pos[2]),
        &f,
        "apg node update",
        "apg node rm",
    )?;
    let node = NodeFile {
        layer: layer.layer_dir().to_string(),
        node_type: pos[1].clone(),
        name: pos[2].clone(),
        body: p.get("body").unwrap_or_default(),
        properties: parse_properties(&p),
        out: Vec::new(),
        in_edges: Vec::new(),
    };
    // Advisory-only wording warning (R1/R5): a proposed tier-1-3 body carrying
    // likely-flagged wording prints the shared advisory, but the write proceeds
    // unchanged — the author decides. The selection mirrors `apg spec lint`
    // ([`crate::spec_lint::tier_body_warning`]), so a constraint is exempt
    // (a negative rule lives legitimately in a layer-scoped constraint, R2).
    if let Some(msg) = crate::spec_lint::tier_body_warning(layer.layer_dir(), &pos[1], &node.body) {
        eprintln!("apg: warning: {f}: {msg}");
    }
    Ok(Change {
        writes: vec![node],
        deletes: Vec::new(),
        message: format!("Added node {f}"),
    })
}

/// `apg node update <layer> <type> <name> [--body B] [--property k=v]*
/// [--unset-property k]*` — body/properties only, edge-preserving: the
/// identity (`layer`/`type`/`name`) and every out/in edge are immutable;
/// properties MERGE with an explicit unset. Refuses an absent node.
fn node_update_change(
    apg_root: &Path,
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!(
            "usage: apg node update <layer> <type> <name> [--body …] [--property k=v]* [--unset-property k]*"
        );
    }
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);
    let body = p.get("body");
    // Resolve the node's existence and base content through the overlay
    // (phase-00 task-9): a node written earlier in the same unsaved run is the
    // base (its buffered content and edges are kept), a staged delete marker
    // means it is absent, and an unstaged identity falls back to the on-disk
    // file. This mirrors `layers::update_node_file`'s refusal and its
    // edge-preserving body/properties merge, over the resolved base rather than
    // a fresh disk read.
    let Some(mut updated) = overlay.read(apg_root, layer, &pos[1], &pos[2])? else {
        anyhow::bail!("node `{f}` does not exist — use `apg node add` to create it");
    };
    if let Some(body) = body.as_deref() {
        updated.body = body.to_string();
    }
    updated.properties = edit_properties(&updated.properties, &p);
    // Advisory-only wording warning (R1/R5): when the supplied `--body` carries
    // likely-flagged wording, print the shared advisory but let the update
    // proceed unchanged — the author decides. Only `--body` carries wording, so
    // an update with no body is silent; the selection mirrors `apg spec lint`
    // ([`crate::spec_lint::tier_body_warning`]).
    if let Some(b) = body.as_deref()
        && let Some(msg) = crate::spec_lint::tier_body_warning(layer.layer_dir(), &pos[1], b)
    {
        eprintln!("apg: warning: {f}: {msg}");
    }
    Ok(Change {
        writes: vec![updated],
        deletes: Vec::new(),
        message: format!("Updated node {f}"),
    })
}

/// `apg node rm <layer> <type> <name>` — remove the node file and rewrite every
/// file that references it (drop the incident edges), one atomic mutation.
fn node_rm_change(apg_root: &Path, args: &[String]) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node rm <layer> <type> <name>");
    }
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);

    // Warn (stderr) about each open/actioned Feedback that reviews the node,
    // then PROCEED: a removed node's Feedback records and their Reviews edges
    // survive in the project's transient record set, so the writer's claim and
    // the reviewer's resolve-or-reject still proceed. `node rm` takes no
    // project argument, so the project is the current worktree's branch —
    // resolved exactly as `review list` does. Best-effort: the write's own
    // project-context guard still governs.
    if let Ok(identity) = crate::git::repo_identity(apg_root)
        && let Some(project) = identity.branch.as_deref()
    {
        let mut records: Vec<Record> = Vec::new();
        for file in specs::project_transient_files(apg_root, project) {
            if file.exists() {
                records.extend(specs::read_jsonl(&file)?);
            }
        }
        let mut items: Vec<String> = Vec::new();
        for r in &records {
            let Record::Feedback {
                fqn: item, status, ..
            } = r
            else {
                continue;
            };
            if status != "open" && status != "actioned" {
                continue;
            }
            if records
                .iter()
                .any(|e| matches!(e, Record::Reviews { from, to } if from == item && to == &f))
            {
                items.push(format!("{item} ({status})"));
            }
        }
        items.sort();
        if !items.is_empty() {
            eprintln!(
                "apg: warning: removing `{f}` — it is reviewed by unresolved feedback: {}",
                items.join(", ")
            );
        }
    }

    let mut deletes = vec![layers::node_file_path(apg_root, layer, &pos[1], &pos[2])];
    let mut writes: Vec<NodeFile> = Vec::new();
    for node in layers::read_existing_nodes(apg_root)? {
        let node_fqn = fqn(resolve_layer(&node.layer)?, &node.node_type, &node.name);
        if node_fqn == f {
            continue; // the deleted node itself — dropped, not rewritten.
        }
        let mut changed = false;
        let mut nf = node.clone();
        nf.out.retain(|oe| {
            let keep = oe.target != f;
            changed |= !keep;
            keep
        });
        nf.in_edges.retain(|ie| {
            let keep = ie.source != f;
            changed |= !keep;
            keep
        });
        if changed {
            writes.push(nf);
        }
    }

    let deletes: Vec<PathBuf> = std::mem::take(&mut deletes);
    Ok(Change {
        writes,
        deletes,
        message: format!("Removed node {f}"),
    })
}

/// Read an edge endpoint: an authored node (`<layer>.<type>.<name>`) or, when it
/// does not parse, a code FQN (`implemented-by`/`details` targets).
fn read_endpoint(apg_root: &Path, f: &str) -> anyhow::Result<Option<NodeFile>> {
    match layers::parse_fqn(f) {
        Ok((layer, node_type, name)) => Ok(Some(layers::read_node_file(
            apg_root, layer, &node_type, &name,
        )?)),
        Err(_) => Ok(None), // code FQN — no node file.
    }
}

/// `apg edge add <kind> <from> <to> [--property k=v]*` — add the out-edge to the
/// source's file and the matching in-edge to the target's file (both halves).
/// Refuses a duplicate `(kind, from, to)` on the source's out-half (the edge is
/// identified by that triple; re-adding duplicates both halves).
fn edge_add_change(apg_root: &Path, args: &[String]) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg edge add <kind> <from> <to> [--property k=v]*");
    }
    let kind = pos[0].as_str();
    let from = pos[1].as_str();
    let to = pos[2].as_str();
    let props = parse_properties(&p);

    let (src_layer, src_type, src_name) = layers::parse_fqn(from)?;
    // This source-file read-push-write RMW (and the target in-half write below)
    // runs inside the whole-durable-sequence flock held by `cmd_edge` (the
    // single acquisition site) — no internal acquire, so no double-lock and the
    // shared endpoint file is never read outside the serialization scope.
    let mut source = layers::read_node_file(apg_root, src_layer, &src_type, &src_name)?;
    layers::refuse_if_present(
        source
            .out
            .iter()
            .any(|oe| oe.kind == kind && oe.target == to),
        &format!("edge {kind} {from} -> {to}"),
        "apg edge update",
        "apg edge rm",
    )?;
    source.out.push(OutEdge {
        kind: kind.to_string(),
        target: to.to_string(),
        properties: props.clone(),
    });

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(apg_root, to)? {
        target.in_edges.push(InEdge {
            kind: kind.to_string(),
            source: from.to_string(),
            properties: props,
        });
        writes.push(target);
    }

    Ok(Change {
        writes,
        deletes: Vec::new(),
        message: format!("Added edge {kind} {from} -> {to}"),
    })
}

/// `apg edge update <kind> <from> <to> --property k=v [--unset-property k]*` —
/// properties only: `kind`/`from`/`to` are immutable identity. Rewrites the
/// source out-half and the target in-half to the same MERGEd property map in
/// one atomic mutation. Refuses an absent edge.
fn edge_update_change(apg_root: &Path, args: &[String]) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!(
            "usage: apg edge update <kind> <from> <to> [--property k=v]* [--unset-property k]*"
        );
    }
    let kind = pos[0].as_str();
    let from = pos[1].as_str();
    let to = pos[2].as_str();

    let (src_layer, src_type, src_name) = layers::parse_fqn(from)?;
    let mut source = layers::read_node_file(apg_root, src_layer, &src_type, &src_name)?;
    let idx = source
        .out
        .iter()
        .position(|oe| oe.kind == kind && oe.target == to)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "edge {kind} {from} -> {to} does not exist — use `apg edge add` to create it"
            )
        })?;
    // MERGE the passed keys over the edge's current out-half properties; both
    // halves must end up with the identical map (pairing requires it).
    let merged = edit_properties(&source.out[idx].properties, &p);
    source.out[idx].properties = merged.clone();

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(apg_root, to)? {
        let Some(in_edge) = target
            .in_edges
            .iter_mut()
            .find(|ie| ie.kind == kind && ie.source == from)
        else {
            anyhow::bail!(
                "edge {kind} {from} -> {to}: the target `{to}` has no matching in-half — the store is not pairing-consistent"
            );
        };
        in_edge.properties = merged;
        writes.push(target);
    }

    Ok(Change {
        writes,
        deletes: Vec::new(),
        message: format!("Updated edge {kind} {from} -> {to}"),
    })
}

/// `apg edge rm <kind> <from> <to>` — drop the out-edge from the source and the
/// in-edge from the target, one atomic mutation.
fn edge_rm_change(apg_root: &Path, args: &[String]) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg edge rm <kind> <from> <to>");
    }
    let kind = pos[0].as_str();
    let from = pos[1].as_str();
    let to = pos[2].as_str();

    let (src_layer, src_type, src_name) = layers::parse_fqn(from)?;
    let mut source = layers::read_node_file(apg_root, src_layer, &src_type, &src_name)?;
    source
        .out
        .retain(|oe| !(oe.kind == kind && oe.target == to));

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(apg_root, to)? {
        target
            .in_edges
            .retain(|ie| !(ie.kind == kind && ie.source == from));
        writes.push(target);
    }

    Ok(Change {
        writes,
        deletes: Vec::new(),
        message: format!("Removed edge {kind} {from} -> {to}"),
    })
}

// ---------------------------------------------------------------------------
// Test-facing direct-path wrappers: the pre-refactor `node_add`/`edge_add`
// command shapes (build the change, persist it through `write_project`, print).
// The production dispatch uses `build_change` + `apply_change` directly so the
// session coordinator can inject its own (already-held) DB handle; these keep
// the existing direct-path tests unchanged and are compiled unconditionally
// (`pub`) so the relocated `tests/node_cmd_e2e.rs` integration crate reaches
// them.
// ---------------------------------------------------------------------------

pub fn node_add(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(
        apg_root,
        node_add_change(apg_root, args, &layers::LayersOverlay::new())?,
    )
}

pub fn node_update(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(
        apg_root,
        node_update_change(apg_root, args, &layers::LayersOverlay::new())?,
    )
}

pub fn edge_add(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(apg_root, edge_add_change(apg_root, args)?)
}

pub fn edge_update(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(apg_root, edge_update_change(apg_root, args)?)
}

#[cfg(test)]
mod tests;
