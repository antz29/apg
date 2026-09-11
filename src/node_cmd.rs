//! `apg node` / `apg edge` — the durable mutation surface of the node-file
//! model (apg-projects SPEC §2.2/§4.1). Generic, type-as-argument: every write
//! routes through [`layers::write_project`] (guard → stale → validate → atomic
//! write → DB re-merge). These commands never touch the legacy `apg/specs/`,
//! `_invariants.jsonl`, or `apg/notes/` paths.

use std::collections::{BTreeMap, BTreeSet};
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
        anyhow::bail!("usage: apg node <add|update|rm> …");
    };
    let apg_root = require_apg_root()?;
    match sub {
        "add" => node_add(&apg_root, &args[1..]),
        "update" => node_update(&apg_root, &args[1..]),
        "rm" => node_rm(&apg_root, &args[1..]),
        other => anyhow::bail!("unknown apg node subcommand: {other}"),
    }
}

pub fn cmd_edge(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("usage: apg edge <add|update|rm> …");
    };
    let apg_root = require_apg_root()?;
    match sub {
        "add" => edge_add(&apg_root, &args[1..]),
        "update" => edge_update(&apg_root, &args[1..]),
        "rm" => edge_rm(&apg_root, &args[1..]),
        other => anyhow::bail!("unknown apg edge subcommand: {other}"),
    }
}

/// `apg node add <layer> <type> <name> [--body B] [--property k=v]*` — refuses
/// when the FQN already exists (existence is never an implicit upsert; a
/// re-add full-replaces the file and drops its edges).
fn node_add(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node add <layer> <type> <name> [--body …] [--property k=v]*");
    }
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);
    layers::refuse_if_present(
        layers::node_file_path(apg_root, layer, &pos[1], &pos[2]).exists(),
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
    layers::write_project(apg_root, &[node], &[])?;
    println!("Added node {f}");
    Ok(())
}

/// `apg node update <layer> <type> <name> [--body B] [--property k=v]*
/// [--unset-property k]*` — body/properties only, edge-preserving: the
/// identity (`layer`/`type`/`name`) and every out/in edge are immutable;
/// properties MERGE with an explicit unset. Refuses an absent node.
fn node_update(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!(
            "usage: apg node update <layer> <type> <name> [--body …] [--property k=v]* [--unset-property k]*"
        );
    }
    let layer = resolve_layer(&pos[0])?;
    let body = p.get("body");
    let updated = layers::update_node_file(
        apg_root,
        layer,
        &pos[1],
        &pos[2],
        body.as_deref(),
        &parse_properties(&p),
        &parse_unset_properties(&p),
    )?;
    layers::write_project(apg_root, std::slice::from_ref(&updated), &[])?;
    println!("Updated node {}", fqn(layer, &pos[1], &pos[2]));
    Ok(())
}

/// `apg node rm <layer> <type> <name>` — remove the node file and rewrite every
/// file that references it (drop the incident edges), one atomic mutation.
fn node_rm(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
    let p = parse_args(args);
    let pos = &p.positional;
    if pos.len() < 3 {
        anyhow::bail!("usage: apg node rm <layer> <type> <name>");
    }
    let layer = resolve_layer(&pos[0])?;
    let f = fqn(layer, &pos[1], &pos[2]);

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
    layers::write_project(apg_root, &writes, &deletes)?;
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
/// Refuses a duplicate `(kind, from, to)` on the source's out-half (the edge is
/// identified by that triple; re-adding duplicates both halves).
fn edge_add(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
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

    layers::write_project(apg_root, &writes, &[])?;
    println!("Added edge {kind} {from} -> {to}");
    Ok(())
}

/// `apg edge update <kind> <from> <to> --property k=v [--unset-property k]*` —
/// properties only: `kind`/`from`/`to` are immutable identity. Rewrites the
/// source out-half and the target in-half to the same MERGEd property map in
/// one atomic mutation. Refuses an absent edge.
fn edge_update(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
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

    layers::write_project(apg_root, &writes, &[])?;
    println!("Updated edge {kind} {from} -> {to}");
    Ok(())
}

/// `apg edge rm <kind> <from> <to>` — drop the out-edge from the source and the
/// in-edge from the target, one atomic mutation.
fn edge_rm(apg_root: &Path, args: &[String]) -> anyhow::Result<()> {
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

    layers::write_project(apg_root, &writes, &[])?;
    println!("Removed edge {kind} {from} -> {to}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactDb;
    use crate::testutil::{self, Repo};
    use std::collections::BTreeSet;

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

    /// `apg node add` (the command shape through `layers::write_project`)
    /// lands ONE file per node at `<layer>/<type>/<name>.json` — the file
    /// name IS the identity, the FQN is derived `<layer>.<type>.<name>` —
    /// visible in the branch DB after the re-merge, auto-committed on the
    /// project branch, and never creating the legacy `apg/specs/`/`apg/notes/`
    /// paths.
    #[test]
    fn node_add_lands_one_file_per_node_with_identity_and_branch_db_visibility() {
        let (wt_apg, repo, wt) = mutation_fixture("node-add");
        // `apg node add requirements requirement timer`.
        let nf = node("requirements", "requirement", "timer");
        layers::write_project(&wt_apg, &[nf], &[]).unwrap();

        // One file per node at the derived path; the file name is the identity.
        let path = wt_apg
            .join(layers::LAYERS_DIR)
            .join("requirements")
            .join("requirement")
            .join("timer.json");
        assert!(path.exists(), "{} must exist", path.display());
        let back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "timer").unwrap();
        assert_eq!(back.name, "timer");
        assert_eq!(
            fqn(Layer::Requirements, "requirement", "timer"),
            "requirements.requirement.timer"
        );

        // Visible in the branch DB (the mutation re-merged the layers tree).
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.requirement.timer"));
        drop(db);

        // Never touches the legacy durable paths.
        assert!(
            !wt_apg.join("specs").exists(),
            "apg/specs must not be created by a node add"
        );
        assert!(
            !wt_apg.join("notes").exists(),
            "apg/notes must not be created by a node add"
        );

        // The node file auto-committed on the project branch (R8).
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            head.tree()
                .unwrap()
                .get_path(Path::new("apg/layers/requirements/requirement/timer.json"))
                .is_ok(),
            "the node file must be committed on the project branch"
        );
        testutil::remove(&repo);
    }

    /// `apg edge add` (the command shape) writes BOTH endpoint files — the
    /// out half in the source's file, the matching in half in the target's —
    /// keeping the store pairing-consistent and landing the edge in the
    /// branch DB.
    #[test]
    fn edge_add_writes_both_endpoint_files() {
        let (wt_apg, repo, _wt) = mutation_fixture("edge-add");
        layers::write_project(&wt_apg, &[node("requirements", "requirement", "r1")], &[]).unwrap();
        layers::write_project(&wt_apg, &[node("requirements", "requirement", "r2")], &[]).unwrap();
        // `apg edge add depends-on requirements.requirement.r1
        // requirements.requirement.r2`.
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

        // Both halves landed: out in the source's file, in in the target's.
        let src_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let dst_back =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        assert_eq!(src_back.out.len(), 1);
        assert_eq!(src_back.out[0].kind, "depends-on");
        assert_eq!(src_back.out[0].target, "requirements.requirement.r2");
        assert_eq!(dst_back.in_edges.len(), 1);
        assert_eq!(dst_back.in_edges[0].kind, "depends-on");
        assert_eq!(dst_back.in_edges[0].source, "requirements.requirement.r1");

        // The store stays pairing-consistent, and the edge is in the branch DB.
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let out = db
            .q("MATCH (a:Requirement {fqn: 'requirements.requirement.r1'})-[:DependsOn]->(b:Requirement {fqn: 'requirements.requirement.r2'}) RETURN count(*)")
            .unwrap();
        assert_eq!(
            out.lines().last().map(str::trim),
            Some("1"),
            "the DependsOn edge must be in the branch DB: {out}"
        );
        testutil::remove(&repo);
    }

    /// Authored `uses` (Person→System) and `calls` (Service→Service) edges
    /// survive the write-through re-merge: `artifacts::edge_merge` maps both
    /// record kinds and the merge guard admits their rel pairs, so the branch
    /// DB shows them exactly like a full scan would. Regression for the
    /// `_ => None` arm that used to drop them.
    #[test]
    fn edge_add_lands_authored_uses_and_calls_in_the_branch_db() {
        let (wt_apg, repo, _wt) = mutation_fixture("authored-edges");
        // `apg node add` × 4: a Person + System (the `uses` pair) and two
        // Services (the `calls` pair).
        layers::write_project(
            &wt_apg,
            &[
                node("solution", "person", "alice"),
                node("solution", "system", "portal"),
                node("domain", "service", "svc-a"),
                node("domain", "service", "svc-b"),
            ],
            &[],
        )
        .unwrap();

        // `apg edge add uses solution.person.alice solution.system.portal` and
        // `apg edge add calls domain.service.svc-a domain.service.svc-b` —
        // both halves of each edge in one mutation.
        let mut person =
            layers::read_node_file(&wt_apg, Layer::Solution, "person", "alice").unwrap();
        person.out.push(OutEdge {
            kind: "uses".to_string(),
            target: "solution.system.portal".to_string(),
            properties: BTreeMap::new(),
        });
        let mut system =
            layers::read_node_file(&wt_apg, Layer::Solution, "system", "portal").unwrap();
        system.in_edges.push(InEdge {
            kind: "uses".to_string(),
            source: "solution.person.alice".to_string(),
            properties: BTreeMap::new(),
        });
        let mut svc_a = layers::read_node_file(&wt_apg, Layer::Domain, "service", "svc-a").unwrap();
        svc_a.out.push(OutEdge {
            kind: "calls".to_string(),
            target: "domain.service.svc-b".to_string(),
            properties: BTreeMap::new(),
        });
        let mut svc_b = layers::read_node_file(&wt_apg, Layer::Domain, "service", "svc-b").unwrap();
        svc_b.in_edges.push(InEdge {
            kind: "calls".to_string(),
            source: "domain.service.svc-a".to_string(),
            properties: BTreeMap::new(),
        });
        layers::write_project(&wt_apg, &[person, system, svc_a, svc_b], &[]).unwrap();

        // Both authored edges are in the branch DB after the write-through
        // re-merge.
        let db = ArtifactDb::open(&wt_apg).unwrap();
        let uses = db
            .q("MATCH (p:Person {fqn: 'solution.person.alice'})-[:Uses]->(s:System {fqn: 'solution.system.portal'}) RETURN count(*)")
            .unwrap();
        assert_eq!(
            uses.lines().last().map(str::trim),
            Some("1"),
            "the authored Uses edge must survive the write-through re-merge: {uses}"
        );
        let calls = db
            .q("MATCH (a:Service {fqn: 'domain.service.svc-a'})-[:Calls]->(b:Service {fqn: 'domain.service.svc-b'}) RETURN count(*)")
            .unwrap();
        assert_eq!(
            calls.lines().last().map(str::trim),
            Some("1"),
            "the authored Calls edge must survive the write-through re-merge: {calls}"
        );
        drop(db);
        testutil::remove(&repo);
    }

    /// Reads see the node files: `read_existing_nodes` returns every written
    /// node; `read_node_file`/`node_file_path` resolve the identity.
    #[test]
    fn reads_see_the_node_files() {
        let (wt_apg, repo, _wt) = mutation_fixture("reads");
        let mut ent = node("domain", "entity", "customer");
        ent.properties
            .insert("kind".to_string(), "entity".to_string());
        layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "r1"), ent],
            &[],
        )
        .unwrap();

        let all = layers::read_existing_nodes(&wt_apg).unwrap();
        assert_eq!(all.len(), 2);
        let fqns: BTreeSet<String> = all
            .iter()
            .map(|n| fqn(resolve_layer(&n.layer).unwrap(), &n.node_type, &n.name))
            .collect();
        assert!(fqns.contains("requirements.requirement.r1"));
        assert!(fqns.contains("domain.entity.customer"));
        let path = layers::node_file_path(&wt_apg, Layer::Domain, "entity", "customer");
        assert!(path.exists(), "{} must exist", path.display());
        let back = layers::read_node_file(&wt_apg, Layer::Domain, "entity", "customer").unwrap();
        assert_eq!(back.name, "customer");
        testutil::remove(&repo);
    }

    /// A failed multi-file mutation leaves NO partial state: the complete
    /// change is validated before anything is written, so an invalid second
    /// endpoint (allowlist-violating name, dangling authored edge, or a
    /// constraint with an unresolvable `attaches-to`) writes nothing — the
    /// pre-existing files stay byte-identical and no new file lands.
    #[test]
    fn failed_multi_file_mutation_leaves_no_partial_state() {
        let (wt_apg, repo, _wt) = mutation_fixture("atomicity");
        // Pre-existing state: one committed node file.
        layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "existing")],
            &[],
        )
        .unwrap();
        let existing_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "existing");
        let before = std::fs::read_to_string(&existing_path).unwrap();

        // (a) An invalid second endpoint (allowlist-violating name) — neither
        // file lands, the pre-existing state is untouched.
        let bad = node("requirements", "requirement", "Bad Name");
        let err = layers::write_project(
            &wt_apg,
            &[node("requirements", "requirement", "fresh"), bad],
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("allowlist"), "{err}");
        let fresh_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "fresh");
        assert!(
            !fresh_path.exists(),
            "a failed mutation must not write the valid half"
        );
        assert_eq!(std::fs::read_to_string(&existing_path).unwrap(), before);

        // (b) A dangling authored edge endpoint — pairing fails validation,
        // nothing is written.
        let mut a = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "existing")
            .unwrap();
        a.out.push(OutEdge {
            kind: "depends-on".to_string(),
            target: "requirements.requirement.ghost".to_string(),
            properties: BTreeMap::new(),
        });
        let victim = node("requirements", "requirement", "victim");
        let err = layers::write_project(&wt_apg, &[a, victim], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("requirements.requirement.ghost"), "{msg}");
        let victim_path =
            layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "victim");
        assert!(!victim_path.exists());
        assert_eq!(std::fs::read_to_string(&existing_path).unwrap(), before);

        // (c) A constraint whose attaches-to does not resolve — refused at
        // write time (R14), before anything lands; the positive control
        // (resolving attachment) writes and re-merges.
        let mut c = node("requirements", "constraint", "law");
        c.properties.insert(
            layers::PROP_ATTACHES_TO.to_string(),
            "domain.entity.ghost".to_string(),
        );
        let err = layers::write_project(&wt_apg, &[c], &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("never a non-thing"), "{msg}");
        let law_path = layers::node_file_path(&wt_apg, Layer::Requirements, "constraint", "law");
        assert!(
            !law_path.exists(),
            "a refused constraint write must not land a file"
        );
        assert_eq!(std::fs::read_to_string(&existing_path).unwrap(), before);
        let mut c = node("requirements", "constraint", "law");
        c.properties.insert(
            layers::PROP_ATTACHES_TO.to_string(),
            "requirements.requirement.existing".to_string(),
        );
        layers::write_project(&wt_apg, &[c], &[]).unwrap();
        let db = ArtifactDb::open(&wt_apg).unwrap();
        assert!(db.has_node("requirements.constraint.law"));

        // The store still pairs cleanly throughout.
        layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
        testutil::remove(&repo);
    }

    /// The CLI-args shape the command arms take (positionals + repeatable
    /// flags).
    fn av(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The shared property-edit helper MERGEs and unsets: `{a:0,b:2}` with
    /// `--property a=1` yields `{a:1,b:2}`; adding `--unset-property b` yields
    /// `{a:1}`; omitting `--unset-property` never drops a key. `edge_update`
    /// reuses this helper.
    #[test]
    fn edit_properties_merges_and_unsets() {
        let base = BTreeMap::from([
            ("a".to_string(), "0".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        let set_only = edit_properties(&base, &parse_args(&av(&["--property", "a=1"])));
        assert_eq!(
            set_only,
            BTreeMap::from([
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
            ]),
            "MERGE overwrites only the passed key"
        );
        let with_unset = edit_properties(
            &base,
            &parse_args(&av(&["--property", "a=1", "--unset-property", "b"])),
        );
        assert_eq!(
            with_unset,
            BTreeMap::from([("a".to_string(), "1".to_string())]),
            "an explicit unset removes exactly the named key"
        );
        // Omitting --unset-property leaves every existing key alone.
        assert_eq!(
            edit_properties(&base, &parse_args(&av(&[]))),
            base,
            "no edit must not drop a key"
        );
    }

    /// Strict node surface against a real repo/branch DB: `node add` refuses an
    /// existing FQN (naming update/rm) and writes nothing; `node update` is
    /// edge-preserving (body/properties merge, incident edges identical) and is
    /// refused when the node is absent.
    #[test]
    fn node_add_refuses_existing_and_node_update_preserves_edges() {
        let (wt_apg, repo, _wt) = mutation_fixture("node-strict");
        // Author r1 --depends-on--> r2 through the real CLI arms.
        node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap();
        node_add(&wt_apg, &av(&["requirements", "requirement", "r2"])).unwrap();
        edge_add(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap();
        let before =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(before.out.len(), 1);
        let path = layers::node_file_path(&wt_apg, Layer::Requirements, "requirement", "r1");
        let bytes_before = std::fs::read_to_string(&path).unwrap();

        // Re-adding r1 is refused, naming both follow-ups; nothing is written.
        let err = node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg node update"), "{msg}");
        assert!(msg.contains("apg node rm"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes_before,
            "a refused re-add must write nothing"
        );

        // node update: body + merged property; every incident edge is identical.
        node_update(
            &wt_apg,
            &av(&[
                "requirements",
                "requirement",
                "r1",
                "--body",
                "updated body",
                "--property",
                "a=1",
            ]),
        )
        .unwrap();
        let after =
            layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(after.body, "updated body");
        assert_eq!(after.properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(
            after.out, before.out,
            "out-edges must survive a node update"
        );
        assert_eq!(
            after.in_edges, before.in_edges,
            "in-edges must survive a node update"
        );

        // Updating an absent node is refused.
        let err = node_update(&wt_apg, &av(&["requirements", "requirement", "ghost"])).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        testutil::remove(&repo);
    }

    /// Strict edge surface against a real repo/branch DB: `edge add` refuses an
    /// identical `(kind, from, to)`; `edge update` rewrites the source out-half
    /// AND the target in-half to the same MERGEd property map, with an explicit
    /// `--unset-property` the only way to drop a key.
    #[test]
    fn edge_add_refuses_duplicate_and_edge_update_merges_both_halves() {
        let (wt_apg, repo, _wt) = mutation_fixture("edge-strict");
        node_add(&wt_apg, &av(&["requirements", "requirement", "r1"])).unwrap();
        node_add(&wt_apg, &av(&["requirements", "requirement", "r2"])).unwrap();
        edge_add(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "a=0",
                "--property",
                "b=2",
            ]),
        )
        .unwrap();

        // A duplicate triple is refused, naming both follow-ups; both halves
        // still number exactly one.
        let err = edge_add(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg edge update"), "{msg}");
        assert!(msg.contains("apg edge rm"), "{msg}");
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        assert_eq!(r1.out.len(), 1, "the duplicate must not add an out-half");
        assert_eq!(
            r2.in_edges.len(),
            1,
            "the duplicate must not add an in-half"
        );

        // edge update --property a=1 MERGEs on BOTH halves: {a:1,b:2}.
        edge_update(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "a=1",
            ]),
        )
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        let expect = BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        assert_eq!(r1.out[0].properties, expect);
        assert_eq!(
            r2.in_edges[0].properties, expect,
            "the target in-half must carry the same map"
        );

        // --unset-property b drops exactly b on both halves: {a:1}.
        edge_update(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--unset-property",
                "b",
            ]),
        )
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        let r2 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r2").unwrap();
        let expect = BTreeMap::from([("a".to_string(), "1".to_string())]);
        assert_eq!(r1.out[0].properties, expect);
        assert_eq!(r2.in_edges[0].properties, expect);

        // Omitting --unset-property keeps every key.
        edge_update(
            &wt_apg,
            &av(&[
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
                "--property",
                "c=3",
            ]),
        )
        .unwrap();
        let r1 = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "r1").unwrap();
        assert_eq!(r1.out[0].properties.get("a").map(String::as_str), Some("1"));
        assert_eq!(r1.out[0].properties.get("c").map(String::as_str), Some("3"));

        // Updating an absent edge is refused.
        let err = edge_update(
            &wt_apg,
            &av(&[
                "drives",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        testutil::remove(&repo);
    }
}
