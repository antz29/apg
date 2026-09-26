//! Per-format structure emitters.
//
// Each emitter returns ONE claimed file's `Struct`/`contains` facts (never
// `Module`/`File` — `run` owns those) under the pinned
// `(path, bytes, id_prefix, next_id)` shape, drawing opaque ids from the shared
// counter. Structure depth is deliberately minimal but useful
// (`requirements.constraint.structural-format-structure-table`): shell
// functions, top-level keys/jobs/steps, tables/keys, elements,
// stages/instructions, targets/variables/includes, sections/keys.

use std::path::Path;

use crate::builder::Structure;

pub(crate) mod dockerfile;
pub(crate) mod ini;
pub(crate) mod json;
pub(crate) mod makefile;
pub(crate) mod sh;
pub(crate) mod toml;
pub(crate) mod xml;
pub(crate) mod yaml;

/// The `misc` residual: a file no format stream (and no code frontend) claims
/// gets its `File` record only — no structure facts.
pub(crate) fn emit_misc(
    _path: &Path,
    _bytes: &[u8],
    _id_prefix: &str,
    _next_id: &mut u64,
) -> Structure {
    Structure {
        structs: Vec::new(),
        contains: Vec::new(),
    }
}
