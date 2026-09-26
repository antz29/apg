//! Repository-base discovery and repo-relative identity rendering.

use std::path::{Path, PathBuf};

/// Lexical-ish absolute path: `canonicalize` when it exists (so a symlinked
/// scan root still matches the target list), else the path made absolute.
pub(crate) fn absolutize(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// The repository base: the nearest ancestor of `root` carrying a `.git` entry
/// (a directory in a primary checkout, a file in a linked worktree), else
/// `root` itself (a non-git scan root). Every emitted module identity is
/// rendered relative to this base.
pub(crate) fn repo_base(root: &Path) -> PathBuf {
    let mut cur = root;
    loop {
        if cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return root.to_path_buf(),
        }
    }
}

/// `path`'s identity relative to `base`: its `/`-joined normal components, with
/// no leading separator. `None` when `path` is not under `base`.
fn relative_join(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Some(parts.join("/"))
}

/// The emitted module identity of `path`'s directory: repo-relative to `base`,
/// EMPTY at the repository base itself.
pub(crate) fn repo_relative_dir(base: &Path, path: &Path) -> String {
    relative_join(base, path.parent().unwrap_or(path)).unwrap_or_default()
}

/// The emitted identity of `path` itself (file name included), repo-relative to
/// `base`; falls back to the bare file name for a foreign/escaping path so an
/// identity never embeds a checkout component.
pub(crate) fn repo_relative_identity(base: &Path, path: &Path) -> String {
    relative_join(base, path).unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}
