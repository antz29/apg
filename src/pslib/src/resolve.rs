//! File-dependency resolution: dot-source and `Import-Module` sites to the
//! scanned file they name, or to an unresolved target.

use std::collections::{BTreeMap, HashMap};

use tree_sitter::{Node, Tree};

use crate::builtins;
use crate::manifest::ModuleMap;
use crate::paths::{self, FileIndex, PathArg};
use crate::source::Decoded;
use crate::units::{self, FileUnits, UnitKind};

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
    (!is_script_block(operand)).then(|| SiteKind::DotSource(paths::path_arg(operand, text)))
}

/// Whether an invocation operator's operand is a single script block.
fn is_script_block(operand: Node) -> bool {
    operand.named_child_count() == 1
        && operand
            .named_child(0)
            .is_some_and(|n| n.kind() == "script_block")
}

/// The command names that import a module (case-insensitive).
const IMPORT_NAMES: &[&str] = &[
    "Import-Module",
    "ipmo",
    "Microsoft.PowerShell.Core\\Import-Module",
];

/// An `Import-Module` command's module argument, bound left to right.
fn import_module(cmd: Node, text: &str) -> Option<SiteKind> {
    let t = |n: Node| n.utf8_text(text.as_bytes()).unwrap_or_default();
    let name = cmd
        .child_by_field_name("name")
        .filter(|n| n.kind() == "command_name")?;
    if !IMPORT_NAMES.iter().any(|m| m.eq_ignore_ascii_case(t(name))) {
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

/// The PowerShell 7.6 LTS in-box modules named `Microsoft.PowerShell.*`, from
/// the PowerShell v7.6.0 sources: the `src/Modules/{Windows,Unix,Shared}`
/// module manifests, `PSGalleryModules.csproj` and `InitialSessionState.cs`.
/// This is the crate's single in-box module list: the builtins command table
/// names its modules from it and never redefines it.
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

/// A script's repo-relative path to its dot-sourced Files, in first-site order.
pub type DotSourceMap = BTreeMap<String, Vec<String>>;

/// The name a written type text resolves by: one enclosing `[ ]` pair, trailing
/// `[]` array suffixes and a trailing generic argument list removed.
pub fn type_name(written: &str) -> String {
    let t = written.trim();
    let t = t
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(t)
        .trim();
    let t = units::property_type_name(t).trim_end();
    generic_base(t).unwrap_or(t).trim_end().to_string()
}

/// `t` without its trailing balanced `[ ... ]` group, when that group is non-empty.
fn generic_base(t: &str) -> Option<&str> {
    let body = t.strip_suffix(']')?;
    let mut depth = 1usize;
    for (i, c) in body.char_indices().rev() {
        match c {
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    return (!body[i + 1..].trim().is_empty()).then(|| &t[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a [`type_name`] result is a System type; the rule lives in `builtins`.
pub fn is_system_type(name: &str) -> bool {
    builtins::is_system_type(name)
}

/// The outcome of resolving a type name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRes {
    One { path: String, index: usize },
    Many,
    Outside,
}

/// Every scanned Class and Enum unit, by file and lowercased name.
pub struct TypeIndex {
    by_file: BTreeMap<String, HashMap<String, Vec<usize>>>,
}

impl TypeIndex {
    pub fn new(files: &[FileUnits]) -> Self {
        let mut by_file: BTreeMap<String, HashMap<String, Vec<usize>>> = BTreeMap::new();
        for f in files {
            let names = by_file.entry(f.path.clone()).or_default();
            for (i, u) in f.units.iter().enumerate() {
                if matches!(u.kind, UnitKind::Class | UnitKind::Enum) {
                    names.entry(u.name.to_lowercase()).or_default().push(i);
                }
            }
        }
        TypeIndex { by_file }
    }

    /// Resolves `name` referenced from `file`: the file itself, then its direct
    /// dot-sourced scripts, then every scanned file; the first step with a match decides.
    pub fn resolve_type(&self, name: &str, file: &str, dot: &DotSourceMap) -> TypeRes {
        let key = type_name(name).to_lowercase();
        let in_files = |paths: &mut dyn Iterator<Item = &String>| {
            let mut hits: Vec<(String, usize)> = Vec::new();
            for p in paths {
                for &i in self
                    .by_file
                    .get(p)
                    .and_then(|m| m.get(&key))
                    .into_iter()
                    .flatten()
                {
                    if !hits.iter().any(|(hp, hi)| hp == p && *hi == i) {
                        hits.push((p.clone(), i));
                    }
                }
            }
            match hits.as_slice() {
                [] => None,
                [(path, index)] => Some(TypeRes::One {
                    path: path.clone(),
                    index: *index,
                }),
                _ => Some(TypeRes::Many),
            }
        };
        let own = file.to_string();
        in_files(&mut std::iter::once(&own))
            .or_else(|| in_files(&mut dot.get(file).into_iter().flatten()))
            .or_else(|| in_files(&mut self.by_file.keys()))
            .unwrap_or(TypeRes::Outside)
    }
}

/// What a type reference or member call points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Unit {
        path: String,
        index: usize,
    },
    Unresolved {
        name: String,
        category: &'static str,
    },
}

/// The target of a resolved type reference written as `name`.
pub fn type_target(res: TypeRes, name: &str) -> Target {
    let n = type_name(name);
    let category = match res {
        TypeRes::One { path, index } => return Target::Unit { path, index },
        TypeRes::Many => "unknown",
        TypeRes::Outside if is_system_type(&n) => "stdlib",
        TypeRes::Outside => "external",
    };
    Target::Unresolved { name: n, category }
}

/// The single constructor (or method named `member`) of class unit `class`
/// taking `argc` parameters, if exactly one does.
pub fn select_member(
    file: &FileUnits,
    class: usize,
    member: &str,
    argc: usize,
    constructor: bool,
) -> Option<usize> {
    let mut hits = file.units.iter().enumerate().filter(|(_, u)| {
        u.owner == Some(class)
            && u.params.len() == argc
            && if constructor {
                u.kind == UnitKind::Constructor
            } else {
                u.kind == UnitKind::Method && u.name.to_lowercase() == member.to_lowercase()
            }
    });
    match (hits.next(), hits.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

/// `Some((module, command))` for a module-qualified `Q\N` (split at the last
/// `\`, both sides non-empty).
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    name.rsplit_once('\\')
        .filter(|(m, c)| !m.is_empty() && !c.is_empty())
}

/// The outcome of resolving a command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRes {
    One { path: String, index: usize },
    Many,
    Builtin,
    External,
}

/// A function unit: its file and unit index.
type Func = (String, usize);

/// Every scanned function, and each manifest module's candidate and export sets.
pub struct CommandIndex {
    /// Each file's Function units: index and name.
    funcs: BTreeMap<String, Vec<(usize, String)>>,
    /// Manifest path to its members' (and their dot-sourced scripts') functions.
    candidates: BTreeMap<String, Vec<Func>>,
    /// Manifest path to the candidates its export spec matches.
    exports: BTreeMap<String, Vec<Func>>,
    /// Lowercased `.psd1` stem to the manifests sharing it.
    by_stem: HashMap<String, Vec<String>>,
}

impl CommandIndex {
    pub fn new(files: &[FileUnits], map: &ModuleMap, dot: &DotSourceMap) -> Self {
        let funcs: BTreeMap<String, Vec<(usize, String)>> = files
            .iter()
            .map(|f| {
                let fs = f.units.iter().enumerate();
                let fs = fs.filter(|(_, u)| u.kind == UnitKind::Function);
                (
                    f.path.clone(),
                    fs.map(|(i, u)| (i, u.name.clone())).collect(),
                )
            })
            .collect();
        let by_stem = ManifestStems::new(map).by_stem;
        let mut idx = CommandIndex {
            funcs,
            candidates: BTreeMap::new(),
            exports: BTreeMap::new(),
            by_stem: HashMap::new(),
        };
        for id in by_stem.values().flatten() {
            let Some(m) = map.manifest_of(id) else {
                continue;
            };
            let srcs = m
                .members
                .iter()
                .flat_map(|mb| std::iter::once(mb).chain(dot.get(mb).into_iter().flatten()));
            let cands = dedup(srcs.flat_map(|p| idx.file_funcs(p)));
            let ex: Vec<Func> = cands
                .iter()
                .filter(|(p, i)| m.exports.matches(idx.name(p, *i)))
                .cloned()
                .collect();
            idx.exports.insert(id.clone(), ex);
            idx.candidates.insert(id.clone(), cands);
        }
        idx.by_stem = by_stem;
        idx
    }

    /// The export set of the manifest module `manifest` (its `.psd1` path).
    pub fn exports(&self, manifest: &str) -> Vec<(String, usize)> {
        self.exports.get(manifest).cloned().unwrap_or_default()
    }

    /// Resolves command `name` called from `file`: a path command, else a
    /// module-qualified lookup, else the unqualified four-step lookup.
    pub fn resolve_command(
        &self,
        name: &str,
        file: &str,
        dot: &DotSourceMap,
        map: &ModuleMap,
        index: &FileIndex,
    ) -> CommandRes {
        let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
        if name.contains(['\\', '/']) {
            let found = paths::resolve(dir, name).and_then(|p| index.find(&p));
            if let Some(p) = found.filter(|p| p.to_ascii_lowercase().ends_with(".ps1")) {
                return CommandRes::One {
                    path: p.to_string(),
                    index: 0,
                };
            }
        }
        let unmatched = |n: &str| {
            if builtins::is_builtin(n) {
                CommandRes::Builtin
            } else {
                CommandRes::External
            }
        };
        if let Some((m, n)) = split_qualified(name) {
            let ids = self.by_stem.get(&m.to_lowercase()).into_iter().flatten();
            let pool = ids.flat_map(|id| self.exports.get(id).into_iter().flatten().cloned());
            return self.pick(pool, n).unwrap_or_else(|| unmatched(name));
        }
        let bare = units::bare_name(name);
        let dotted = || {
            dot.get(file)
                .into_iter()
                .flatten()
                .flat_map(|p| self.file_funcs(p))
        };
        let own_module = || {
            let m = map.manifest_of(file);
            m.and_then(|m| self.candidates.get(&m.path))
                .into_iter()
                .flatten()
                .cloned()
        };
        let all_exports = || self.exports.values().flatten().cloned();
        self.pick(self.file_funcs(file), bare)
            .or_else(|| self.pick(dotted(), bare))
            .or_else(|| self.pick(own_module(), bare))
            .or_else(|| self.pick(all_exports(), bare))
            .unwrap_or_else(|| unmatched(bare))
    }

    fn file_funcs<'a>(&'a self, path: &'a str) -> impl Iterator<Item = Func> + 'a {
        let fs = self.funcs.get(path).into_iter().flatten();
        fs.map(move |(i, _)| (path.to_string(), *i))
    }

    fn name(&self, path: &str, index: usize) -> &str {
        let mut fs = self.funcs.get(path).into_iter().flatten();
        fs.find(|(i, _)| *i == index).map_or("", |(_, n)| n)
    }

    /// The functions of `pool` named `name` (case-insensitive), deduplicated:
    /// `None` when there are none.
    fn pick(&self, pool: impl Iterator<Item = Func>, name: &str) -> Option<CommandRes> {
        let key = name.to_lowercase();
        let hits = dedup(pool.filter(|(p, i)| self.name(p, *i).to_lowercase() == key));
        match hits.as_slice() {
            [] => None,
            [(path, index)] => Some(CommandRes::One {
                path: path.clone(),
                index: *index,
            }),
            _ => Some(CommandRes::Many),
        }
    }
}

/// `funcs` deduplicated by (path, index), first occurrence kept.
fn dedup(funcs: impl Iterator<Item = Func>) -> Vec<Func> {
    let mut out: Vec<Func> = Vec::new();
    for f in funcs {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// The target of a command written as `name` and resolved to `res`.
pub fn command_target(res: CommandRes, name: &str) -> Target {
    let n = if split_qualified(name).is_some() {
        name
    } else {
        units::bare_name(name)
    };
    let category = match res {
        CommandRes::One { path, index } => return Target::Unit { path, index },
        CommandRes::Many => "unknown",
        CommandRes::Builtin => "stdlib",
        CommandRes::External => "external",
    };
    Target::Unresolved {
        name: n.to_string(),
        category,
    }
}

/// What an invocation site calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    Command {
        name: String,
    },
    CallLiteral {
        arg: PathArg,
        written: String,
    },
    FuncValue,
    Static {
        type_name: String,
        member: String,
        argc: usize,
    },
    This {
        member: String,
        argc: usize,
    },
    Instance {
        member: String,
        argc: usize,
    },
}

/// An invocation site and its raw on-disk byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub kind: CallKind,
    pub raw_offset: usize,
}

/// Every invocation outside ERROR regions, in source order.
pub fn call_sites(src: &Decoded, tree: &Tree) -> Vec<CallSite> {
    let mut out = Vec::new();
    walk_calls(tree.root_node(), src, &mut out);
    out
}

fn walk_calls(node: Node, src: &Decoded, out: &mut Vec<CallSite>) {
    // A site inside an ERROR node (the whole-file ERROR root included) is skipped.
    if node.is_error() {
        return;
    }
    let text = &src.text;
    let kind = match node.kind() {
        "command" => command_call(node, text),
        "invocation_operator_command" => operator_call(node, text),
        "invocation_expression" => member_call(node, text),
        _ => None,
    };
    if let Some(kind) = kind {
        out.push(CallSite {
            kind,
            raw_offset: src.raw_offset(node.start_byte()),
        });
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_calls(child, src, out);
    }
}

/// A command named by a `command_name`, except module imports and `Import-DscResource`.
fn command_call(cmd: Node, text: &str) -> Option<CallKind> {
    let name = cmd
        .child_by_field_name("name")
        .filter(|n| n.kind() == "command_name")?;
    let name = &text[name.byte_range()];
    let excluded = IMPORT_NAMES.iter().chain(&["Import-DscResource"]);
    if excluded.into_iter().any(|m| m.eq_ignore_ascii_case(name)) {
        return None;
    }
    Some(CallKind::Command {
        name: name.to_string(),
    })
}

/// `&` on a static operand is a call literal, on anything else a function value;
/// `.` on a script block is a function value (any other `.` is a dot-source).
fn operator_call(node: Node, text: &str) -> Option<CallKind> {
    let op = node.child_by_field_name("operator")?.kind();
    let operand = node.child_by_field_name("command")?;
    match op {
        "&" => Some(match paths::path_arg(operand, text) {
            arg @ PathArg::Static { .. } => {
                let w = text[operand.byte_range()].trim();
                let unquoted = ['\'', '"']
                    .iter()
                    .find_map(|q| w.strip_prefix(*q)?.strip_suffix(*q));
                CallKind::CallLiteral {
                    arg,
                    written: unquoted.unwrap_or(w).to_string(),
                }
            }
            PathArg::Computed => CallKind::FuncValue,
        }),
        "." if is_script_block(operand) => Some(CallKind::FuncValue),
        _ => None,
    }
}

/// `[T]::m(...)` is Static, `$this.m(...)` / `${this}.m(...)` This, any other `x.m(...)` Instance.
fn member_call(node: Node, text: &str) -> Option<CallKind> {
    let access = node
        .child_by_field_name("function")
        .filter(|n| n.kind() == "member_access")?;
    let args = node
        .child_by_field_name("arguments")
        .filter(|n| n.kind() == "argument_list")?;
    let object = access.child_by_field_name("object")?;
    let member = access
        .child_by_field_name("member")
        .filter(|n| n.kind() == "identifier")?;
    let member = text[member.byte_range()].to_string();
    let mut c = args.walk();
    let argc = args
        .named_children(&mut c)
        .filter(|n| n.kind() != "comment")
        .count();
    let obj = &text[object.byte_range()];
    match access.child_by_field_name("operator")?.kind() {
        "::" if object.kind() == "type_literal" => Some(CallKind::Static {
            type_name: obj
                .strip_prefix('[')
                .and_then(|t| t.strip_suffix(']'))
                .unwrap_or(obj)
                .trim()
                .to_string(),
            member,
            argc,
        }),
        "." if object.kind() == "variable"
            && (obj.eq_ignore_ascii_case("$this") || obj.eq_ignore_ascii_case("${this}")) =>
        {
            Some(CallKind::This { member, argc })
        }
        "." => Some(CallKind::Instance { member, argc }),
        _ => None,
    }
}

/// The index of the innermost ScriptBody, Function, Method or Constructor unit
/// whose span holds `raw_offset`; the `<script>` body (0) when no other does.
pub fn enclosing(file: &FileUnits, raw_offset: usize) -> usize {
    let callers = file.units.iter().enumerate().filter(|(_, u)| {
        matches!(
            u.kind,
            UnitKind::ScriptBody | UnitKind::Function | UnitKind::Method | UnitKind::Constructor
        ) && (u.span.start..u.span.end).contains(&raw_offset)
    });
    callers
        .min_by_key(|(_, u)| u.span.end - u.span.start)
        .map_or(0, |(i, _)| i)
}
