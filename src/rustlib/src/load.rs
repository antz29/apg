//! Workspace loading and the per-project loader context.

use std::collections::HashMap;

use anyhow::Result;
use hir::Semantics;
use ide_db::FxHashMap;
use ide_db::RootDatabase;
use load_cargo::{load_workspace, LoadCargoConfig, ProcMacroServerChoice};
use project_model::{
    CargoConfig, CargoFeatures, CargoWorkspace, ProjectManifest, ProjectWorkspace,
    ProjectWorkspaceKind, RustLibSource,
};
use vfs::{AbsPathBuf, Vfs};

pub(crate) struct Ctx<'db> {
    pub(crate) db: &'db RootDatabase,
    pub(crate) sema: Semantics<'db, RootDatabase>,
    pub(crate) vfs: &'db Vfs,
    /// Cargo package name keyed by the crate-root file's absolute path, for
    /// every target of every package the loaded project resolved (phase-05
    /// task-10). Consulted by [`crate::identity::package_prefix_map`] to group
    /// local crates by package and to prefer the package identity over the
    /// target/display name.
    pub(crate) package_by_root: HashMap<String, String>,
    /// Resolved prefix override by crate-root absolute path: the cargo package
    /// name for a single-target package, and the distinct `<pkg>-bin` prefix for
    /// the bin of a colliding lib + bin package (libbin-fix). Populated per
    /// loaded project by [`crate::scanner::scan`]; an absent entry means "use the
    /// display-name fallback".
    pub(crate) package_prefix: HashMap<String, String>,
}

pub(crate) fn load_project(
    root: &std::path::Path,
    no_build_scripts: bool,
) -> Result<(RootDatabase, Vfs, HashMap<String, String>)> {
    let progress = |_msg: String| {};
    let abs = AbsPathBuf::assert_utf8(root.to_path_buf());
    let manifest = ProjectManifest::discover_single(&abs)?;

    let mut cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: true,
        set_test: true,
        features: CargoFeatures::All,
        ..Default::default()
    };

    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: !no_build_scripts,
        with_proc_macro_server: if no_build_scripts {
            ProcMacroServerChoice::None
        } else {
            ProcMacroServerChoice::Sysroot
        },
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 1,
    };

    let mut ws = match ProjectWorkspace::load(manifest, &cargo_config, &progress) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("warning: workspace load failed ({e}); retrying without sysroot");
            cargo_config.sysroot = None;
            let manifest = ProjectManifest::discover_single(&abs)?;
            ProjectWorkspace::load(manifest, &cargo_config, &progress)?
        }
    };

    let ws = if no_build_scripts {
        ws
    } else {
        match ws.run_build_scripts(&cargo_config, &progress) {
            Ok(bs) => {
                if let Some(err) = bs.error() {
                    eprintln!("warning: build scripts had errors: {err}");
                }
                ws.set_build_scripts(bs);
                ws
            }
            Err(e) => {
                eprintln!("warning: build scripts failed ({e}); continuing source-only");
                ws
            }
        }
    };

    // The cargo PACKAGE identity per crate-root file, retained from the loaded
    // project model before it is consumed by `load_workspace` (phase-05
    // task-10). The target/display name rust-analyzer derives can differ from
    // the package name (`[[bin]] name` vs `[package] name`), and the module
    // prefix must carry the package identity.
    let package_by_root = match &ws.kind {
        ProjectWorkspaceKind::Cargo { cargo, .. } => package_roots(cargo),
        _ => HashMap::new(),
    };

    let extra_env: FxHashMap<String, Option<String>> = FxHashMap::default();
    let (db, vfs, _) = load_workspace(ws, &extra_env, &load_config)?;
    Ok((db, vfs, package_by_root))
}

/// Cargo package name by the crate-root file's absolute path, for every target
/// of every package the loaded workspace resolved (phase-05 task-10). The map
/// also covers dependency packages; only the local crates' roots are ever
/// consulted (see [`crate::identity::package_prefix_map`]).
pub(crate) fn package_roots(cargo: &CargoWorkspace) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for pkg in cargo.packages() {
        let data = &cargo[pkg];
        for target in &data.targets {
            out.insert(cargo[*target].root.to_string(), data.name.clone());
        }
    }
    out
}
