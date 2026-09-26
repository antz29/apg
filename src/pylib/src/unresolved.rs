//! Unresolved-target origin classification (task-6).

use std::path::{Component, Path, PathBuf};

use ruff_db::files::{File, FilePath};
use ty_project::ProjectDatabase;

/// Dependency-environment directory names (task-6): a resolved reference
/// landing under one of these is dependency code → `external`, never
/// `unknown`. `*.egg-info` is matched by suffix.
pub(crate) const DEPENDENCY_DIR_NAMES: &[&str] = &[
    "vendor",
    "third_party",
    "thirdparty",
    "site-packages",
    ".venv",
    "venv",
    ".tox",
    ".eggs",
    "node_modules",
    "__pycache__",
];

/// Which kind of file a `ty`-resolved reference landed in. Drives the
/// unresolved category (task-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathOrigin {
    /// The tool-bundled vendored typeshed → `stdlib`.
    Vendored,
    /// A real file on disk → `external`/`unknown` by location.
    System,
    /// A virtual installed-environment file → `external`.
    SystemVirtual,
}

/// True when any path component names a dependency/installed-environment tree.
pub(crate) fn under_dependency_tree(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(s) => {
            let s = s.to_string_lossy();
            DEPENDENCY_DIR_NAMES.contains(&s.as_ref()) || s.ends_with(".egg-info")
        }
        _ => false,
    })
}

/// The unresolved category for a `ty`-resolved reference outside our own
/// declaration set (task-6, first match wins). `Vendored` (bundled typeshed)
/// is `stdlib`; a virtual installed environment is `external`; a real file
/// under a dependency tree (project `vendor`/`third_party` or a `.venv`
/// site-packages) is `external`; elsewhere under the scan root is in-root
/// unbound/dynamic residue → `unknown`; anywhere else unambiguously external.
pub(crate) fn classify_unresolved(origin: PathOrigin, path: &Path, root: &Path) -> &'static str {
    match origin {
        PathOrigin::Vendored => "stdlib",
        PathOrigin::SystemVirtual => "external",
        PathOrigin::System => {
            if under_dependency_tree(path) {
                "external"
            } else if path.starts_with(root) {
                "unknown"
            } else {
                "external"
            }
        }
    }
}

/// The `ty` path origin plus a plain `Path`, for category classification.
pub(crate) fn file_origin(db: &ProjectDatabase, file: File) -> (PathOrigin, PathBuf) {
    match file.path(db) {
        FilePath::Vendored(p) => (PathOrigin::Vendored, PathBuf::from(p.as_str())),
        FilePath::System(p) => (PathOrigin::System, p.as_std_path().to_path_buf()),
        FilePath::SystemVirtual(p) => (PathOrigin::SystemVirtual, PathBuf::from(p.as_str())),
    }
}

/// Builds a stable "unresolved" FQN for a `ty`-resolved definition that falls
/// outside our own declaration set (stdlib, third-party, or otherwise outside
/// the project root).
pub(crate) fn unresolved_name_for(db: &ProjectDatabase, file: File, root: &Path) -> String {
    match file.path(db) {
        FilePath::Vendored(p) => p.as_str().to_string(),
        FilePath::System(p) => {
            let std_path = p.as_std_path();
            std_path
                .strip_prefix(root)
                .unwrap_or(std_path)
                .to_string_lossy()
                .into_owned()
        }
        FilePath::SystemVirtual(p) => p.as_str().to_string(),
    }
}
