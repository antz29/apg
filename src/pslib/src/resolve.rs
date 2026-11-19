//! File-dependency resolution: dot-source and `Import-Module` sites to the
//! scanned file they name, or to an unresolved target.

use std::collections::HashMap;

use tree_sitter::{Node, Tree};

use crate::manifest::ModuleMap;
use crate::paths::{self, FileIndex, PathArg};
use crate::source::Decoded;

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

/// `Import-Module`'s switches and the switch-typed common parameters: they take no value.
const SWITCHES: &[&str] = &[
    "Force",
    "Global",
    "PassThru",
    "AsCustomObject",
    "DisableNameChecking",
    "NoClobber",
    "SkipEditionCheck",
    "UseWindowsPowerShell",
    "Verbose",
    "Debug",
    "WhatIf",
    "Confirm",
];

/// Every dot-source and `Import-Module` site outside ERROR regions, in source order.
pub fn file_deps(src: &Decoded, tree: &Tree) -> Vec<DepSite> {
    let mut out = Vec::new();
    walk_deps(tree.root_node(), src, &mut out);
    out
}

fn walk_deps(node: Node, src: &Decoded, out: &mut Vec<DepSite>) {
    // A site inside an ERROR node (the whole-file ERROR root included) is skipped.
    if node.is_error() {
        return;
    }
    let kind = match node.kind() {
        "invocation_operator_command" => dot_source(node, &src.text),
        "command" => import_module(node, &src.text),
        _ => None,
    };
    if let Some(kind) = kind {
        out.push(DepSite {
            kind,
            raw_offset: src.raw_offset(node.start_byte()),
        });
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_deps(child, src, out);
    }
}

/// A `.` invocation's operand, unless it is a script block (or the operator is `&`).
fn dot_source(node: Node, text: &str) -> Option<SiteKind> {
    if node.child_by_field_name("operator")?.kind() != "." {
        return None;
    }
    let operand = node.child_by_field_name("command")?;
    let block = operand.named_child_count() == 1
        && operand
            .named_child(0)
            .is_some_and(|n| n.kind() == "script_block");
    (!block).then(|| SiteKind::DotSource(paths::path_arg(operand, text)))
}

/// An `Import-Module` command's module argument, bound left to right.
fn import_module(cmd: Node, text: &str) -> Option<SiteKind> {
    let t = |n: Node| n.utf8_text(text.as_bytes()).unwrap_or_default();
    let name = cmd
        .child_by_field_name("name")
        .filter(|n| n.kind() == "command_name")?;
    let names = [
        "Import-Module",
        "ipmo",
        "Microsoft.PowerShell.Core\\Import-Module",
    ];
    if !names.iter().any(|m| m.eq_ignore_ascii_case(t(name))) {
        return None;
    }
    let (mut pending, mut bound, mut positional) = (None, Vec::new(), None);
    let mut c = cmd.walk();
    for child in cmd.named_children(&mut c) {
        match child.kind() {
            "command_parameter" => {
                let raw = child.child_by_field_name("name").map_or("", t);
                let p = raw.strip_prefix('-').unwrap_or(raw);
                let p = p.strip_suffix(':').unwrap_or(p).to_ascii_lowercase();
                pending = None;
                if let Some(v) = child.child_by_field_name("value") {
                    bound.push((p, v));
                } else if !SWITCHES.iter().any(|s| s.eq_ignore_ascii_case(&p)) {
                    pending = Some(p);
                }
            }
            "command_argument" => match pending.take() {
                Some(p) => bound.push((p, child)),
                None => {
                    positional.get_or_insert(child);
                }
            },
            _ => {}
        }
    }
    let by = |ps: &[&str]| {
        bound
            .iter()
            .find(|(p, _)| ps.contains(&p.as_str()))
            .map(|(_, v)| *v)
    };
    let arg = by(&["name"])
        .or_else(|| by(&["fullyqualifiedname"]))
        .or_else(|| by(&["moduleinfo", "assembly"]))
        .or(positional)?;
    Some(SiteKind::Import(import_arg(paths::path_arg(arg, text))))
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
