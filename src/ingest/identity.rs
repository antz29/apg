//! Checkout-independent identity rendering: repo-relative paths, language
//! rooting of modules and scopes, and the endpoint rooters the record stream
//! uses.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::classify::ApgConfig;

/// True when the `apg/config.json` structural scope EXCLUDES the repo-relative
/// `identity`: a non-empty `include` list requires a match, and any `exclude`
/// match wins — the same rule the structural scanner's `in_scope` applies to
/// its walk. An absent scope (or no config) is the default ON: nothing is
/// excluded.
pub(crate) fn structural_scope_excludes(identity: &str, config: Option<&ApgConfig>) -> bool {
    let Some(scope) = config.and_then(|c| c.structural.as_ref()) else {
        return false;
    };
    if !scope.include.is_empty()
        && !scope
            .include
            .iter()
            .any(|g| crate::classify::matches_glob(g, identity))
    {
        return true;
    }
    scope
        .exclude
        .iter()
        .any(|g| crate::classify::matches_glob(g, identity))
}

pub(crate) fn file_basename(file: &str) -> String {
    std::path::Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string())
}

/// Renders a scanner path as its checkout-independent identity relative to
/// `base` (the git toplevel, or the scan root when the scanned tree is not a
/// git repository): a `/`-separated path with no leading separator, no `.`
/// component and no `..` segment. An already-relative identity (a frontend's
/// dotted package identity, e.g. `pkg.sub` or `@co/ui.src`) passes through
/// unchanged; an absolute path under `base` is stripped to its tail.
///
/// A path that resolves to `base` **itself** — the repository root, whose
/// repo-relative identity is empty — renders the empty string, NOT the
/// `file_name(base)` checkout basename: a module rooted at the repo root is
/// the bare `<language>.` root (`md.`), never a checkout-named module
/// (`md.apg`) (`requirements.constraint.no-checkout-named-module`).
///
/// A path outside `base`, or one whose tail would escape `base` with `..`,
/// falls back to its file name so an identity can never embed a checkout
/// component (a leading `/` or a `..` segment is a violation —
/// `requirements.constraint.file-identity-is-repo-relative`).
///
/// An empty `base` is the pure pass-through sentinel the in-memory renderer
/// tests use: the input is returned verbatim.
pub fn repo_relative_identity(base: &Path, path: &str) -> String {
    if base.as_os_str().is_empty() {
        return path.to_string();
    }
    let fallback = || -> String {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if name.is_empty() {
            path.replace('\\', "/").trim_start_matches('/').to_string()
        } else {
            name
        }
    };
    let p = Path::new(path);
    let candidate = if p.is_absolute() {
        match p.strip_prefix(base) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => return fallback(),
        }
    } else {
        p.to_path_buf()
    };
    let mut out: Vec<String> = Vec::new();
    for comp in candidate.components() {
        match comp {
            std::path::Component::Normal(s) => out.push(s.to_string_lossy().replace('\\', "/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if out.pop().is_none() {
                    return fallback();
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
        }
    }
    if out.is_empty() {
        // The path resolves to the base itself (or an empty identity): the
        // repo-root identity is empty. Every genuinely escaping/foreign path
        // returned `fallback()` above, so only the under-base/empty case lands
        // here.
        return String::new();
    }
    out.join("/")
}

/// Renders a module's canonical FQN by rooting its frontend-emitted dotted
/// identity under the `lang_switch` language id (PHASE_09 language rooting)
/// after rendering the identity repo-relative against `base` (a Markdown
/// module identity is an absolute directory path; every other frontend emits a
/// relative identity, which passes through unchanged):
///
/// ```text
/// root_module_fqn("rust", "apg.ingest", base) -> "rust.apg.ingest"
/// root_module_fqn("py",   "pkg.sub",    base) -> "py.pkg.sub"
/// root_module_fqn("ts",   "@co/ui.src", base) -> "ts.@co/ui.src"
/// root_module_fqn("md",   "/abs/docs",  base) -> "md.docs"
/// root_module_fqn("md",   "",           base) -> "md."     // the repo root
/// root_module_fqn("sh",   "",           base) -> "sh."     // the repo root
/// ```
///
/// The bundled structural scanner emits each module identity repo-relative to
/// the repository base — EMPTY at the repo root. Rooting an empty identity
/// therefore renders the bare `<language>.` root (`md.`, `sh.`, …), which is
/// the repo-root module, never a checkout-named module (`md.apg`); this is the
/// ingestor half of the `md.apg` → `md.` relocation (the emitter half is the
/// scanner's `repo_relative_dir`). Every non-root structural identity roots
/// under its stream id exactly like a code module (`md.docs`, `yaml.ci`).
///
/// The frontends emit the module identity VERBATIM and UNROOTED (a
/// frontend-baked root would double-root once the ingestor applies it); the
/// ingestor applies the `lang_switch` id here. Rooting is what makes two
/// languages that both define a module identity `apg` distinct (`rust.apg` vs
/// `py.apg`), so a cross-language module-module FQN collision is impossible by
/// construction. Every declaration's scope parent begins with a module identity,
/// so `parent.name` inherits the root without a second transform.
pub fn root_module_fqn(language: &str, identity: &str, base: &Path) -> String {
    format!("{language}.{}", repo_relative_identity(base, identity))
}

/// Applies [`root_module_fqn`] to a declaration's scope parent (or a module
/// identity), leaving an EMPTY parent empty — a declaration with no scope is a
/// scanner anomaly and must not become the bare `<language>.` root.
pub(crate) fn rooted_scope(language: &str, parent: &str, base: &Path) -> String {
    if parent.is_empty() {
        String::new()
    } else {
        root_module_fqn(language, parent, base)
    }
}

/// Roots a `contains` endpoint iff it is a known module identity (the frontends
/// emit `Module -> Module` containment with the raw identity endpoints, while
/// declaration containment uses opaque ids this must never rewrite). Module
/// records for both endpoints are emitted before their containment edge (the
/// frontends emit global module scaffolding first), so membership is exact.
pub(crate) fn root_module_endpoint(
    language: &str,
    endpoint: &str,
    module_identities: &HashMap<String, HashSet<String>>,
    base: &Path,
) -> String {
    match module_identities.get(language) {
        Some(ids) if ids.contains(endpoint) => root_module_fqn(language, endpoint, base),
        _ => endpoint.to_string(),
    }
}

/// Roots a scanner-fact edge endpoint that names a **project symbol**, not a
/// module: a cross-stream (`--targets`) edge carries the target's canonical FQN
/// instead of an opaque id (phase-02 task-9), and the frontend emits that FQN
/// UNROOTED. The endpoint is rooted by its longest module-identity prefix
/// (walked right-to-left over `.`/`/` boundaries, so a nested module wins over
/// its parent); an opaque id or a foreign name has no such prefix and is left
/// unchanged. Only the CURRENT stream's language identities are consulted, so
/// two languages that both define `apg` can never cross.
pub(crate) fn root_edge_endpoint(
    language: &str,
    endpoint: &str,
    module_identities: &HashMap<String, HashSet<String>>,
    base: &Path,
) -> String {
    let Some(identities) = module_identities.get(language) else {
        return endpoint.to_string();
    };
    if identities.contains(endpoint) {
        return root_module_fqn(language, endpoint, base);
    }
    let bytes = endpoint.as_bytes();
    let mut sep = bytes.len();
    while sep > 0 {
        sep -= 1;
        if (bytes[sep] == b'.' || bytes[sep] == b'/')
            && sep > 0
            && identities.contains(&endpoint[..sep])
        {
            return root_module_fqn(language, endpoint, base);
        }
    }
    endpoint.to_string()
}
