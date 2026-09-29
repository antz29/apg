//! Durable-tree ingestion (SPEC §4.1): walk `apg/layers/`, deserialize each
//! node file, validate the assembled set, and convert it into the unified
//! `Record` stream the scan chains after the scanner records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::schema::Record;

use super::catalog::{LAYERS_DIR, LAYERS_TREE, Layer};
use super::code_refs::validate_code_refs;
use super::node_file::{NodeFile, fqn};
use super::validate::{check_edge_pairing, eval_constraint};
use super::validate_assembled_rules;

// ---------------------------------------------------------------------------
// SPEC §4.1 — tree ingestion (phase-3 task-15)
// ---------------------------------------------------------------------------

/// Ingest the durable `apg/layers/` tree into the new-model graph records
/// (apg-projects SPEC §4.1): walk [`LAYERS_TREE`], deserialize every
/// `<layer>/<type>/<name>.json` into a [`NodeFile`], validate the assembled
/// set, and convert it into the unified-JSONL `Record` stream `cmd_scan`
/// chains after the scanner records.
///
/// Validation (all ERRORs, never silent):
/// - [`check_edge_pairing`] over every node file — an in/out edge in one file
///   without the matching out/in edge (same source, kind, target, AND
///   properties) in the other endpoint's file is refused (R16 AC).
/// - [`validate_assembled_rules`] over the assembled set — contains/depends-on
///   trees acyclic ([`validate_trees_acyclic`]) and `publishes`/`subscribes`
///   targets are `Entity (kind: event)` ([`validate_event_targets`]) (SPEC
///   §3.3 property rules).
/// - [`validate_code_refs`] on every `implemented-by` target against
///   `scanned_code` (the code FQNs the just-run scan produced) and `planned`
///   (the plan's planned-node FQNs, from `.trans`): resolves → real; planned →
///   pending (not an error); gone from both → spec-drift error.
/// - [`eval_constraint`] on every `constraint` node (structure validation
///   only — name allowlist, type-in-layer, uniqueness; satisfaction is
///   review-only, R14).
///
/// The FQN of each node is derived from its **path** (`fqn(layer, type,
/// name)`), never read — the file name IS the identity, and the file's own
/// `layer`/`type`/`name` fields must match the path (SPEC §4.1). Out-edges
/// are the canonical source for the emitted edge records; in-edges are
/// verified (pairing) but never emitted.
pub fn ingest_tree(
    apg_root: &Path,
    scanned_code: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<Vec<Record>> {
    // Walk the durable tree, deserializing one NodeFile per `<name>.json`.
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
                // The file name IS the identity: the layer/type/name fields
                // must match the path segments (SPEC §4.1).
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
    // Deterministic record order (one file per node, so paths are unique).
    nodes.sort_by(|a, b| (&a.layer, &a.node_type, &a.name).cmp(&(&b.layer, &b.node_type, &b.name)));

    // The assembled identity universe + FQN → node map: the constraint check
    // and the property-aware §3.3 rules both resolve against them.
    let universe: BTreeSet<(Layer, String, String)> = nodes
        .iter()
        .map(|n| (layer_of(&n.layer), n.node_type.clone(), n.name.clone()))
        .collect();
    let assembled: BTreeMap<String, &NodeFile> = nodes
        .iter()
        .map(|n| (fqn(layer_of(&n.layer), &n.node_type, &n.name), n))
        .collect();

    // 1. Pairwise in/out symmetry across ALL node files (R16 AC).
    check_edge_pairing(&nodes)?;

    // 2. Property-aware §3.3 rules over the assembled set: contains/depends-on
    // trees acyclic, and publishes/subscribes targets are Entity (kind: event).
    validate_assembled_rules(&assembled, &universe)?;

    // 3. `implemented-by` code refs against the scanned graph + planned nodes.
    let refs: Vec<&str> = nodes
        .iter()
        .flat_map(|n| n.out.iter())
        .filter(|oe| oe.kind == "implemented-by")
        .map(|oe| oe.target.as_str())
        .collect();
    validate_code_refs(&refs, scanned_code, planned)?;

    // 4. Constraint structure/reference validation over the assembled graph.
    for n in &nodes {
        if n.node_type != "constraint" {
            continue;
        }
        // The constraint under validation is not part of its own existence
        // universe: `eval_constraint` reuses `validate_node`'s uniqueness
        // rule, which compares against everything EXCEPT the node being
        // validated (the same exclusion `validate_change` applies). Duplicates
        // among constraints are still caught — each one validates against the
        // other's presence.
        let mut own_universe = universe.clone();
        own_universe.remove(&(layer_of(&n.layer), n.node_type.clone(), n.name.clone()));
        eval_constraint(layer_of(&n.layer), &n.name, &n.properties, &own_universe)?;
    }

    // Convert node files + out-edges into records (out is canonical).
    let mut records = Vec::new();
    for n in &nodes {
        let layer = layer_of(&n.layer);
        let f = fqn(layer, &n.node_type, &n.name);
        records.push(node_record(&f, &n.node_type, n)?);
        for oe in &n.out {
            records.push(edge_record(&f, &oe.kind, &oe.target)?);
        }
    }
    Ok(records)
}

/// Resolve a node file's `layer` string to its [`Layer`] — a hard error on an
/// unknown layer (the catalog is closed).
pub(crate) fn layer_of(layer_dir: &str) -> Layer {
    match Layer::ALL.iter().find(|l| l.layer_dir() == layer_dir) {
        Some(l) => *l,
        None => panic!("unknown layer `{layer_dir}`"),
    }
}

/// Convert one node file into its new-model node record. `f` is the derived
/// FQN (`<layer>.<type>.<name>`); `properties` carries the §3.1 attributes
/// (entity/container `kind`, group `attribute`/`root`, constraint
/// `attaches-to`, a requirement's metadata `id`/`feature`) — short ids are
/// metadata, never identity.
fn node_record(f: &str, node_type: &str, n: &NodeFile) -> anyhow::Result<Record> {
    let name = n.name.clone();
    let body = n.body.clone();
    let prop = |k: &str| n.properties.get(k).cloned().unwrap_or_default();
    Ok(match node_type {
        "stakeholder" => Record::Stakeholder {
            fqn: f.to_string(),
            name,
            body,
        },
        "user" => Record::User {
            fqn: f.to_string(),
            name,
            body,
        },
        "requirement" => Record::Requirement {
            fqn: f.to_string(),
            id: prop("id"),
            title: name,
            body,
            feature: prop("feature"),
        },
        "note" => Record::Note {
            fqn: f.to_string(),
            body,
            kind: prop("kind"),
        },
        "constraint" => Record::Constraint {
            fqn: f.to_string(),
            name,
            body,
            attaches_to: prop("attaches-to"),
        },
        "group" => Record::Group {
            fqn: f.to_string(),
            name,
            attribute: prop("attribute"),
            root: prop("root"),
            body,
        },
        "entity" => Record::Entity {
            fqn: f.to_string(),
            name,
            body,
        },
        "value" => Record::Value {
            fqn: f.to_string(),
            name,
            body,
        },
        "service" => Record::Service {
            fqn: f.to_string(),
            name,
            body,
        },
        "system" => Record::System {
            fqn: f.to_string(),
            name,
            body,
        },
        "container" => Record::Container {
            fqn: f.to_string(),
            name,
            kind: prop("kind"),
            body,
        },
        "component" => Record::Component {
            fqn: f.to_string(),
            name,
            body,
        },
        "person" => Record::Person {
            fqn: f.to_string(),
            name,
            body,
        },
        other => anyhow::bail!("unknown node type `{other}` in layer `{}`", n.layer),
    })
}

/// Convert one out-edge into its new-model edge record. `from` is the source
/// node's derived FQN. The §3.3 kebab kinds map to the new-model records
/// (`realised-by` → `RealisedBy`, `implemented-by` → `SpecImplementedBy`);
/// `contains`/`drives`/`calls`/`uses`/`represents`/`details`/`depends-on` map
/// to the shared records the ingestor routes by endpoint node-kind.
fn edge_record(from: &str, kind: &str, to: &str) -> anyhow::Result<Record> {
    let from = from.to_string();
    let to = to.to_string();
    Ok(match kind {
        "contains" => Record::Contains { from, to },
        "drives" => Record::Drives { from, to },
        "realised-by" => Record::RealisedBy { from, to },
        "implemented-by" => Record::SpecImplementedBy { from, to },
        "calls" => Record::Calls { from, to },
        "publishes" => Record::Publishes { from, to },
        "subscribes" => Record::Subscribes { from, to },
        "depends-on" => Record::DependsOn { from, to },
        "uses" => Record::Uses { from, to },
        "represents" => Record::Represents { from, to },
        "details" => Record::Details { from, to },
        other => anyhow::bail!("unknown edge kind `{other}`"),
    })
}
