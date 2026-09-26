//! The claim taxonomy, source walk and scope filtering.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::{glob_match, StructuralScope};
use crate::paths::{absolutize, repo_relative_identity};

/// Directories never descended into: the git store, cargo/build output, the
/// package manager's tree, and the project-worktree store (a nested checkout
/// of the same repository). Generated trees (`gen`, `generated`, `dist`,
/// `build`, `out`) are deliberately NOT here: every accepted file stays in the
/// graph and is filtered by `code_type`.
///
/// `.git` and `.worktrees` also happen to be hidden, but the taxonomy does not
/// prune hidden trees wholesale — `.github/`, `.cargo/` and the config dotfiles
/// are claimed like any other authored content.
const EXCLUDED_DIRS: &[&str] = &["target", "node_modules", ".git", ".worktrees"];

/// The code-frontend extensions the structural scanner never claims: a file a
/// code frontend parses stays that frontend's. This is the UNION of every
/// shipped code frontend's accepted extensions — Rust/Go/Java/C#/Python
/// (`rs go java cs py pyi`), C++'s sources and headers
/// (`cpp cc cxx c++ h hpp hh hxx tpp ipp`), and the unified JS/TS module
/// variants (`ts tsx mts cts js jsx mjs cjs`). `.c` is deliberately absent —
/// the C++ frontend does not claim it, so a tracked `.c` file falls to the
/// residual `misc` stream.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "go", "java", "cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx", "tpp", "ipp", "ts",
    "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "cs", "py", "pyi",
];

/// Reads the pinned `--targets` list: absolute source paths, one per line,
/// blanks ignored. `None` — an absent flag, an unreadable file or an empty list
/// — means NO emission filter (the byte-identical full scan).
pub(crate) fn read_target_set(path: Option<&str>) -> Option<HashSet<PathBuf>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| absolutize(Path::new(l)))
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// The COMPLETE extension/filename → stream-id taxonomy. `None` when a code
/// frontend claims the file (its extension is in [`CODE_EXTENSIONS`]); `Some`
/// with the residual `misc` id otherwise, so every unclaimed file is claimed by
/// exactly one structural stream.
pub(crate) fn stream_for_path(path: &Path) -> Option<&'static str> {
    let name = path.file_name().and_then(|n| n.to_str())?;
    let lower = name.to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    if let Some(e) = ext.as_deref() {
        if CODE_EXTENSIONS.contains(&e) {
            return None;
        }
    }

    // Filename-keyed formats first: Dockerfile/Makefile and the extension-less
    // config dotfiles carry no (or an ambiguous) extension.
    if lower == "dockerfile" || lower.starts_with("dockerfile.") || lower.ends_with(".dockerfile") {
        return Some("dockerfile");
    }
    if lower == "makefile" || lower == "gnumakefile" || lower.ends_with(".mk") {
        return Some("makefile");
    }
    if lower == "cargo.lock" {
        return Some("toml");
    }
    if lower == "package-lock.json" {
        return Some("json");
    }
    if lower == ".editorconfig"
        || lower == ".gitconfig"
        || lower == ".env"
        || lower.starts_with(".env.")
    {
        return Some("ini");
    }

    Some(match ext.as_deref() {
        Some("md") | Some("markdown") => "md",
        Some("sh") | Some("bash") | Some("zsh") | Some("ksh") => "sh",
        Some("yaml") | Some("yml") => "yaml",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("xml") => "xml",
        Some("ini") | Some("cfg") | Some("conf") | Some("properties") | Some("env") => "ini",
        // Everything the taxonomy does not name — an unknown extension, a
        // dotfile (`.gitignore`, `.dockerignore`), a fixture or a binary — is
        // the residual `misc` stream.
        _ => "misc",
    })
}

pub(crate) fn is_pruned_dir(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name)
}

/// True when `path` carries a discovery-excluded directory component BELOW
/// `root` (so a scan root that itself lives under `.worktrees/` is still
/// scanned, while a `target/`, `node_modules/` or nested `.worktrees/` tree
/// inside it is not).
pub(crate) fn under_excluded_tree(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| match c {
        std::path::Component::Normal(s) => is_pruned_dir(&s.to_string_lossy()),
        _ => false,
    })
}

/// The claim/scope boundary: pruned trees, `--module` restriction,
/// `--exclude-path` substrings, and the `apg/config.json` structural scope
/// include/exclude globs (matched against the repo-relative identity).
pub(crate) fn in_scope(
    path: &Path,
    root: &Path,
    base: &Path,
    module_dirs: &[PathBuf],
    excludes: &[String],
    scope: &StructuralScope,
) -> bool {
    if under_excluded_tree(path, root) {
        return false;
    }
    if !module_dirs.is_empty() && !module_dirs.iter().any(|m| path.starts_with(m)) {
        return false;
    }
    let s = path.to_string_lossy();
    if excludes.iter().any(|x| s.contains(x.as_str())) {
        return false;
    }
    let rel = repo_relative_identity(base, path);
    if !scope.include.is_empty() && !scope.include.iter().any(|g| glob_match(g, &rel)) {
        return false;
    }
    if scope.exclude.iter().any(|g| glob_match(g, &rel)) {
        return false;
    }
    true
}

/// Every claimed file at or below `dir`, paired with its stream id (the
/// full-scan discovery walk). Code-frontend extensions and the pruned directory
/// trees are skipped here; `--module`/`--exclude-path`/config scope are applied
/// by the caller so the walk stays the pure taxonomy.
pub(crate) fn walk(dir: &Path, out: &mut Vec<(PathBuf, &'static str)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if is_pruned_dir(&entry.file_name().to_string_lossy()) {
                continue;
            }
            walk(&path, out);
        } else if ft.is_file() {
            if let Some(stream) = stream_for_path(&path) {
                out.push((path, stream));
            }
        }
    }
}
