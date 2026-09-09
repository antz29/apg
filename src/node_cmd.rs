//! `apg node` / `apg edge` — the durable mutation surface of the node-file
//! model (apg-projects SPEC §2.2/§4.1). Generic, type-as-argument: every write
//! routes through [`layers::write_project`] (guard → stale → validate → atomic
//! write → DB re-merge). These commands never touch the legacy `apg/specs/`,
//! `_invariants.jsonl`, or `apg/notes/` paths.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::artifacts::{ParsedArgs, parse_args};
use crate::layers::{self, InEdge, Layer, NodeFile, OutEdge, fqn};
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

/// Resolve a layer dir name to its [`Layer`] (closed catalog).
fn resolve_layer(dir: &str) -> anyhow::Result<Layer> {
    Layer::ALL
        .iter()
        .find(|l| l.layer_dir() == dir)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("unknown layer `{dir}`"))
}

pub fn cmd_node(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg node <add|rm> …");
    };
    match sub {
        "add" => node_add(&args[1..]),
        "rm" => node_rm(&args[1..]),
        other => anyhow::bail!("unknown apg node subcommand: {other}"),
    }
}

pub fn cmd_edge(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg edge <add|rm> …");
    };
    match sub {
        "add" => edge_add(&args[1..]),
        "rm" => edge_rm(&args[1..]),
        other => anyhow::bail!("unknown apg edge subcommand: {other}"),
    }
}

/// `apg node add <layer> <type> <name> [--body B] [--property k=v]*`.
fn node_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node add <layer> <type> <name> [--body …] [--property k=v]*");
    }
    let apg_root = require_apg_root()?;
    let layer = resolve_layer(&pos[0])?;
    let node = NodeFile {
        layer: layer.layer_dir().to_string(),
        node_type: pos[1].clone(),
        name: pos[2].clone(),
        body: p.get("body").unwrap_or_default(),
        properties: parse_properties(&p),
        out: Vec::new(),
        in_edges: Vec::new(),
    };
    layers::write_project(&apg_root, &[node], &[])?;
    let f = fqn(layer, &pos[1], &pos[2]);
    println!("Added node {f}");
    Ok(())
}

/// `apg node rm <layer> <type> <name>` — remove the node file and rewrite every
/// file that references it (drop the incident edges), one atomic mutation.
fn node_rm(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node rm <layer> <type> <name>");
    }
    let apg_root = require_apg_root()?;
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);

    let mut deletes = vec![layers::node_file_path(&apg_root, layer, &pos[1], &pos[2])];
    let mut writes: Vec<NodeFile> = Vec::new();
    for node in layers::read_existing_nodes(&apg_root)? {
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
    layers::write_project(&apg_root, &writes, &deletes)?;
    println!("Removed node {f}");
    Ok(())
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
fn edge_add(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg edge add <kind> <from> <to> [--property k=v]*");
    }
    let apg_root = require_apg_root()?;
    let kind = pos[0].as_str();
    let from = pos[1].as_str();
    let to = pos[2].as_str();
    let props = parse_properties(&p);

    let (src_layer, src_type, src_name) = layers::parse_fqn(from)?;
    let mut source = layers::read_node_file(&apg_root, src_layer, &src_type, &src_name)?;
    source.out.push(OutEdge {
        kind: kind.to_string(),
        target: to.to_string(),
        properties: props.clone(),
    });

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(&apg_root, to)? {
        target.in_edges.push(InEdge {
            kind: kind.to_string(),
            source: from.to_string(),
            properties: props,
        });
        writes.push(target);
    }

    layers::write_project(&apg_root, &writes, &[])?;
    println!("Added edge {kind} {from} -> {to}");
    Ok(())
}

/// `apg edge rm <kind> <from> <to>` — drop the out-edge from the source and the
/// in-edge from the target, one atomic mutation.
fn edge_rm(args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg edge rm <kind> <from> <to>");
    }
    let apg_root = require_apg_root()?;
    let kind = pos[0].as_str();
    let from = pos[1].as_str();
    let to = pos[2].as_str();

    let (src_layer, src_type, src_name) = layers::parse_fqn(from)?;
    let mut source = layers::read_node_file(&apg_root, src_layer, &src_type, &src_name)?;
    source
        .out
        .retain(|oe| !(oe.kind == kind && oe.target == to));

    let mut writes = vec![source];
    if let Some(mut target) = read_endpoint(&apg_root, to)? {
        target
            .in_edges
            .retain(|ie| !(ie.kind == kind && ie.source == from));
        writes.push(target);
    }

    layers::write_project(&apg_root, &writes, &[])?;
    println!("Removed edge {kind} {from} -> {to}");
    Ok(())
}
