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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactDb;
    use crate::testutil::{self, Repo};

    const MOD: &str = "fixture.mod";
    const FILE: &str = "/abs/store.go";

    /// Commits paths on the worktree's branch (git2 — the same mechanics
    /// `git::commit_files` uses).
    fn wt_commit(wt: &Path, rels: &[&str], msg: &str) {
        let repo = git2::Repository::open(wt).unwrap();
        let mut index = repo.index().unwrap();
        for rel in rels {
            index.add_path(Path::new(rel)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
            .unwrap();
    }

    /// Writes `content` to `<wt>/<rel>`, creating parent dirs.
    fn wt_write(wt: &Path, rel: &str, content: &str) {
        let p = wt.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// A real project context (R3/R4): a git repo whose worktree `foo` on
    /// branch `foo` carries a real `apg/.trans/db.lbug` code graph built by
    /// the hermetic scan fixture — the context every `apg node`/`apg edge`
    /// mutation runs in. Returns `(wt_apg_root, repo, wt_root)`.
    fn mutation_fixture(tag: &str) -> (PathBuf, Repo, PathBuf) {
        let repo = Repo::new(&format!("nodecmd-{tag}"));
        let wt = repo.start_project("foo");
        let wt_apg = wt.join(specs::LAYOUT);
        wt_write(
            &wt,
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        wt_commit(&wt, &["code/seed.scan.jsonl"], "seed code");
        testutil::scan_checkout(&wt).unwrap();
        (wt_apg, repo, wt)
    }

    /// A bare node file with no edges — the exact shape `apg node add`
    /// builds.
    fn node(layer: &str, node_type: &str, name: &str) -> NodeFile {
        NodeFile {
            layer: layer.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            body: String::new(),
            properties: BTreeMap::new(),
            out: Vec::new(),
            in_edges: Vec::new(),
        }
    }

    /// `RETURN count(*)` over `db` as a number.
    fn count_rows(db: &ArtifactDb, q: &str) -> i64 {
        db.q(q)
            .unwrap()
            .lines()
            .last()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0)
    }

    /// R17 VI (write half): `apg node add` and `apg edge add` never create or
    /// modify `apg/specs/` or `apg/notes/`. Pre-existing committed legacy
    /// files stay byte-identical through node and edge mutations, and no new
    /// file appears beside them — the mutations land in `apg/layers/` + the
    /// branch DB only.
    #[test]
    fn node_and_edge_mutations_leave_pre_existing_legacy_files_untouched() {
        let repo = Repo::new("nodecmd-vi-unwritten");
        let wt = repo.start_project("foo");
        let wt_apg = wt.join(specs::LAYOUT);
        // Committed legacy durable files with sentinel content.
        std::fs::create_dir_all(wt_apg.join("specs")).unwrap();
        std::fs::create_dir_all(wt_apg.join("notes")).unwrap();
        std::fs::write(wt_apg.join("specs").join("foo.jsonl"), "SENTINEL SPEC\n").unwrap();
        std::fs::write(
            wt_apg.join("notes").join("fixture.mod.jsonl"),
            "SENTINEL NOTE\n",
        )
        .unwrap();
        wt_write(
            &wt,
            "code/seed.scan.jsonl",
            &testutil::code_payload(MOD, FILE, &["Store"]),
        );
        wt_commit(
            &wt,
            &[
                "code/seed.scan.jsonl",
                "apg/specs/foo.jsonl",
                "apg/notes/fixture.mod.jsonl",
            ],
            "seed code + legacy durable files",
        );
        testutil::scan_checkout(&wt).unwrap();

        // `apg node add` twice, then `apg edge add` once (the command shapes).
        layers::write_project(&wt_apg, &[node("requirements", "requirement", "r1")], &[]).unwrap();
        layers::write_project(&wt_apg, &[node("requirements", "requirement", "r2")], &[]).unwrap();
        let mut src =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        src.out.push(OutEdge {
            kind: "depends-on".to_string(),
            target: "requirements.requirement.r2".to_string(),
            properties: BTreeMap::new(),
        });
        let mut dst =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        dst.in_edges.push(InEdge {
            kind: "depends-on".to_string(),
            source: "requirements.requirement.r1".to_string(),
            properties: BTreeMap::new(),
        });
        layers::write_project(&wt_apg, &[src, dst], &[]).unwrap();

        // The legacy files are byte-identical, and nothing new appeared.
        assert_eq!(
            std::fs::read_to_string(wt_apg.join("specs").join("foo.jsonl")).unwrap(),
            "SENTINEL SPEC\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt_apg.join("notes").join("fixture.mod.jsonl")).unwrap(),
            "SENTINEL NOTE\n"
        );
        assert_eq!(
            std::fs::read_dir(wt_apg.join("specs")).unwrap().count(),
            1,
            "apg/specs must not gain files"
        );
        assert_eq!(
            std::fs::read_dir(wt_apg.join("notes")).unwrap().count(),
            1,
            "apg/notes must not gain files"
        );
        testutil::remove(&repo);
    }
}
