//! Scanner-internal module identity computation.

use std::path::Path;

use super::Scanner;
use crate::identity::dotted_identity;

impl Scanner {
    /// Computes the dotted module identity for a source file from its full
    /// package directory path, relative to the REPO BASE (the git toplevel
    /// found by walking up from the scan root, or the scan root when the tree
    /// is not a git checkout): every directory between the base and the file
    /// is a package-or-PEP-420-namespace component (a directory with no
    /// `__init__.py` is a namespace package and contributes its name just like
    /// an `__init__.py`/`__init__.pyi`-bearing regular package). The base is
    /// the import root and never contributes its own name, so no module is
    /// named after the checkout/scan-root directory — and because the boundary
    /// is the repo base, `<repo>` and `<repo>/subdir` scans agree. A file
    /// outside the repo base keeps the pre-namespace rule (only
    /// `__init__`-bearing ancestors).
    pub(crate) fn module_fqn_for(&self, file: &Path) -> String {
        let is_init = file.file_stem().is_some_and(|s| s == "__init__");
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "module".to_string());
        // The identity boundary: walk up from the scan root for a `.git` entry
        // (the git toplevel), falling back to the scan root when no ancestor is
        // a checkout. Inline block — no new unit. The same repo-base model the
        // TS frontend uses (`ts.apg-tsfrontend.identity.packageIdentity`), so
        // the boundary is never the scan root.
        let repo_base = {
            let mut dir = self.root.clone();
            loop {
                if dir.join(".git").exists() {
                    break dir;
                }
                match dir.parent() {
                    Some(parent) => dir = parent.to_path_buf(),
                    None => break self.root.clone(),
                }
            }
        };
        let under_base = file.starts_with(&repo_base);
        let mut ancestors: Vec<String> = Vec::new();
        let mut dir = file.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            if under_base {
                // Directories strictly below the repo base all contribute;
                // the base itself is the boundary (exclusive).
                if d == repo_base {
                    break;
                }
            } else {
                // Outside the repo base: preserve the pre-namespace boundary.
                let has_init = d.join("__init__.py").is_file() || d.join("__init__.pyi").is_file();
                if !has_init {
                    break;
                }
            }
            if let Some(name) = d.file_name() {
                ancestors.push(name.to_string_lossy().into_owned());
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        dotted_identity(is_init, &stem, &ancestors)
    }
}
