use std::collections::BTreeSet;

use crate::graph::Graph;

use super::depindex::DepIndex;
use super::signature::{OverloadIndex, overload_groups_of_file, overload_peer_files};

// ---------------------------------------------------------------------------
// The impact closure
// ---------------------------------------------------------------------------

/// The **re-emission target set** for a delta, derived from the cached graph:
///
/// ```text
/// changed files
///   ∪ reverse-dependency closure (unless every changed file is body-only)
///   ∪ overload-group peers of changed files
/// ```
///
/// `signature_changed` is the set of changed files whose exported signature
/// actually changed; a changed file NOT in this set is body-only and does not
/// pull its dependents. (An added/removed file always counts as a signature
/// change — its declaration surface appeared or vanished.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetSet {
    pub files: BTreeSet<String>,
    /// The subset of `files` that were pulled in as reverse dependencies of a
    /// signature change (diagnostics / logging).
    pub dependents: BTreeSet<String>,
    /// The subset of `files` pulled in as overload peers (diagnostics).
    pub overload_peers: BTreeSet<String>,
}

impl TargetSet {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether `rel`/`abs` is in the target set.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }
}

/// Computes the re-emission target set.
///
/// * `changed` — the changed files (absolute paths) from the git delta/manifest.
/// * `signature_changed` — those changed files whose exported signature changed.
///   Files in `changed` but not here are body-only and do not cascade.
/// * `index` — the cached file-level dependency index.
/// * `cached_graph` — the previous graph (for the overload groups).
pub fn target_set(
    changed: &BTreeSet<String>,
    signature_changed: &BTreeSet<String>,
    index: &DepIndex,
    cached_graph: &Graph,
) -> TargetSet {
    let mut out = TargetSet {
        files: changed.clone(),
        ..TargetSet::default()
    };

    // Reverse-dependency closure: only a signature change cascades. A
    // body-only change (in `changed` but not `signature_changed`) is cut off.
    let cascade_seeds: BTreeSet<String> =
        changed.intersection(signature_changed).cloned().collect();
    if !cascade_seeds.is_empty() {
        let closure = index.reverse_closure(&cascade_seeds);
        for f in &closure {
            if !out.files.contains(f) {
                out.files.insert(f.clone());
                out.dependents.insert(f.clone());
            }
        }
    }

    // Overload-group peers: a changed file containing a function pulls in every
    // file declaring a function under the same scope (the FQNs re-suffix
    // graph-wide). Scope-based so a newly-added overload is still covered.
    let mut scopes: BTreeSet<String> = BTreeSet::new();
    for f in changed {
        scopes.extend(overload_groups_of_file(cached_graph, f));
    }
    if !scopes.is_empty() {
        let peers = overload_peer_files(cached_graph, &scopes);
        for p in peers {
            if out.files.insert(p.clone()) {
                out.overload_peers.insert(p);
            }
        }
    }

    out
}

/// The portable (checkout-relative) target closure: changed ∪ reverse-dep
/// closure (of a signature change) ∪ overload peers. Returns checkout-relative
/// paths — the entry point a fresh worktree uses directly.
pub fn target_set_rel(
    changed_rel: &BTreeSet<String>,
    signature_changed_rel: &BTreeSet<String>,
    index: &DepIndex,
    overloads: &OverloadIndex,
) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = changed_rel.clone();

    let cascade_seeds: BTreeSet<String> = changed_rel
        .intersection(signature_changed_rel)
        .cloned()
        .collect();
    if !cascade_seeds.is_empty() {
        out.extend(index.reverse_closure(&cascade_seeds));
    }

    for f in changed_rel {
        out.extend(overloads.peer_files(f));
    }
    out
}

/// Restricts a target set to files whose language is one of `languages` (the
/// per-language run/skip verdict). A language whose intersection is empty and
/// unchanged is skipped.
pub fn by_language(
    files: &BTreeSet<String>,
    language_of: impl Fn(&str) -> Option<String>,
    language: &str,
) -> BTreeSet<String> {
    files
        .iter()
        .filter(|f| language_of(f).as_deref() == Some(language))
        .cloned()
        .collect()
}
