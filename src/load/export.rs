//! The `graph.jsonl` export: the `Export` record enum and
//! [`write_graph_jsonl`], which re-serializes the assembled graph with
//! canonical FQNs (nodes) and resolved endpoints (edges), without opaque ids.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::Serialize;

use crate::graph::{Graph, NodeKind};

use super::parquet::{lines, loc};

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Export {
    /// The export control record, written as **line 1** of graph.jsonl from a
    /// `Scan` graph node (the git state the scan ran under). Git fields are
    /// absent when the scan was not in a git repo; `content_key` is the
    /// phase-01 content-identity key (`recorded_scan` reads it on the next
    /// scan to decide the fast-path).
    ScanMeta {
        #[serde(skip_serializing_if = "Option::is_none")]
        git_sha: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_clean: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_key: Option<String>,
        scanned_at: String,
    },
    Module {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    /// A language-root node (`lang_switch` id, e.g. `rust`) — the root of the
    /// `Language -Contains-> Module` hierarchy (PHASE_09 language rooting).
    Language {
        fqn: String,
    },
    Struct {
        fqn: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
        code_type: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    Function {
        fqn: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
        code_type: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    File {
        fqn: String,
        start_line: u32,
        end_line: u32,
        code_type: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    Unresolved {
        fqn: String,
        category: String,
    },
    Contains {
        from: String,
        to: String,
    },
    Calls {
        from: String,
        to: String,
    },
    Uses {
        from: String,
        to: String,
    },
    UnresolvedCall {
        from: String,
        to: String,
        target_type: String,
    },
    UnresolvedUse {
        from: String,
        to: String,
    },
    Requirement {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        id: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        feature: String,
    },
    Note {
        fqn: String,
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
    },
    Feedback {
        fqn: String,
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        disposition: String,
    },
    Plan {
        fqn: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        strategy: String,
    },
    PlanPhase {
        fqn: String,
        number: u32,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        deliverable: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    Task {
        fqn: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        tier: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        verb: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        target: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        new_fqn: String,
    },
    Stakeholder {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Entity {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    System {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Container {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Component {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    User {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Group {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        attribute: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        root: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Value {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Service {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Person {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Constraint {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
        #[serde(rename = "attaches-to", skip_serializing_if = "String::is_empty")]
        attaches_to: String,
    },
    Details {
        from: String,
        to: String,
    },
    Reviews {
        from: String,
        to: String,
    },
    DependsOn {
        from: String,
        to: String,
    },
    Gates {
        from: String,
        to: String,
    },
    Satisfies {
        from: String,
        to: String,
    },
    Drives {
        from: String,
        to: String,
    },
    Represents {
        from: String,
        to: String,
    },
    #[serde(rename = "realised-by")]
    RealisedBy {
        from: String,
        to: String,
    },
    #[serde(rename = "implemented-by")]
    SpecImplementedBy {
        from: String,
        to: String,
    },
    Publishes {
        from: String,
        to: String,
    },
    Subscribes {
        from: String,
        to: String,
    },
}

/// Writes `graph.jsonl`: the final graph re-serialized with canonical FQNs
/// (nodes) and resolved endpoints (edges), without opaque ids. A `Scan` node
/// exports as a `scan_meta` control record on **line 1** (before every node
/// and edge line), mirroring the `lang_switch` control-record convention.
///
/// Identity contract (`requirements.requirement.portable-graph-identity`,
/// `solution.constraint.graph-stores-relative-only`): the graph this serializes
/// carries repo-relative identities only — a File node's `fqn` is its
/// `/`-separated path relative to the git toplevel (the scan root when
/// non-git), and a Struct/Function's `path` is the same source-file identity.
/// No absolute checkout path is emitted; the absolute path is reconstructed in
/// the suite-tool boundary against the caller's project directory.
///
/// **Lossless-layers consistency (phase-4 task-15).** The durable authored
/// node/edge properties now travel on `Graph` (`Node::properties` and
/// `Graph::edge_properties`) so the DB projection can be lossless
/// (`create_schema`/`build_load_files` write the serialized-properties column;
/// the session's `merge_records` re-merges it). This export is a *stable view*
/// of that graph: it deliberately does **not** serialize those properties, so
/// the `Export` record shape — and therefore `graph.jsonl` line-for-line
/// output — is unchanged for code records AND for the durable authored
/// records/edges, and the transient plan/feedback sets keep their current
/// shape. A `read_graph_jsonl` → `write_graph_jsonl` round-trip stays
/// byte-stable, and the export reader (`code_universes_from_export`, the
/// splice/cache comparisons) needs no property handling. The properties live in
/// the DB projection (`ArtifactDb::node_files_from_db`), which is the
/// phase-4 round-trip source of truth — not in this JSONL.
pub fn write_graph_jsonl(graph: &Graph, path: &Path) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let write_line = |w: &mut BufWriter<File>, v: &Export| -> anyhow::Result<()> {
        w.write_all(serde_json::to_string(v)?.as_bytes())?;
        w.write_all(b"\n")?;
        Ok(())
    };

    // The scan_meta control record leads the export (line 1). A Scan node is
    // not emitted again in the node loop below.
    for (_, node) in graph.nodes.iter().filter(|(_, n)| n.kind == NodeKind::Scan) {
        write_line(
            &mut w,
            &Export::ScanMeta {
                git_sha: node.git_sha.clone(),
                git_clean: node.git_clean,
                content_key: node.content_key.clone(),
                scanned_at: node.scanned_at.clone().unwrap_or_default(),
            },
        )?;
    }

    for (fqn, node) in &graph.nodes {
        let rec = match node.kind {
            NodeKind::Scan => continue,
            NodeKind::Module => Export::Module {
                fqn: fqn.clone(),
                status: node.status.clone().unwrap_or_default(),
            },
            NodeKind::Language => Export::Language { fqn: fqn.clone() },
            NodeKind::Struct => {
                let (path, start, end) = loc(graph, fqn);
                let (start_line, end_line) = lines(graph, fqn);
                Export::Struct {
                    fqn: fqn.clone(),
                    path,
                    start: start as u32,
                    end: end as u32,
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                    status: node.status.clone().unwrap_or_default(),
                }
            }
            NodeKind::Function => {
                let (path, start, end) = loc(graph, fqn);
                let (start_line, end_line) = lines(graph, fqn);
                Export::Function {
                    fqn: fqn.clone(),
                    path,
                    start: start as u32,
                    end: end as u32,
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                    status: node.status.clone().unwrap_or_default(),
                }
            }
            NodeKind::File => {
                let (start_line, end_line) = lines(graph, fqn);
                Export::File {
                    fqn: fqn.clone(),
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                    status: node.status.clone().unwrap_or_default(),
                }
            }
            NodeKind::UnresolvedTarget => Export::Unresolved {
                fqn: fqn.clone(),
                category: node.category.clone().unwrap_or_default(),
            },
            NodeKind::Requirement => Export::Requirement {
                fqn: fqn.clone(),
                id: node.id.clone().unwrap_or_default(),
                title: node.title.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
                feature: node.feature.clone().unwrap_or_default(),
            },
            NodeKind::Note => Export::Note {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
            },
            NodeKind::Feedback => Export::Feedback {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
                status: node.status.clone().unwrap_or_default(),
                disposition: node.disposition.clone().unwrap_or_default(),
            },
            NodeKind::Plan => Export::Plan {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                strategy: node.strategy.clone().unwrap_or_default(),
            },
            NodeKind::PlanPhase => Export::PlanPhase {
                fqn: fqn.clone(),
                number: node.number.unwrap_or_default(),
                title: node.title.clone().unwrap_or_default(),
                deliverable: node.deliverable.clone().unwrap_or_default(),
                status: node.status.clone().unwrap_or_default(),
            },
            NodeKind::Task => Export::Task {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                tier: node.tier.clone().unwrap_or_default(),
                status: node.status.clone().unwrap_or_default(),
                verb: node.verb.clone().unwrap_or_default(),
                target: node.target.clone().unwrap_or_default(),
                new_fqn: node.new_fqn.clone().unwrap_or_default(),
            },
            NodeKind::Stakeholder => Export::Stakeholder {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Entity => Export::Entity {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::System => Export::System {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Container => Export::Container {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Component => Export::Component {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::User => Export::User {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Group => Export::Group {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                attribute: node.attribute.clone().unwrap_or_default(),
                root: node.root.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Value => Export::Value {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Service => Export::Service {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Person => Export::Person {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Constraint => Export::Constraint {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
                attaches_to: node.attaches_to.clone().unwrap_or_default(),
            },
        };
        write_line(&mut w, &rec)?;
    }
    for (a, b) in &graph.contains {
        write_line(
            &mut w,
            &Export::Contains {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.calls {
        write_line(
            &mut w,
            &Export::Calls {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.uses {
        write_line(
            &mut w,
            &Export::Uses {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b, t) in &graph.unresolved_calls {
        write_line(
            &mut w,
            &Export::UnresolvedCall {
                from: a.clone(),
                to: b.clone(),
                target_type: t.clone(),
            },
        )?;
    }
    for (a, b) in &graph.unresolved_uses {
        write_line(
            &mut w,
            &Export::UnresolvedUse {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.details {
        write_line(
            &mut w,
            &Export::Details {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.reviews {
        write_line(
            &mut w,
            &Export::Reviews {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.depends_on {
        write_line(
            &mut w,
            &Export::DependsOn {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.gates {
        write_line(
            &mut w,
            &Export::Gates {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.satisfies {
        write_line(
            &mut w,
            &Export::Satisfies {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.drives {
        write_line(
            &mut w,
            &Export::Drives {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.represents {
        write_line(
            &mut w,
            &Export::Represents {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.realised_by {
        write_line(
            &mut w,
            &Export::RealisedBy {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.spec_implemented_by {
        write_line(
            &mut w,
            &Export::SpecImplementedBy {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.publishes {
        write_line(
            &mut w,
            &Export::Publishes {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.subscribes {
        write_line(
            &mut w,
            &Export::Subscribes {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    w.flush()?;
    Ok(())
}
