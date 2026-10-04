//! Edge spooling and resolution: the binary `Record` edge codec and reader,
//! plus the `Contains` pair and spec/plan/spine edge validators.

use std::collections::HashSet;
use std::io::{BufRead, Write};

use crate::graph::{Graph, NodeKind};
use crate::layers::{NodeProperties, properties_from_json, properties_json};
use crate::schema::Record;

/// Whether a `(from, to)` kind pair is a valid `Contains` edge (SPEC §7, R2,
/// R21): the six code pairs, the two plan pairs, the §3.1 requirements tree
/// (Stakeholder/User/Requirement ⊃ Requirement), the plain-named domain
/// hierarchy (Group ⊃ Group/Entity/Value/Service), and the solution hierarchy
/// (System ⊃ Container ⊃ Component).
pub(crate) fn valid_contains_pair(a: &NodeKind, b: &NodeKind) -> bool {
    matches!(
        (a, b),
        (NodeKind::Language, NodeKind::Module)
            | (NodeKind::Module, NodeKind::Module)
            | (NodeKind::Module, NodeKind::File)
            | (NodeKind::File, NodeKind::Struct)
            | (NodeKind::File, NodeKind::Function)
            | (NodeKind::Struct, NodeKind::Struct)
            | (NodeKind::Struct, NodeKind::Function)
            | (NodeKind::Plan, NodeKind::PlanPhase)
            | (NodeKind::PlanPhase, NodeKind::Task)
            // New-model §3.3 `contains` rows (apg-projects): the requirements
            // tree and the plain-named domain hierarchy.
            | (NodeKind::Stakeholder, NodeKind::Requirement)
            | (NodeKind::User, NodeKind::Requirement)
            | (NodeKind::Requirement, NodeKind::Requirement)
            | (NodeKind::Group, NodeKind::Group)
            | (NodeKind::Group, NodeKind::Entity)
            | (NodeKind::Group, NodeKind::Value)
            | (NodeKind::Group, NodeKind::Service)
            | (NodeKind::System, NodeKind::Container)
            | (NodeKind::Container, NodeKind::Component)
    )
}

pub(crate) fn kind_is(graph: &Graph, fqn: &str, k: NodeKind) -> bool {
    graph.nodes.get(fqn).is_some_and(|n| n.kind == k)
}

/// The Solution-tier (C4) node kinds: System, Container, Component.
pub(crate) fn is_solution_kind(graph: &Graph, fqn: &str) -> bool {
    graph.nodes.get(fqn).is_some_and(|n| {
        matches!(
            n.kind,
            NodeKind::System | NodeKind::Container | NodeKind::Component
        )
    })
}

/// Returns the edges of `edges` that pass `keep`. A free function so each call
/// scopes its immutable borrow of `graph` (unlike a capturing closure, which
/// would block a later mutable borrow).
pub(crate) fn filter_edges(
    graph: &Graph,
    edges: &HashSet<(String, String)>,
    keep: impl Fn(&Graph, &str, &str) -> bool,
) -> HashSet<(String, String)> {
    edges
        .iter()
        .filter(|(a, b)| keep(graph, a, b))
        .cloned()
        .collect()
}

/// Binary spool format for edge records: one u8 tag (0 contains, 1 calls,
/// 2 uses, 3 unresolved_call, 4 unresolved_use, 5 details, 6 reviews,
/// 7 depends_on, 8 gates, 9 satisfies, 10 drives, 11 represents,
/// 12 realised_by, 13 spec_implemented_by, 14 publishes, 15 subscribes)
/// followed by four length-prefixed UTF-8 strings (from, to, target_type,
/// properties; `target_type` is empty for most, `properties` is the canonical
/// JSON of the edge record's full properties map — `"{}"` for a record that
/// carries none, so the codec never drops an authored edge property).
pub(crate) fn write_edge(w: &mut impl Write, r: Record) {
    match r {
        Record::Contains { from, to, properties } => {
            write_edge_fields(w, 0, &from, &to, "", &properties)
        }
        Record::Calls { from, to, properties } => {
            write_edge_fields(w, 1, &from, &to, "", &properties)
        }
        Record::Uses { from, to, properties } => {
            write_edge_fields(w, 2, &from, &to, "", &properties)
        }
        Record::UnresolvedCall {
            from,
            to,
            target_type,
        } => write_edge_fields(w, 3, &from, &to, &target_type, &NodeProperties::new()),
        Record::UnresolvedUse { from, to } => {
            write_edge_fields(w, 4, &from, &to, "", &NodeProperties::new())
        }
        Record::Details { from, to, properties } => {
            write_edge_fields(w, 5, &from, &to, "", &properties)
        }
        Record::Reviews { from, to } => {
            write_edge_fields(w, 6, &from, &to, "", &NodeProperties::new())
        }
        Record::DependsOn { from, to, properties } => {
            write_edge_fields(w, 7, &from, &to, "", &properties)
        }
        Record::Gates { from, to } => {
            write_edge_fields(w, 8, &from, &to, "", &NodeProperties::new())
        }
        Record::Satisfies { from, to } => {
            write_edge_fields(w, 9, &from, &to, "", &NodeProperties::new())
        }
        Record::Drives { from, to, properties } => {
            write_edge_fields(w, 10, &from, &to, "", &properties)
        }
        Record::Represents { from, to, properties } => {
            write_edge_fields(w, 11, &from, &to, "", &properties)
        }
        Record::RealisedBy { from, to, properties } => {
            write_edge_fields(w, 12, &from, &to, "", &properties)
        }
        Record::SpecImplementedBy { from, to, properties } => {
            write_edge_fields(w, 13, &from, &to, "", &properties)
        }
        Record::Publishes { from, to, properties } => {
            write_edge_fields(w, 14, &from, &to, "", &properties)
        }
        Record::Subscribes { from, to, properties } => {
            write_edge_fields(w, 15, &from, &to, "", &properties)
        }
        other => unreachable!("non-edge record reached the edge spool: {other:?}"),
    }
}

fn write_edge_fields(
    w: &mut impl Write,
    tag: u8,
    a: &str,
    b: &str,
    c: &str,
    properties: &NodeProperties,
) {
    w.write_all(&[tag]).unwrap();
    let d = properties_json(properties);
    for s in [a, b, c, d.as_str()] {
        w.write_all(&(s.len() as u32).to_le_bytes()).unwrap();
        w.write_all(s.as_bytes()).unwrap();
    }
}

pub(crate) struct EdgeReader<R: BufRead> {
    pub(crate) r: R,
}

impl<R: BufRead> EdgeReader<R> {
    pub(crate) fn next_edge(&mut self) -> Option<Record> {
        let mut tag = [0u8; 1];
        if self.r.read_exact(&mut tag).is_err() {
            return None;
        }
        let a = self.read_str();
        let b = self.read_str();
        let c = self.read_str();
        let d = self.read_str();
        // The writer always emits the canonical JSON of the record's full
        // properties map (`"{}"` for an empty one), so decoding never fails on
        // a spool this module wrote; a corrupt spool panics loudly rather than
        // silently dropping an authored edge property.
        let properties =
            properties_from_json(&d).expect("edge spool properties are canonical JSON");
        Some(match tag[0] {
            0 => Record::Contains {
                from: a,
                to: b,
                properties,
            },
            1 => Record::Calls {
                from: a,
                to: b,
                properties,
            },
            2 => Record::Uses {
                from: a,
                to: b,
                properties,
            },
            3 => Record::UnresolvedCall {
                from: a,
                to: b,
                target_type: c,
            },
            4 => Record::UnresolvedUse { from: a, to: b },
            5 => Record::Details {
                from: a,
                to: b,
                properties,
            },
            6 => Record::Reviews { from: a, to: b },
            7 => Record::DependsOn {
                from: a,
                to: b,
                properties,
            },
            8 => Record::Gates { from: a, to: b },
            9 => Record::Satisfies { from: a, to: b },
            10 => Record::Drives {
                from: a,
                to: b,
                properties,
            },
            11 => Record::Represents {
                from: a,
                to: b,
                properties,
            },
            12 => Record::RealisedBy {
                from: a,
                to: b,
                properties,
            },
            13 => Record::SpecImplementedBy {
                from: a,
                to: b,
                properties,
            },
            14 => Record::Publishes {
                from: a,
                to: b,
                properties,
            },
            15 => Record::Subscribes {
                from: a,
                to: b,
                properties,
            },
            t => panic!("bad edge spool tag: {t}"),
        })
    }

    fn read_str(&mut self) -> String {
        let mut len = [0u8; 4];
        self.r.read_exact(&mut len).unwrap();
        let n = u32::from_le_bytes(len) as usize;
        let mut buf = vec![0u8; n];
        self.r.read_exact(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }
}
