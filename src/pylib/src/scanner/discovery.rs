//! Source-file discovery: the `.py`/`.pyi` walk with the exclusion policy.

use std::path::{Path, PathBuf};

use super::Scanner;
use crate::exclusions::is_excluded_dir_name;

impl Scanner {
    pub(crate) fn is_path_excluded(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.excludes.iter().any(|pat| s.contains(pat.as_str()))
    }

    /// Discovers every `.py`/`.pyi` file under the scan roots (the project
    /// root, or the `--module` dirs when given), skipping non-project
    /// directories and any CLI-excluded path.
    pub(crate) fn discover_files(&self) -> Vec<PathBuf> {
        let roots: Vec<PathBuf> = if self.module_dirs.is_empty() {
            vec![self.root.clone()]
        } else {
            self.module_dirs.clone()
        };
        let mut files = Vec::new();
        for r in &roots {
            self.walk_dir(r, &mut files);
        }
        files.sort();
        files.dedup();
        files
    }

    pub(crate) fn walk_dir(&self, dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_excluded_dir_name(&name) {
                continue;
            }
            if self.is_path_excluded(&path) {
                continue;
            }
            if path.is_dir() {
                self.walk_dir(&path, out);
            } else if path.extension().is_some_and(|e| e == "py" || e == "pyi") {
                out.push(path);
            }
        }
    }
}
