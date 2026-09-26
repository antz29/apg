//! Per-file structure accumulation: the `Struct`/`contains` facts an emitter
//! produces for one claimed file.

use std::collections::HashSet;

use crate::lines::Span;
use crate::record::Rec;

/// The structure facts one format emitter produces for a single claimed file:
/// its `Struct` records plus the `contains` edges that nest them. An emitter
/// NEVER emits `Module`/`File` — `run` assembles those around it — and draws
/// opaque ids from the shared `id_prefix`/`next_id` counter so ids stay unique
/// across the whole spawn.
pub(crate) struct Structure {
    pub(crate) structs: Vec<Rec>,
    pub(crate) contains: Vec<Rec>,
}

/// Accumulates one file's structure facts: the `Struct` records plus the
/// parent→child `contains` edges that nest them. A top-level entry is
/// file-rooted (its `parent` is the file path, mirroring the absorbed md
/// heading Structs); a nested entry's parent is its parent's rendered FQN.
/// Names are disambiguated per parent so no two records in a file render the
/// same `parent.name` FQN (the ingestor panics on a duplicate claim).
pub(crate) struct StructureBuilder<'a> {
    path: String,
    id_prefix: &'a str,
    next_id: &'a mut u64,
    structs: Vec<Rec>,
    contains: Vec<Rec>,
    assigned: HashSet<String>,
}

impl<'a> StructureBuilder<'a> {
    pub(crate) fn new(path: &str, id_prefix: &'a str, next_id: &'a mut u64) -> Self {
        Self {
            path: path.to_string(),
            id_prefix,
            next_id,
            structs: Vec::new(),
            contains: Vec::new(),
            assigned: HashSet::new(),
        }
    }

    /// Adds one `Struct` under `parent` — `(id, fqn)` of the parent, or `None`
    /// for a file-rooted top-level entry — returning the new `(id, fqn)` so the
    /// caller can nest children under it. The `contains` edge parent→child is
    /// emitted only for a nested entry.
    pub(crate) fn add(
        &mut self,
        parent: Option<(&str, &str)>,
        name: &str,
        span: Span,
    ) -> (String, String) {
        let (parent_id, parent_fqn) = match parent {
            Some((id, fqn)) => (Some(id), fqn.to_string()),
            None => (None, self.path.clone()),
        };
        let name = self.unique(&parent_fqn, name);
        let fqn = format!("{parent_fqn}.{name}");
        let id = format!("{}{}", self.id_prefix, *self.next_id);
        *self.next_id += 1;
        if let Some(pid) = parent_id {
            self.contains.push(Rec::Contains {
                from: pid.to_string(),
                to: id.clone(),
            });
        }
        self.structs.push(Rec::Struct {
            id: id.clone(),
            parent: parent_fqn,
            name,
            path: self.path.clone(),
            start: span.start,
            end: span.end,
            start_line: span.start_line,
            end_line: span.end_line,
        });
        (id, fqn)
    }

    /// Disambiguates `name` among its siblings so `parent.name` is unique in
    /// this file: the first occurrence keeps `name`, later ones get `-1`, `-2`, …
    fn unique(&mut self, parent: &str, name: &str) -> String {
        let base = if name.is_empty() { "section" } else { name };
        let mut candidate = base.to_string();
        let mut k: u64 = 0;
        loop {
            if self.assigned.insert(format!("{parent}.{candidate}")) {
                return candidate;
            }
            k += 1;
            candidate = format!("{base}-{k}");
        }
    }

    pub(crate) fn finish(self) -> Structure {
        Structure {
            structs: self.structs,
            contains: self.contains,
        }
    }
}
