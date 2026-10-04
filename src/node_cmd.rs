//! `apg node` / `apg edge` — the durable mutation surface of the node-file
//! model (apg-projects SPEC §2.2/§4.1). Generic, type-as-argument: every write
//! routes through [`layers::write_project`] (guard → stale → validate → atomic
//! write → DB re-merge). These commands never touch the legacy `apg/specs/`,
//! `_invariants.jsonl`, or `apg/notes/` paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::artifacts::{ParsedArgs, parse_args};
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
///
/// An `--unset-property k` naming a key absent from `base` is refused (never a
/// silent no-op that reports success): the error names the keys present and,
/// when the requested key differs only by `_`/`-`, the stored spelling.
fn edit_properties(
    base: &BTreeMap<String, String>,
    p: &ParsedArgs,
) -> anyhow::Result<BTreeMap<String, String>> {
    let unset = parse_unset_properties(p);
    for k in &unset {
        if base.contains_key(k) {
            continue;
        }
        let present: Vec<&str> = base.keys().map(String::as_str).collect();
        let near = base
            .keys()
            .find(|have| have.replace('_', "-") == k.replace('_', "-"));
        let hint = match near {
            Some(have) => format!(" — did you mean `{have}`?"),
            None => String::new(),
        };
        anyhow::bail!(
            "--unset-property `{k}`: no such property (present: [{}]){hint}",
            present.join(", ")
        );
    }
    Ok(layers::merge_properties(base, &parse_properties(p), &unset))
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
    // Durable mutations are mandatory-session (spec
    // `requirements.requirement.cli-session-required-for-durable-mutations` /
    // `requirements.constraint.cli-session-mandatory`): when a session is live
    // it owns the DB AND the write-back buffer, and performs the whole durable
    // write in receive order, so the mutation is forwarded and the flock is
    // never acquired here (the session holds it for its life). A failed forward
    // is an ERROR — there is no silent mid-flight fallback, which could
    // double-apply a mutation that already landed. With no live session we
    // REFUSE: the direct path is gone, and the caller must open a session.
    if !session::live_session(&apg_root) {
        anyhow::bail!(
            "a live session is required for durable mutations — run `apg session start` first"
        );
    }
    let forwarded = session::Coordinator::forward_mutation(&apg_root, "node", args)?;
    let out = forwarded.output;
    println!("{out}");
    // Write-time warnings ride back in the session reply; print them on the
    // caller's stderr after the output (one per line, verbatim — the strings
    // already begin `apg: warning: …`). They are advisory only and never block
    // or alter the write's result.
    for warning in &forwarded.warnings {
        eprintln!("{warning}");
    }
    Ok(())
}

pub fn cmd_edge(args: &[String]) -> anyhow::Result<()> {
    let Some(_sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg edge <add|update|rm> …");
    };
    let apg_root = require_apg_root()?;
    // Durable mutations are mandatory-session (spec
    // `requirements.requirement.cli-session-required-for-durable-mutations` /
    // `requirements.constraint.cli-session-mandatory`): when a session is live
    // it owns the DB AND the write-back buffer, and performs the whole durable
    // write in receive order, so the mutation is forwarded and the flock is
    // never acquired here (the session holds it for its life). A failed forward
    // is an ERROR — there is no silent mid-flight fallback, which could
    // double-apply a mutation that already landed. With no live session we
    // REFUSE: the direct path is gone, and the caller must open a session.
    if !session::live_session(&apg_root) {
        anyhow::bail!(
            "a live session is required for durable mutations — run `apg session start` first"
        );
    }
    let forwarded = session::Coordinator::forward_mutation(&apg_root, "edge", args)?;
    let out = forwarded.output;
    println!("{out}");
    // Write-time warnings ride back in the session reply; print them on the
    // caller's stderr after the output (one per line, verbatim — the strings
    // already begin `apg: warning: …`). They are advisory only and never block
    // or alter the write's result.
    for warning in &forwarded.warnings {
        eprintln!("{warning}");
    }
    Ok(())
}

/// A complete logical mutation: the node files to write, the paths to delete,
/// the write-time advisories to surface to the caller, and the human message the
/// command prints. Building the change only READS the store (existence checks +
/// RMW); applying it is the durable sequence. The phase-03 session coordinator
/// builds a change and applies it itself (single writer), so this one builder
/// serves both the direct and the routed path.
pub struct Change {
    pub writes: Vec<NodeFile>,
    pub deletes: Vec<PathBuf>,
    /// Write-time advisories built beside the write (e.g. the node-add/update
    /// wording advisory and the node-rm outstanding-feedback notice). They ride
    /// in the change so a routed mutation can return them to the caller, who
    /// prints them on stderr at the mutation — they never block the write.
    pub warnings: Vec<String>,
    pub message: String,
}

/// Build the complete change for one `node`/`edge` mutation (the shared
/// read-modify-write the direct command and the session coordinator both run).
/// The direct path supplies `build_change_over` the on-disk node set as its base
/// ([`layers::read_existing_nodes`]) with an empty overlay, preserving the
/// direct path's existence checks and read-modify-write over `apg/layers/**`.
pub fn build_change(apg_root: &Path, kind: &str, args: &[String]) -> anyhow::Result<Change> {
    let base = layers::read_existing_nodes(apg_root)?;
    build_change_over(&base, apg_root, kind, args, &layers::LayersOverlay::new())
}

/// Buffer-aware twin of [`build_change`]: the same dispatch, but every arm
/// threads the caller-supplied `base` node set (the effective node set — the
/// session's `ArtifactDb::node_files_from_db`-reconstructed durable universe)
/// and a caller-supplied [`layers::LayersOverlay`] to its change builder, so a
/// mutation's existence checks and read-modify-write resolve against the base
/// with the cumulative buffered state folded in (an update/rm of an
/// earlier-buffered node applies over its buffered content) rather than against
/// unstaged identities read from `apg/layers/**` on disk. The session
/// coordinator calls this with the overlay it builds from its write-back buffer
/// and the DB-reconstructed base; the direct path ([`build_change`]) passes the
/// on-disk node set and an empty overlay.
pub fn build_change_over(
    base: &[NodeFile],
    apg_root: &Path,
    kind: &str,
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg {kind} <add|update|rm> …");
    };
    let rest = &args[1..];
    match (kind, sub) {
        ("node", "add") => node_add_change(base, rest, overlay),
        ("node", "update") => node_update_change(base, rest, overlay),
        ("node", "rm") => node_rm_change(base, apg_root, rest, overlay),
        ("edge", "add") => edge_add_change(base, rest, overlay),
        ("edge", "update") => edge_update_change(base, rest, overlay),
        ("edge", "rm") => edge_rm_change(base, rest, overlay),
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
/// re-add full-replaces the file and drops its edges). Existence resolves
/// through `overlay` against the caller-supplied `base` node set (the session's
/// `ArtifactDb::node_files_from_db`-reconstructed durable universe) via
/// [`layers::LayersOverlay::over_base`]: a staged write is present, a staged
/// delete marker is absent, and an unstaged identity is present exactly when the
/// base carries it. Pure map/list logic — it never reads `apg/layers/**` and
/// never probes `node_file_path`, so it cannot observe a node file the caller's
/// base does not carry.
fn node_add_change(
    base: &[NodeFile],
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
        overlay.over_base(base, layer, &pos[1], &pos[2]).is_some(),
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
    // likely-flagged wording rides in the returned [`Change`], but the write
    // proceeds unchanged — the author decides. The selection mirrors
    // `apg spec lint` ([`crate::spec_lint::tier_body_warning`]), so a
    // constraint is exempt (a negative rule lives legitimately in a
    // layer-scoped constraint, R2). Carrying it (rather than printing here)
    // lets a routed mutation return it in the session reply, and the caller
    // (cmd_node) prints it on its own stderr while the add completes.
    let mut warnings = Vec::new();
    if let Some(msg) = crate::spec_lint::tier_body_warning(layer.layer_dir(), &pos[1], &node.body) {
        warnings.push(format!("apg: warning: {f}: {msg}"));
    }
    Ok(Change {
        writes: vec![node],
        deletes: Vec::new(),
        warnings,
        message: format!("Added node {f}"),
    })
}

/// `apg node update <layer> <type> <name> [--body B] [--property k=v]*
/// [--unset-property k]*` — body/properties only, edge-preserving: the
/// identity (`layer`/`type`/`name`) and every out/in edge are immutable;
/// properties MERGE with an explicit unset. Refuses an absent node.
///
/// The node's existence and base content resolve through `overlay` against the
/// caller-supplied `base` node set (the session's
/// `ArtifactDb::node_files_from_db`-reconstructed durable universe) via
/// [`layers::LayersOverlay::over_base`]: a staged write yields its buffered
/// content (its edges are kept), a staged delete marker reads as absent, and an
/// unstaged identity yields the matching base node (or nothing when the base
/// holds none). Pure map/list logic — it never reads `apg/layers/**` and never
/// probes `node_file_path`, so it cannot observe a node file the caller's base
/// does not carry. The refusal and the edge-preserving body/properties merge are
/// applied over the resolved base rather than a fresh disk read.
fn node_update_change(
    base: &[NodeFile],
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
    // Resolve the node's existence and base content through the overlay against
    // the caller-supplied base (task-13): a node written earlier in the same
    // unsaved run is the base (its buffered content and edges are kept), a
    // staged delete marker means it is absent, and an unstaged identity yields
    // the matching base node (or nothing when the base holds none). The refusal
    // and the edge-preserving body/properties merge are applied over the resolved
    // base rather than a fresh disk read.
    let Some(mut updated) = overlay.over_base(base, layer, &pos[1], &pos[2]) else {
        anyhow::bail!("node `{f}` does not exist — use `apg node add` to create it");
    };
    if let Some(body) = body.as_deref() {
        updated.body = body.to_string();
    }
    updated.properties = edit_properties(&updated.properties, &p)?;
    // Advisory-only wording warning (R1/R5): when the supplied `--body` carries
    // likely-flagged wording, carry the shared advisory in the returned
    // [`Change`] but let the update proceed unchanged — the author decides. Only
    // `--body` carries wording, so an update with no body is silent; the
    // selection mirrors `apg spec lint` ([`crate::spec_lint::tier_body_warning`]).
    // Carrying it (rather than printing here) lets a routed mutation return it in
    // the session reply, and the caller (cmd_node) prints it on its own stderr
    // while the update completes.
    let mut warnings = Vec::new();
    if let Some(b) = body.as_deref()
        && let Some(msg) = crate::spec_lint::tier_body_warning(layer.layer_dir(), &pos[1], b)
    {
        warnings.push(format!("apg: warning: {f}: {msg}"));
    }
    Ok(Change {
        writes: vec![updated],
        deletes: Vec::new(),
        warnings,
        message: format!("Updated node {f}"),
    })
}

/// `apg node rm <layer> <type> <name>` — remove the node file and rewrite every
/// file that references it (drop the incident edges), one atomic mutation.
///
/// The node's existence resolves through `overlay` against the caller-supplied
/// `base` node set (the session's `ArtifactDb::node_files_from_db`-reconstructed
/// durable universe) via [`layers::LayersOverlay::over_base`]: a staged write is
/// present, a staged delete marker is absent, and an unstaged identity is present
/// exactly when the base carries it. The incident-edge rewrite iterates the
/// EFFECTIVE node set — the caller-supplied base with the overlay folded in
/// ([`layers::LayersOverlay::apply_to_base`]) — so a removal drops the edges a
/// node staged earlier in the same unsaved run. Both resolve purely over the
/// caller-supplied base and the overlay: the builder never reads
/// `apg/layers/**` and never probes `node_file_path`, so it cannot observe a
/// node file the caller's base does not carry. `apg_root` is still needed for
/// the transient-feedback warning and to compute the deleted file's path.
fn node_rm_change(
    base: &[NodeFile],
    apg_root: &Path,
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node rm <layer> <type> <name>");
    }
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);

    // Carry a warning about each open/actioned Feedback that reviews the node in
    // the returned [`Change`] (rather than printing it here), then PROCEED: a
    // removed node's Feedback records and their Reviews edges survive in the
    // project's transient record set, so the writer's claim and the reviewer's
    // resolve-or-reject still proceed. `node rm` takes no project argument, so
    // the project is the current worktree's branch — resolved exactly as
    // `review list` does. Best-effort: the write's own project-context guard
    // still governs. Carrying it (rather than printing here) lets a routed
    // mutation return it in the session reply, and the caller (cmd_node) prints
    // it on its own stderr while the removal completes.
    let mut warnings = Vec::new();
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
            warnings.push(format!(
                "apg: warning: removing `{f}` — it is reviewed by unresolved feedback: {}",
                items.join(", ")
            ));
        }
    }

    // Refuse an absent identity through the overlay against the caller-supplied
    // base (task-14), mirroring `node_update_change`: a node written earlier in
    // the same unsaved run is present (rm succeeds), a staged delete marker is
    // absent, and an unstaged identity is present exactly when the base carries
    // it.
    if overlay.over_base(base, layer, &pos[1], &pos[2]).is_none() {
        anyhow::bail!("node `{f}` does not exist — use `apg node add` to create it");
    }

    let mut deletes = vec![layers::node_file_path(apg_root, layer, &pos[1], &pos[2])];
    let mut writes: Vec<NodeFile> = Vec::new();
    // Rewrite incident edges over the EFFECTIVE node set: the caller-supplied
    // base with the overlay folded in, so a removal sees and drops the edges a
    // node staged earlier in the same unsaved run — not just the edges in the
    // base. Delete-marked identities are already absent from the effective set.
    for node in overlay.apply_to_base(base) {
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
        warnings,
        message: format!("Removed node {f}"),
    })
}

/// Read an edge endpoint: an authored node (`<layer>.<type>.<name>`) or, when it
/// does not parse, a code FQN (`implemented-by`/`details` targets). An authored
/// endpoint resolves through `overlay` against the caller-supplied `base` node
/// set (the session's `ArtifactDb::node_files_from_db`-reconstructed durable
/// universe) via [`layers::LayersOverlay::over_base`]: a staged write yields its
/// buffered content, a staged delete marker yields absent, and an unstaged
/// identity yields the matching base node (or nothing when the base holds none).
/// This is pure map/list logic — it never reads `apg/layers/**` and never probes
/// `node_file_path`, so it cannot observe a node file the caller's base does not
/// carry. A code FQN resolves to no node file. Infallible.
fn read_endpoint(
    base: &[NodeFile],
    overlay: &layers::LayersOverlay,
    f: &str,
) -> Option<NodeFile> {
    match layers::parse_fqn(f) {
        Ok((layer, node_type, name)) => overlay.over_base(base, layer, &node_type, &name),
        Err(_) => None, // code FQN — no node file.
    }
}

/// `apg edge add <kind> <from> <to> [--property k=v]*` — add the out-edge to the
/// source's file and the matching in-edge to the target's file (both halves).
/// Refuses a duplicate `(kind, from, to)` on the source's out-half (the edge is
/// identified by that triple; re-adding duplicates both halves).
///
/// Both endpoints resolve through `overlay` against the caller-supplied `base`
/// node set (the session's `ArtifactDb::node_files_from_db`-reconstructed
/// durable universe) via [`layers::LayersOverlay::over_base`] and
/// [`read_endpoint`]: the source out-half is read from the staged content first
/// (so an endpoint created or modified earlier in the same unsaved run
/// composes), a staged delete marker reads as absent, and an unstaged identity
/// yields the matching base node (or nothing when the base holds none). Pure
/// map/list logic — it never reads `apg/layers/**` and never probes
/// `node_file_path`, so it cannot observe a node file the caller's base does not
/// carry. The duplicate-triple refusal is evaluated on the resolved (buffered)
/// out-half.
fn edge_add_change(
    base: &[NodeFile],
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
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
    // shared endpoint file is never read outside the serialization scope. The
    // read resolves through `overlay` against the caller-supplied `base`, so a
    // source staged earlier in the same unsaved run is the base; a staged
    // delete, or an identity the base does not carry, refuses.
    let mut source = overlay
        .over_base(base, src_layer, &src_type, &src_name)
        .ok_or_else(|| {
            anyhow::anyhow!("node `{from}` does not exist — use `apg node add` to create it")
        })?;
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
    if let Some(mut target) = read_endpoint(base, overlay, to) {
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
        warnings: Vec::new(),
        message: format!("Added edge {kind} {from} -> {to}"),
    })
}

/// `apg edge update <kind> <from> <to> --property k=v [--unset-property k]*` —
/// properties only: `kind`/`from`/`to` are immutable identity. Rewrites the
/// source out-half and the target in-half to the same MERGEd property map in
/// one atomic mutation. Refuses an absent edge.
///
/// Both endpoints resolve through `overlay` against the caller-supplied `base`
/// node set (the session's `ArtifactDb::node_files_from_db`-reconstructed
/// durable universe) via [`layers::LayersOverlay::over_base`] and
/// [`read_endpoint`]: the source out-half is read from the staged content first
/// (so an edge added earlier in the same unsaved run is found and its
/// properties merge on the buffered state), a staged delete marker reads as
/// absent, and an unstaged identity yields the matching base node (or nothing
/// when the base holds none). Pure map/list logic — it never reads
/// `apg/layers/**` and never probes `node_file_path`, so it cannot observe a
/// node file the caller's base does not carry.
fn edge_update_change(
    base: &[NodeFile],
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
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
    let mut source = overlay
        .over_base(base, src_layer, &src_type, &src_name)
        .ok_or_else(|| {
            anyhow::anyhow!("node `{from}` does not exist — use `apg node add` to create it")
        })?;
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
    let merged = edit_properties(&source.out[idx].properties, &p)?;
    source.out[idx].properties = merged.clone();

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(base, overlay, to) {
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
        warnings: Vec::new(),
        message: format!("Updated edge {kind} {from} -> {to}"),
    })
}

/// `apg edge rm <kind> <from> <to>` — drop the out-edge from the source and the
/// in-edge from the target, one atomic mutation.
///
/// Both endpoints resolve through `overlay` against the caller-supplied `base`
/// node set (the session's `ArtifactDb::node_files_from_db`-reconstructed
/// durable universe) via [`layers::LayersOverlay::over_base`] and
/// [`read_endpoint`]: the source out-half is read from the staged content first
/// (so an edge added earlier in the same unsaved run is found and dropped from
/// the buffered state), a staged delete marker reads as absent, and an
/// unstaged identity yields the matching base node (or nothing when the base
/// holds none). Pure map/list logic — it never reads `apg/layers/**` and never
/// probes `node_file_path`, so it cannot observe a node file the caller's base
/// does not carry.
fn edge_rm_change(
    base: &[NodeFile],
    args: &[String],
    overlay: &layers::LayersOverlay,
) -> anyhow::Result<Change> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg edge rm <kind> <from> <to>");
    }
    let kind = pos[0].as_str();
    let from = pos[1].as_str();
    let to = pos[2].as_str();

    let (src_layer, src_type, src_name) = layers::parse_fqn(from)?;
    let mut source = overlay
        .over_base(base, src_layer, &src_type, &src_name)
        .ok_or_else(|| {
            anyhow::anyhow!("node `{from}` does not exist — use `apg node add` to create it")
        })?;
    source
        .out
        .retain(|oe| !(oe.kind == kind && oe.target == to));

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(base, overlay, to) {
        target
            .in_edges
            .retain(|ie| !(ie.kind == kind && ie.source == from));
        writes.push(target);
    }

    Ok(Change {
        writes,
        deletes: Vec::new(),
        warnings: Vec::new(),
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
        node_add_change(
            &layers::read_existing_nodes(apg_root)?,
            args,
            &layers::LayersOverlay::new(),
        )?,
    )
}

pub fn node_update(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(
        apg_root,
        node_update_change(apg_root, args, &layers::LayersOverlay::new())?,
    )
}

pub fn edge_add(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(
        apg_root,
        edge_add_change(apg_root, args, &layers::LayersOverlay::new())?,
    )
}

pub fn edge_update(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    apply_change(
        apg_root,
        edge_update_change(apg_root, args, &layers::LayersOverlay::new())?,
    )
}

#[cfg(test)]
mod tests;
