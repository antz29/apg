//! Cargo-manifest / local-crate-root source discovery and target-set reading.

use std::collections::HashSet;

use hir::Crate;
use ide_db::RootDatabase;
use vfs::Vfs;

use crate::source::path_of_vfs;

/// Directory names never descended into by the all-manifest discovery walk
/// (phase-05 task-3 PART 1): a `Cargo.toml` inside any of them is never a scan
/// root. Matched on a whole path component, so a file named `target.rs` or a
/// directory named `targets` is unaffected.
pub(crate) const DISCOVERY_EXCLUDED_DIRS: &[&str] =
    &["target", "vendor", "node_modules", ".worktrees"];

pub(crate) fn is_discovery_excluded_dir(name: &str) -> bool {
    DISCOVERY_EXCLUDED_DIRS.contains(&name)
}

/// True when `path` carries a discovery-excluded directory component BELOW
/// `root`. The exclusion is relative to the scan root, so a project checked out
/// under a `.worktrees/` directory (the apg project flow) is not itself
/// excluded, while a `target/`, `vendor/`, `node_modules/` or nested
/// `.worktrees/` tree inside it is (phase-05 task-3 PART 2).
pub(crate) fn under_excluded_tree(path: &str, root: &std::path::Path) -> bool {
    let p = std::path::Path::new(path);
    let rel = p.strip_prefix(root).unwrap_or(p);
    rel.components().any(|c| match c {
        std::path::Component::Normal(s) => s.to_str().is_some_and(is_discovery_excluded_dir),
        _ => false,
    })
}

/// Every directory at or below `root` that holds a `Cargo.toml`, with the
/// generated/dependency/other-tree directories pruned (phase-05 task-1/-3).
/// Nested non-workspace crates are found; members of a discovered workspace are
/// found too and deduped by the load loop in [`crate::scanner::run`].
pub(crate) fn discover_manifest_dirs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join("Cargo.toml").is_file() {
            out.push(dir.clone());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // `.git` is pruned too — it can never hold a manifest and walking it
            // is pure overhead; it is not part of the task's exclusion set.
            if is_discovery_excluded_dir(&name) || name == ".git" {
                continue;
            }
            stack.push(entry.path());
        }
    }
    // Shallow-first: a workspace root is loaded before its members, so the
    // members are then covered (and skipped) by the workspace that loaded them.
    out.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    out.dedup();
    out
}

/// The root files of every local crate the loaded workspace resolved. Used to
/// dedupe discovered projects by manifest root: a candidate directory a
/// previously loaded workspace already covers is never re-loaded as a top-level
/// project (phase-05 task-2).
pub(crate) fn local_crate_root_files(db: &RootDatabase, vfs: &Vfs) -> Vec<std::path::PathBuf> {
    Crate::all(db)
        .into_iter()
        .filter(|k| k.origin(db).is_local())
        .map(|k| path_of_vfs(vfs, k.root_file(db)))
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// The absolute target set read from the pinned `--targets <file>` hand-off
/// (phase-02 task-9). An absent flag, a missing/unreadable file, or an empty
/// file yields `None` — "no emission filter", the byte-identical full scan.
pub(crate) fn read_target_set(path: Option<&str>) -> Option<HashSet<std::path::PathBuf>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<std::path::PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p = std::path::PathBuf::from(l);
            // The VFS exposes canonical paths; normalize the target list the
            // same way so a symlinked scan root still matches.
            std::fs::canonicalize(&p).unwrap_or(p)
        })
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// The pinned per-language native-artifact location for the Rust frontend,
/// `<cache-dir>/rust/<cache-key>/` (phase-02 task-9 NATIVE-ARTIFACT RULE).
/// rust-analyzer resolves through an in-process salsa database, so the
/// directory carries no separate on-disk compiler cache; creating it makes the
/// shared store's Rust location exist and keeps it keyed by the global cache
/// key, so a key drift lands in a fresh directory.
pub(crate) fn ensure_artifact_dir(cache_dir: Option<&str>, cache_key: Option<&str>) {
    let Some(dir) = cache_dir else { return };
    let mut p = std::path::PathBuf::from(dir);
    p.push("rust");
    if let Some(k) = cache_key {
        p.push(k);
    }
    if let Err(e) = std::fs::create_dir_all(&p) {
        eprintln!(
            "warning: could not create rust artifact dir {}: {e}",
            p.display()
        );
    }
}
