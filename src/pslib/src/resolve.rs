//! File-dependency resolution: dot-source and `Import-Module` sites to the
//! scanned file they name, or to an unresolved target.

use std::collections::HashMap;

use crate::manifest::ModuleMap;
use crate::paths::{self, FileIndex, PathArg};

/// The kind a file dependency's uses edge carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    DotSource,
    ImportModule,
}

impl DepKind {
    /// The uses edge's `properties` kind.
    pub fn as_str(self) -> &'static str {
        match self {
            DepKind::DotSource => "dot-source",
            DepKind::ImportModule => "import-module",
        }
    }
}

/// An `Import-Module` argument, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportArg {
    Path(PathArg),
    Name(String),
    Computed,
}

/// Classifies an `Import-Module` argument as a path, a module name or computed.
pub fn import_arg(arg: PathArg) -> ImportArg {
    let PathArg::Static { text, anchored } = &arg else {
        return ImportArg::Computed;
    };
    let lower = text.to_ascii_lowercase();
    let is_path = *anchored
        || text.contains(['\\', '/'])
        || text.starts_with('.')
        || [".psm1", ".psd1", ".ps1", ".dll"]
            .iter()
            .any(|e| lower.ends_with(e));
    if is_path {
        ImportArg::Path(arg)
    } else {
        ImportArg::Name(text.clone())
    }
}

/// What a dependency site names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteKind {
    DotSource(PathArg),
    Import(ImportArg),
}

/// A dependency site and its raw on-disk byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSite {
    pub kind: SiteKind,
    pub raw_offset: usize,
}

/// The outcome of matching a module name against the manifest stems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StemMatch {
    /// The single manifest's repo-relative `.psd1` path.
    One(String),
    Many,
    None,
}

/// The recognised manifests, indexed by lowercased `.psd1` file stem.
pub struct ManifestStems {
    by_stem: HashMap<String, Vec<String>>,
}

impl ManifestStems {
    pub fn new(map: &ModuleMap) -> Self {
        let mut by_stem: HashMap<String, Vec<String>> = HashMap::new();
        for id in map.modules() {
            // Only a manifest module's identity is its own manifest's path.
            if map.manifest_of(&id).is_some_and(|m| m.path == id) {
                let name = id.rsplit('/').next().unwrap_or(&id);
                let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
                by_stem.entry(stem.to_lowercase()).or_default().push(id);
            }
        }
        ManifestStems { by_stem }
    }

    pub fn find(&self, name: &str) -> StemMatch {
        match self.by_stem.get(&name.to_lowercase()).map(Vec::as_slice) {
            Some([only]) => StemMatch::One(only.clone()),
            Some([_, _, ..]) => StemMatch::Many,
            _ => StemMatch::None,
        }
    }
}

/// The PowerShell 7.6 LTS in-box modules named `Microsoft.PowerShell.*`,
/// transcribed from `apg/.scratch/ps7-builtin-cmdlets.md` (PowerShell source
/// tag v7.6.0). This is the crate's single in-box module list: the builtins
/// command table names its modules from it and never redefines it.
pub const INBOX_MODULES: &[&str] = &[
    "Microsoft.PowerShell.Core",
    "Microsoft.PowerShell.Management",
    "Microsoft.PowerShell.Utility",
    "Microsoft.PowerShell.Security",
    "Microsoft.PowerShell.Host",
    "Microsoft.PowerShell.Diagnostics",
    "Microsoft.PowerShell.Archive",
    "Microsoft.PowerShell.PSResourceGet",
    "Microsoft.PowerShell.ThreadJob",
];

/// `stdlib` for an in-box module (whole-name, case-insensitive), else `external`.
pub fn module_category(name: &str) -> &'static str {
    if INBOX_MODULES.iter().any(|m| m.eq_ignore_ascii_case(name)) {
        "stdlib"
    } else {
        "external"
    }
}

/// The site-keyed sentinel naming a dependency with no static target.
pub fn nameless_key(path: &str, raw_offset: usize) -> String {
    format!("<nameless@{path}:{raw_offset}>")
}

/// A resolved dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dep {
    File {
        path: String,
        kind: DepKind,
    },
    Unresolved {
        name: String,
        category: &'static str,
    },
}

/// Resolves a site in `file` (repo-relative) to its dependency.
pub fn resolve_site(site: &DepSite, file: &str, index: &FileIndex, stems: &ManifestStems) -> Dep {
    let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
    let find = |arg: &PathArg| match arg {
        PathArg::Static { text, .. } => paths::resolve(dir, text).and_then(|p| index.find(&p)),
        PathArg::Computed => None,
    };
    let dynamic = || Dep::Unresolved {
        name: nameless_key(file, site.raw_offset),
        category: "unknown",
    };
    let file_dep = |path: &str, kind| Dep::File {
        path: path.to_string(),
        kind,
    };
    match &site.kind {
        SiteKind::DotSource(arg) => {
            find(arg).map_or_else(dynamic, |p| file_dep(p, DepKind::DotSource))
        }
        SiteKind::Import(ImportArg::Path(arg)) => find(arg)
            .filter(|p| {
                let l = p.to_ascii_lowercase();
                l.ends_with(".psm1") || l.ends_with(".psd1")
            })
            .map_or_else(dynamic, |p| file_dep(p, DepKind::ImportModule)),
        SiteKind::Import(ImportArg::Computed) => dynamic(),
        SiteKind::Import(ImportArg::Name(n)) => match stems.find(n) {
            StemMatch::One(p) => file_dep(&p, DepKind::ImportModule),
            StemMatch::Many => Dep::Unresolved {
                name: n.clone(),
                category: "unknown",
            },
            StemMatch::None => Dep::Unresolved {
                name: n.clone(),
                category: module_category(n),
            },
        },
    }
}
