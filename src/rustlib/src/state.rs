//! Scanner accumulator state and the immutable endpoint view used by pass 2.

use std::collections::{HashMap, HashSet};

use crate::record::{Decl, ImplEdge};

pub(crate) struct State {
    pub(crate) next_id: usize,
    /// Canonical FQN (`parent.name`) -> id, for all struct-like nodes.
    pub(crate) struct_id: HashMap<String, String>,
    /// (path, byte offset) -> id, for every declared struct and function.
    pub(crate) id_by_source: HashMap<(String, u32), String>,
    /// (path, byte offset) -> id, for every declared struct-like node.
    pub(crate) struct_sources: HashMap<(String, u32), String>,
    /// id -> the canonical FQN the ingestor renders for it (including the
    /// `parent.name(params)` overload suffix). Built over the FULL collected
    /// declaration set — the full resolution context — so a cross-crate edge to
    /// a declaration outside the emission target set can carry its canonical
    /// FQN instead of a dangling opaque id (phase-05 task-8).
    pub(crate) id_fqn: HashMap<String, String>,
    /// The ids whose node records are actually part of the emitted stream (the
    /// target set, or every declaration when no filter is in force). An edge to
    /// an id NOT in this set carries the target's canonical FQN, which the
    /// ingestor's cached-fact splice resolves against the reused unit.
    pub(crate) emitted_id: HashSet<String>,
    /// Dedup of unresolved records by fqn (first category wins).
    pub(crate) unresolved_seen: HashSet<String>,
    pub(crate) impl_edges: Vec<ImplEdge>,
}

impl State {
    pub(crate) fn new() -> State {
        State {
            next_id: 0,
            struct_id: HashMap::new(),
            id_by_source: HashMap::new(),
            struct_sources: HashMap::new(),
            id_fqn: HashMap::new(),
            emitted_id: HashSet::new(),
            unresolved_seen: HashSet::new(),
            impl_edges: Vec::new(),
        }
    }

    /// The endpoint to emit for an edge target with opaque `id`: the id when the
    /// declaration is part of the emitted stream, else the declaration's
    /// canonical FQN. With no filter in force every id is emitted, so this is
    /// always `id` — byte-identical to a full scan.
    pub(crate) fn endpoint(&self, id: &str) -> String {
        if self.emitted_id.contains(id) {
            id.to_string()
        } else {
            self.id_fqn
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }
    }
}

/// Immutable view of the resolution maps used by the pass-2 syntax walk. Bundled
/// so handlers can resolve an edge target to the opaque id (or its canonical
/// FQN when the target is outside the emission target set) while the walk still
/// mutates `unresolved_seen` independently.
pub(crate) struct Endpoints<'a> {
    pub(crate) id_by_source: &'a HashMap<(String, u32), String>,
    pub(crate) struct_sources: &'a HashMap<(String, u32), String>,
    pub(crate) id_fqn: &'a HashMap<String, String>,
    pub(crate) emitted_id: &'a HashSet<String>,
}

impl Endpoints<'_> {
    pub(crate) fn endpoint(&self, id: &str) -> String {
        if self.emitted_id.contains(id) {
            id.to_string()
        } else {
            self.id_fqn
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }
    }

    /// Endpoint for a function/struct source key, when it resolves.
    pub(crate) fn fn_id(&self, key: &(String, u32)) -> Option<String> {
        self.id_by_source.get(key).map(|id| self.endpoint(id))
    }

    /// Endpoint for a struct-like source key, when it resolves.
    pub(crate) fn struct_id(&self, key: &(String, u32)) -> Option<String> {
        self.struct_sources.get(key).map(|id| self.endpoint(id))
    }
}

pub(crate) fn push_decl(state: &mut State, decls: &mut Vec<Decl>, mut d: Decl, emit: bool) {
    // Ids are assigned later, in sorted (path, start) order, for deterministic
    // output. Until then the decl carries an empty id.
    d.emit = emit;
    decls.push(d);
    let _ = state;
}
