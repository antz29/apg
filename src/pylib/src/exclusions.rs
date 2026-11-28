//! The frontend's own discovery-walk exclusion policy.

/// Directory names never descended into by the frontend's own discovery walk
/// (`domain.constraint.python-exclusions`). Hidden dot entries
/// (`.venv`, `.tox`, `.git`, `.hg`, `.mypy_cache`, `.pytest_cache`,
/// `.ruff_cache`, `.eggs`, …) are covered by the general hidden-name rule;
/// `*.egg-info` by the suffix rule. The same trees are still ty resolution
/// inputs — this is a discovery exclusion, not a resolution one.
pub(crate) const EXCLUDED_DIR_NAMES: &[&str] = &[
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "site-packages",
    ".eggs",
    "node_modules",
];

/// A parse/discovery decision: is the frontend's own walk allowed into `name`?
pub(crate) fn is_excluded_dir_name(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIR_NAMES.contains(&name) || name.ends_with(".egg-info")
}
