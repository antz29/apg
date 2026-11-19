//! Facts for the unified JSONL stream: a Module per manifest or scan-root-relative directory and a File
//! per claimed PowerShell source, a script Struct plus its units (contains-linked) per
//! `.ps1`/`.psm1` with its file dependencies (uses/unresolved uses), with each file's
//! syntax-error regions reported to stderr.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use crate::diag::{self, write_diagnostic, Diagnostic};
use crate::identity;
use crate::manifest::{self, ModuleMap};
use crate::parse::parse;
use crate::paths::FileIndex;
use crate::resolve::{self, Dep, DepKind, DepSite, ManifestStems};
use crate::source;
use crate::units::{self, FileUnits, UnitKind};

/// Unified JSONL records (SPEC §2), the `ps` stream's wire shapes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: usize,
        end_line: usize,
    },
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: usize,
        end: usize,
        start_line: usize,
        end_line: usize,
    },
    Function {
        id: String,
        parent: String,
        name: String,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        params: Vec<String>,
        file: String,
        path: String,
        start: usize,
        end: usize,
        start_line: usize,
        end_line: usize,
    },
    Contains {
        from: String,
        to: String,
    },
    /// A script's file dependency: its Struct's `uses` of the File it dot-sources or imports,
    /// `properties.kind` `dot-source` or `import-module`.
    Uses {
        from: String,
        to: String,
        #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
        properties: BTreeMap<String, String>,
    },
    UnresolvedUse {
        from: String,
        to: String,
    },
    Unresolved {
        fqn: String,
        category: String,
    },
}

/// Accumulates one scan's records. Ids are `<prefix>1`, `<prefix>2`, ... in emission order;
/// module identities are relative to `base` (the scan directory).
#[derive(Debug)]
pub struct Emitter {
    recs: Vec<Rec>,
    modules: BTreeSet<String>,
    map: ModuleMap,
    /// Every scanned file's base-relative path.
    paths: BTreeSet<String>,
    /// Each scanned script's record path and its (base-relative) units, in scan order.
    units: Vec<(String, FileUnits)>,
    /// Each scanned script's dependency sites, parallel to `units`.
    sites: Vec<Vec<DepSite>>,
    /// Every scanned file's base-relative path to its File record path.
    record_paths: HashMap<String, String>,
    /// Each script's (base-relative) dot-sourced File paths, first-site order.
    dot_sourced: HashMap<String, Vec<String>>,
    /// How many scanned files held at least one error region.
    errored: usize,
    next_id: usize,
    base: PathBuf,
    prefix: String,
}

/// `path`'s `/`-joined components relative to `base` (just its file name outside `base`, so
/// it falls in the root's empty-identity directory module).
fn rel_of(base: &Path, path: &Path) -> String {
    let rel = path
        .strip_prefix(base)
        .unwrap_or_else(|_| path.file_name().map_or(path, Path::new));
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

impl Emitter {
    /// An emitter whose module identities are relative to `base` and whose ids start `id_prefix`.
    pub fn new(base: &Path, id_prefix: &str) -> Self {
        Emitter {
            recs: Vec::new(),
            modules: BTreeSet::new(),
            map: ModuleMap::default(),
            paths: BTreeSet::new(),
            units: Vec::new(),
            sites: Vec::new(),
            record_paths: HashMap::new(),
            dot_sourced: HashMap::new(),
            errored: 0,
            next_id: 0,
            base: base.to_path_buf(),
            prefix: id_prefix.to_string(),
        }
    }

    /// Sets the module map that parents each scanned file (directory modules until set).
    pub fn set_modules(&mut self, map: ModuleMap) {
        self.map = map;
    }

    /// The next opaque id: `<prefix>1`, `<prefix>2`, ...
    pub fn fresh_id(&mut self) -> String {
        self.next_id += 1;
        format!("{}{}", self.prefix, self.next_id)
    }

    /// Scans one PowerShell file: `path` is the identity its records carry, `bytes` its raw
    /// content. Returns its syntax-error diagnostics, which are not emitted as records.
    pub fn scan_file(&mut self, path: &str, bytes: &[u8]) -> Vec<Diagnostic> {
        let decoded = source::decode_mapped(bytes);
        let text = &decoded.text;
        let tree = parse(text);
        let diags = diag::error_regions(path, text, &tree);
        if !diags.is_empty() {
            self.errored += 1;
        }
        let rel = rel_of(&self.base, Path::new(path));
        let script = Path::new(path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ps1") || e.eq_ignore_ascii_case("psm1"));
        if script {
            self.units
                .push((path.to_string(), units::scan_units(&rel, &decoded, &tree)));
            self.sites.push(resolve::file_deps(&decoded, &tree));
        }
        let module = self.map.module(&rel);
        self.record_paths.insert(rel.clone(), path.to_string());
        self.paths.insert(rel);
        if self.modules.insert(module.clone()) {
            self.recs.push(Rec::Module {
                fqn: module.clone(),
            });
        }
        self.recs.push(Rec::File {
            path: path.to_string(),
            parent: module,
            start_line: 1,
            end_line: source::line_count(text),
        });
        diags
    }

    /// Names every scanned script's units over the whole tree, then emits per script (scan
    /// order) its Struct and each unit in ordinal order, each contained by its script (a
    /// Method/Constructor by its class).
    pub fn finish(&mut self) {
        let (paths, files): (Vec<String>, Vec<FileUnits>) =
            std::mem::take(&mut self.units).into_iter().unzip();
        let id = identity::resolve(&self.map, &files, &self.paths);
        let index = FileIndex::new(self.paths.iter().cloned());
        let stems = ManifestStems::new(&self.map);
        let sites = std::mem::take(&mut self.sites);
        let mut named = HashSet::new();
        for ((path, fu), sites) in paths.iter().zip(&files).zip(&sites) {
            let rel = fu.path.as_str();
            let script = self.fresh_id();
            let span = fu.file_span;
            self.recs.push(Rec::Struct {
                id: script.clone(),
                parent: id.script_parent(rel),
                name: id.script_name(rel).expect("scanned script").to_string(),
                path: path.clone(),
                start: span.start,
                end: span.end,
                start_line: span.start_line,
                end_line: span.end_line,
            });
            let mut order: Vec<usize> = (0..fu.units.len()).collect();
            order.sort_by_key(|&i| fu.units[i].ordinal);
            let mut ids: Vec<Option<String>> = vec![None; fu.units.len()];
            for i in order {
                let u = &fu.units[i];
                let uid = self.fresh_id();
                let (parent, name) = (id.unit_parent(rel, i), id.unit_name(rel, i).to_string());
                let s = u.span;
                self.recs.push(match u.kind {
                    UnitKind::Class | UnitKind::Enum => Rec::Struct {
                        id: uid.clone(),
                        parent,
                        name,
                        path: path.clone(),
                        start: s.start,
                        end: s.end,
                        start_line: s.start_line,
                        end_line: s.end_line,
                    },
                    _ => Rec::Function {
                        id: uid.clone(),
                        parent,
                        name,
                        params: u.params.clone(),
                        file: path.clone(),
                        path: path.clone(),
                        start: s.start,
                        end: s.end,
                        start_line: s.start_line,
                        end_line: s.end_line,
                    },
                });
                // A class precedes its members in ordinal order, so its id is set.
                let from = u.owner.map_or_else(
                    || script.clone(),
                    |o| ids[o].clone().expect("class before member"),
                );
                self.recs.push(Rec::Contains {
                    from,
                    to: uid.clone(),
                });
                ids[i] = Some(uid);
            }
            let deps: Vec<Dep> = sites
                .iter()
                .map(|s| resolve::resolve_site(s, rel, &index, &stems))
                .collect();
            // Dot-source wins: one uses per File, whatever the order and count of its sites.
            let mut dots: Vec<String> = Vec::new();
            for d in &deps {
                if let Dep::File {
                    path: p,
                    kind: DepKind::DotSource,
                } = d
                {
                    if !dots.contains(p) {
                        dots.push(p.clone());
                    }
                }
            }
            let (mut linked, mut used) = (HashSet::new(), HashSet::new());
            for d in deps {
                match d {
                    Dep::File { path: p, .. } => {
                        if linked.insert(p.clone()) {
                            let kind = if dots.contains(&p) {
                                DepKind::DotSource
                            } else {
                                DepKind::ImportModule
                            };
                            self.recs.push(Rec::Uses {
                                from: script.clone(),
                                to: self.record_paths.get(&p).cloned().unwrap_or(p),
                                properties: BTreeMap::from([(
                                    "kind".to_string(),
                                    kind.as_str().to_string(),
                                )]),
                            });
                        }
                    }
                    Dep::Unresolved { name, category } => {
                        if named.insert(name.clone()) {
                            self.recs.push(Rec::Unresolved {
                                fqn: name.clone(),
                                category: category.to_string(),
                            });
                        }
                        if used.insert(name.clone()) {
                            self.recs.push(Rec::UnresolvedUse {
                                from: script.clone(),
                                to: name,
                            });
                        }
                    }
                }
            }
            if !dots.is_empty() {
                self.dot_sourced.insert(rel.to_string(), dots);
            }
        }
    }

    /// The (base-relative) Files the script at base-relative `path` dot-sources, first-site
    /// order; empty when none.
    pub fn dot_sourced(&self, path: &str) -> &[String] {
        self.dot_sourced.get(path).map_or(&[], Vec::as_slice)
    }

    /// The records emitted so far, in order.
    pub fn records(&self) -> &[Rec] {
        &self.recs
    }

    /// The closing stderr line over every file scanned so far.
    pub fn error_summary(&self) -> String {
        diag::error_summary(self.errored)
    }

    /// Keeps only the facts of the `targets` files (File records and the Struct/Function
    /// records located in them); a Module stays when a kept File sits in it, a Contains edge
    /// when both its ends are kept, a Uses/UnresolvedUse when its source is kept (a File target
    /// is a path, never rewritten), an Unresolved when a kept UnresolvedUse names it.
    pub fn retain_targets(&mut self, targets: &HashSet<String>) {
        self.recs.retain(|r| match r {
            Rec::File { path, .. } | Rec::Struct { path, .. } | Rec::Function { path, .. } => {
                targets.contains(path)
            }
            _ => true,
        });
        let ids: HashSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::Struct { id, .. } | Rec::Function { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        self.recs.retain(|r| match r {
            Rec::Contains { from, to } => ids.contains(from) && ids.contains(to),
            Rec::Uses { from, .. } | Rec::UnresolvedUse { from, .. } => ids.contains(from),
            _ => true,
        });
        let named: HashSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::UnresolvedUse { to, .. } => Some(to.clone()),
                _ => None,
            })
            .collect();
        self.recs.retain(|r| match r {
            Rec::Unresolved { fqn, .. } => named.contains(fqn),
            _ => true,
        });
        let parents: BTreeSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::File { parent, .. } => Some(parent.clone()),
                _ => None,
            })
            .collect();
        self.recs.retain(|r| match r {
            Rec::Module { fqn } => parents.contains(fqn),
            _ => true,
        });
    }

    /// Writes every record as one JSON line.
    pub fn write_jsonl(&self, w: &mut impl Write) -> io::Result<()> {
        for rec in &self.recs {
            let line = serde_json::to_string(rec).expect("serialize record");
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}

const USAGE: &str = "usage: psfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]";

/// The parsed command line (see [`run`]).
#[derive(Debug)]
pub(crate) struct Args {
    pub(crate) root: String,
    pub(crate) modules: Vec<String>,
    pub(crate) id_prefix: String,
    pub(crate) targets: Option<String>,
    pub(crate) excludes: Vec<String>,
}

/// Parses the argv after the program name. A value flag left without its value, and any
/// unknown argument, is an exclude substring.
pub(crate) fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some((root, rest)) = argv.split_first() else {
        return Err(USAGE.to_string());
    };
    let mut args = Args {
        root: root.clone(),
        modules: Vec::new(),
        id_prefix: "n".to_string(),
        targets: None,
        excludes: Vec::new(),
    };
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        i += 1;
        let Some(value) = rest.get(i).filter(|_| value_flag(arg)).cloned() else {
            args.excludes.push(arg.to_string());
            continue;
        };
        i += 1;
        match arg {
            "--module" => args.modules.push(value),
            "--id-prefix" => args.id_prefix = value,
            "--targets" => args.targets = Some(value),
            // Accepted for the pinned hand-off, then ignored (no native cache).
            _ => {}
        }
    }
    Ok(args)
}

/// True for the flags that consume the following argument.
fn value_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--module" | "--id-prefix" | "--targets" | "--cache-dir" | "--cache-key"
    )
}

/// `path` made absolute and symlink-resolved when it exists (so the walk and the target list
/// agree on identities).
fn absolutize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_or_else(|_| path.to_path_buf(), |d| d.join(path))
        }
    })
}

/// The PowerShell source extensions (`.ps1`, `.psm1`, `.psd1`), in any letter case.
pub(crate) fn is_ps_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ps1" | "psm1" | "psd1"))
}

/// Every claimed source at or below `dir`. Build, VCS, worktree and the top-level `apg/`
/// layout trees are pruned.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pruned = matches!(
                name.as_str(),
                "target" | "node_modules" | ".git" | ".worktrees"
            );
            if pruned || (dir == root && name == "apg") {
                continue;
            }
            walk(root, &path, out);
        } else if ft.is_file() && is_ps_source(&path) {
            out.push(path);
        }
    }
}

/// The `--targets` list: absolute paths, one per line, blanks ignored. An absent, unreadable
/// or empty list is no filter (the full scan).
fn read_targets(path: Option<&str>) -> Option<HashSet<String>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| absolutize(Path::new(l)).to_string_lossy().into_owned())
        .collect();
    if set.is_empty() {
        eprintln!("warning: targets {path} is empty; scanning unfiltered");
    }
    (!set.is_empty()).then_some(set)
}

/// Runs the PowerShell frontend over `argv` (the process arguments after the program name):
/// walks the tree under the positional directory, scans every claimed file writing its
/// diagnostics to stderr, then writes the facts to stdout, filtered to the `--targets` files
/// when given.
pub fn run(argv: &[String]) -> Result<(), String> {
    let args = parse_args(argv)?;
    let root = absolutize(Path::new(&args.root));
    let module_dirs: Vec<PathBuf> = args
        .modules
        .iter()
        .map(|m| absolutize(&root.join(m)))
        .collect();
    let targets = read_targets(args.targets.as_deref());

    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.retain(|p| {
        let s = p.to_string_lossy();
        (module_dirs.is_empty() || module_dirs.iter().any(|m| p.starts_with(m)))
            && !args.excludes.iter().any(|x| s.contains(x.as_str()))
    });
    files.sort();

    // Pass 1: read every file, then assign modules over the whole claimed tree.
    let files: Vec<(PathBuf, Vec<u8>)> = files
        .into_iter()
        .filter_map(|p| match std::fs::read(&p) {
            Ok(bytes) => Some((p, bytes)),
            Err(_) => {
                eprintln!("warning: could not read {}", p.display());
                None
            }
        })
        .collect();
    let rels: Vec<String> = files.iter().map(|(p, _)| rel_of(&root, p)).collect();
    let manifests: Vec<_> = files
        .iter()
        .zip(&rels)
        .filter(|((p, _), _)| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("psd1"))
        })
        .filter_map(|((_, bytes), rel)| {
            let text = source::decode(bytes);
            manifest::read_manifest(&text, &parse(&text)).map(|keys| (rel.clone(), keys))
        })
        .collect();

    let mut emitter = Emitter::new(&root, &args.id_prefix);
    emitter.set_modules(manifest::assign(&rels, &manifests));
    let mut err = io::stderr();
    for (path, bytes) in &files {
        for diag in emitter.scan_file(&path.to_string_lossy(), bytes) {
            write_diagnostic(&mut err, &diag).map_err(|e| e.to_string())?;
        }
    }
    // Ids, ordinals and names over the whole claimed tree, before any --targets filter.
    emitter.finish();

    writeln!(err, "{}", emitter.error_summary()).map_err(|e| e.to_string())?;
    if let Some(set) = &targets {
        emitter.retain_targets(set);
    }
    let mut out = BufWriter::new(io::stdout().lock());
    emitter.write_jsonl(&mut out).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}
