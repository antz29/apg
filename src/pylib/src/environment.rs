//! Filesystem-marker environment detection (task-4).

use std::path::Path;

/// The environment shape auto-detected from FILESYSTEM MARKERS only (task-4):
/// a uv project (`pyproject.toml`/`uv.lock`), a virtualenv (`pyvenv.cfg`), or
/// a bare src root. No interpreter probe, no `python`/`uv` shell-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectKind {
    Uv,
    VirtualEnv,
    BareSrc,
}

/// The environment shape from the marker booleans (pure seam for task-4).
pub(crate) fn classify_project_markers(
    pyproject: bool,
    uv_lock: bool,
    pyvenv: bool,
) -> ProjectKind {
    if pyproject || uv_lock {
        ProjectKind::Uv
    } else if pyvenv {
        ProjectKind::VirtualEnv
    } else {
        ProjectKind::BareSrc
    }
}

/// Filesystem-marker environment detection (task-4). No interpreter probe.
pub(crate) fn detect_project_kind(root: &Path) -> ProjectKind {
    let has_pyproject = root.join("pyproject.toml").is_file();
    let has_uv_lock = root.join("uv.lock").is_file();
    let has_pyvenv = root.join("pyvenv.cfg").is_file()
        || root.join(".venv").join("pyvenv.cfg").is_file()
        || root.join("venv").join("pyvenv.cfg").is_file();
    classify_project_markers(has_pyproject, has_uv_lock, has_pyvenv)
}
