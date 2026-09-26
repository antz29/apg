//! The scan-hygiene predicate `is_blacklisted`: the user blacklist, the
//! default build-output trees, the containing checkout's gitignore, and the
//! structural config scope.

use std::path::Path;

use super::IngestOptions;
use super::identity::structural_scope_excludes;

/// The scan-hygiene predicate: a record is out of scope when
///
/// - its canonical FQN carries a user blacklist prefix, or
/// - its repo-relative source path (or identity) sits under a default
///   build-output tree — `.git/**` or `target/**`, via the shared
///   [`crate::classify::is_build_output_path`] predicate, or
/// - that path is gitignored by the containing checkout: the scan's content
///   identity already excludes ignored content, so keeping it would make the
///   graph a function of checkout state rather than of authored content.
///
/// A record on a STRUCTURAL stream (see
/// [`crate::classify::is_structural_language`]) is additionally dropped when the
/// config scope excludes it, so the scanner's claim walk and this blacklist
/// agree exactly on `target`/`.git`/gitignored/config-excluded paths and a
/// structural record is never silently kept or lost. Code streams are
/// unaffected.
///
/// `path` is `None` for records that carry no source location (modules) and
/// for edge endpoints, whose FQN prefix is still honoured. A **structural
/// module** identity is a repo-relative tree in disguise (`misc.apg/.trans`):
/// the scanner's walk prunes only `target`/`node_modules`/`.git`/`.worktrees`
/// and never consults `.gitignore`, so a path-less structural module whose
/// identity sits under a gitignored/build-output tree is dropped here exactly
/// as its File records are — otherwise an empty `module:misc.<ignored-tree>`
/// would survive. The check is scoped to an fqn actually rooted under the
/// current structural language id, so a code FQN or an opaque id is never
/// re-interpreted as a path. An edge into a dropped record dangles and is
/// pruned by the final cleanup.
pub(crate) fn is_blacklisted(
    fqn: &str,
    path: Option<&str>,
    language: &str,
    opts: &IngestOptions,
) -> bool {
    if opts.blacklist.iter().any(|p| fqn.starts_with(p.as_str())) {
        return true;
    }
    let Some(path) = path else {
        // The bare `<language>.` root (empty identity) is the repo root: it is
        // never build-output/gitignored, so it always survives.
        if crate::classify::is_structural_language(language)
            && let Some(identity) = fqn
                .strip_prefix(language)
                .and_then(|rest| rest.strip_prefix('.'))
        {
            if crate::classify::is_build_output_path(identity) {
                return true;
            }
            if opts
                .base
                .is_some_and(|base| crate::git::path_is_ignored(base, Path::new(identity)))
            {
                return true;
            }
        }
        return false;
    };
    if crate::classify::is_build_output_path(path) {
        return true;
    }
    if crate::classify::is_structural_language(language)
        && structural_scope_excludes(path, opts.config)
    {
        return true;
    }
    opts.base
        .is_some_and(|base| crate::git::path_is_ignored(base, Path::new(path)))
}
