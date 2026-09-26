//! Canonical identity rendering: dotted module identities and the ingestor's
//! canonical function/struct FQNs (SPEC §4).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Dotted module identity from the file's stem and its init-bearing ancestor
/// directory names in NEAREST-FIRST order. `__init__.py`/`__init__.pyi`
/// contribute only their package path; every other file appends its stem.
pub(crate) fn dotted_identity(
    is_init: bool,
    stem: &str,
    ancestors_nearest_first: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !is_init {
        parts.push(stem.to_string());
    }
    parts.extend(ancestors_nearest_first.iter().cloned());
    parts.reverse();
    if parts.is_empty() {
        stem.to_string()
    } else {
        parts.join(".")
    }
}

/// Canonical FQN for a function exactly as the ingestor renders it (SPEC §4):
/// `parent.name`, or `parent.name(T1,T2,...)` when its `(parent, name)` group
/// is overloaded (the `()` form is retained for an empty param list).
pub(crate) fn canonical_function_fqn(
    parent: &str,
    name: &str,
    params: &[String],
    overloaded: bool,
) -> String {
    if overloaded {
        let joined = params.join(",");
        format!("{parent}.{name}({joined})")
    } else {
        format!("{parent}.{name}")
    }
}

/// Canonical FQN for a class: always `parent.name`.
pub(crate) fn canonical_struct_fqn(parent: &str, name: &str) -> String {
    format!("{parent}.{name}")
}

/// Maps an impact-set of absolute target paths to their module identities
/// (pure seam for task-7's package/module-granularity emission filter).
pub(crate) fn identities_from_targets<F>(targets: &[PathBuf], identity_of: F) -> HashSet<String>
where
    F: Fn(&Path) -> String,
{
    targets.iter().map(|p| identity_of(p.as_path())).collect()
}
