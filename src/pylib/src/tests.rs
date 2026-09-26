//! Unit tests for the `pyfrontend` library (`rust.pyfrontend`).
//!
//! Relocated verbatim from `src/lib.rs`'s inline `#[cfg(test)] mod tests`:
//! the pure in-memory `unit` tier lives here and keeps the `tests::unit::…`
//! libtest path filter. Non-test helpers stay at this module's root (never
//! inside a tier); this tier needs none.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::environment::{ProjectKind, classify_project_markers};
use crate::exclusions::is_excluded_dir_name;
use crate::identity::{
    canonical_function_fqn, canonical_struct_fqn, dotted_identity, identities_from_targets,
};
use crate::unresolved::{PathOrigin, classify_unresolved};

mod unit {
    use super::*;

    #[test]
    fn dotted_identity_builds_package_and_flat_module_names() {
        let packages = ["sub".to_string(), "pkg".to_string()];
        assert_eq!(
            dotted_identity(false, "mod", &packages),
            "pkg.sub.mod",
            "a package module appends its stem to the init-bearing path"
        );
        assert_eq!(
            dotted_identity(true, "__init__", &packages),
            "pkg.sub",
            "an __init__ file contributes only the package path"
        );
        assert_eq!(
            dotted_identity(false, "foo", &[]),
            "foo",
            "a file with no init-bearing ancestor is a flat module named by its stem"
        );
        assert_eq!(
            dotted_identity(false, "bar", &[]),
            "bar",
            "a distinct-stem flat module keeps a distinct identity"
        );
        assert_eq!(
            dotted_identity(true, "__init__", &["fixture".to_string()]),
            "fixture",
            "a root-level __init__ takes its directory name"
        );
        // PEP-420 namespace components: a directory with no `__init__.py`
        // contributes its name, so namespace siblings keep distinct
        // identities instead of collapsing to the bare stem.
        assert_eq!(
            dotted_identity(false, "mod", &["pkg".to_string()]),
            "pkg.mod",
            "a namespace directory folds into the identity like a package"
        );
        assert_eq!(
            dotted_identity(false, "mod", &["other".to_string()]),
            "other.mod",
            "a sibling namespace directory yields a distinct identity"
        );
        assert_ne!(
            dotted_identity(false, "mod", &["pkg".to_string()]),
            dotted_identity(false, "mod", &["other".to_string()]),
            "same-stem modules under different namespace packages must not collapse"
        );
        assert_eq!(
            dotted_identity(false, "mod", &["ns".to_string(), "reg".to_string()]),
            "reg.ns.mod",
            "a namespace package nested under a regular package keeps both components"
        );
    }

    #[test]
    fn excluded_dir_names_cover_venv_cache_and_egg_info() {
        for name in [
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
            ".git",
            ".hg",
            "pkg.egg-info",
        ] {
            assert!(is_excluded_dir_name(name), "{name} must be excluded");
        }
        for name in ["src", "pkg", "tests", "app", "my_package"] {
            assert!(!is_excluded_dir_name(name), "{name} must not be excluded");
        }
    }

    #[test]
    fn project_kind_prefers_uv_then_virtualenv_then_bare() {
        assert_eq!(
            classify_project_markers(true, false, false),
            ProjectKind::Uv
        );
        assert_eq!(
            classify_project_markers(false, true, false),
            ProjectKind::Uv
        );
        assert_eq!(
            classify_project_markers(false, false, true),
            ProjectKind::VirtualEnv
        );
        assert_eq!(
            classify_project_markers(false, false, false),
            ProjectKind::BareSrc
        );
    }

    #[test]
    fn unresolved_category_precedence_holds() {
        let root = Path::new("/proj");
        assert_eq!(
            classify_unresolved(
                PathOrigin::Vendored,
                Path::new("typeshed/stdlib/os.pyi"),
                root
            ),
            "stdlib",
            "bundled typeshed is stdlib"
        );
        assert_eq!(
            classify_unresolved(
                PathOrigin::System,
                Path::new("/proj/.venv/lib/site-packages/requests/__init__.py"),
                root
            ),
            "external",
            "a venv site-packages tree is dependency code"
        );
        assert_eq!(
            classify_unresolved(
                PathOrigin::System,
                Path::new("/proj/vendor/foo/bar.py"),
                root
            ),
            "external",
            "a project vendor tree is dependency code"
        );
        assert_eq!(
            classify_unresolved(
                PathOrigin::System,
                Path::new("/usr/lib/python3.12/os.py"),
                root
            ),
            "external",
            "an installed environment outside the root is external"
        );
        assert_eq!(
            classify_unresolved(PathOrigin::System, Path::new("/proj/src/dynamic.py"), root),
            "unknown",
            "in-root unbound/dynamic residue is unknown"
        );
        assert_eq!(
            classify_unresolved(
                PathOrigin::SystemVirtual,
                Path::new("typeshed/stdlib/builtins.pyi"),
                root
            ),
            "external",
            "a virtual installed environment is external"
        );
    }

    #[test]
    fn overloaded_functions_get_the_erased_param_suffix() {
        assert_eq!(
            canonical_function_fqn("pkg.mod.Foo", "bar", &["int".to_string()], true),
            "pkg.mod.Foo.bar(int)"
        );
        assert_eq!(
            canonical_function_fqn("pkg.mod", "bar", &[], true),
            "pkg.mod.bar()",
            "the empty erased param list keeps its () form"
        );
        assert_eq!(
            canonical_function_fqn("pkg.mod", "bar", &["int".to_string()], false),
            "pkg.mod.bar",
            "a singleton keeps the bare parent.name"
        );
        assert_eq!(canonical_struct_fqn("pkg.mod", "Foo"), "pkg.mod.Foo");
    }

    #[test]
    fn target_paths_map_to_module_identities() {
        let targets = vec![
            PathBuf::from("/proj/pkg/__init__.py"),
            PathBuf::from("/proj/pkg/mod.py"),
        ];
        let identities = identities_from_targets(&targets, |p| {
            if p.ends_with("__init__.py") {
                "pkg".to_string()
            } else {
                "pkg.mod".to_string()
            }
        });
        let expected: HashSet<String> = ["pkg".to_string(), "pkg.mod".to_string()]
            .into_iter()
            .collect();
        assert_eq!(identities, expected);
    }
}
