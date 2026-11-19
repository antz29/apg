//! Facts for the unified JSONL stream: a Module per scan-root-relative directory and a File
//! per claimed PowerShell source, with each file's syntax-error regions reported to stderr.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use crate::diag::{self, write_diagnostic, Diagnostic};
use crate::parse::parse;
use crate::source;

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
}

/// Accumulates one scan's records. Ids are `<prefix>1`, `<prefix>2`, ... in emission order;
/// module identities are relative to `base` (the scan directory).
#[derive(Debug)]
pub struct Emitter {
    recs: Vec<Rec>,
    modules: BTreeSet<String>,
    /// How many scanned files held at least one error region.
    errored: usize,
    next_id: usize,
    base: PathBuf,
    prefix: String,
}

/// The module identity of `path`: its `/`-joined directory components relative to `base`,
/// EMPTY at the base itself (never `.`, never absolute).
fn module_of(base: &Path, path: &Path) -> String {
    let dir = path.parent().unwrap_or(path);
    dir.strip_prefix(base).map_or_else(
        |_| String::new(),
        |rel| {
            rel.components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        },
    )
}

impl Emitter {
    /// An emitter whose module identities are relative to `base` and whose ids start `id_prefix`.
    pub fn new(base: &Path, id_prefix: &str) -> Self {
        Emitter {
            recs: Vec::new(),
            modules: BTreeSet::new(),
            errored: 0,
            next_id: 0,
            base: base.to_path_buf(),
            prefix: id_prefix.to_string(),
        }
    }

    /// The next opaque id: `<prefix>1`, `<prefix>2`, ...
    pub fn fresh_id(&mut self) -> String {
        self.next_id += 1;
        format!("{}{}", self.prefix, self.next_id)
    }

    /// Scans one PowerShell file: `path` is the identity its records carry, `bytes` its raw
    /// content. Returns its syntax-error diagnostics, which are not emitted as records.
    pub fn scan_file(&mut self, path: &str, bytes: &[u8]) -> Vec<Diagnostic> {
        let text = source::decode(bytes);
        let tree = parse(&text);
        let diags = diag::error_regions(path, &text, &tree);
        if !diags.is_empty() {
            self.errored += 1;
        }
        let module = module_of(&self.base, Path::new(path));
        if self.modules.insert(module.clone()) {
            self.recs.push(Rec::Module {
                fqn: module.clone(),
            });
        }
        self.recs.push(Rec::File {
            path: path.to_string(),
            parent: module,
            start_line: 1,
            end_line: source::line_count(&text),
        });
        diags
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
    /// records located in them); a Module stays when a kept File sits in it.
    pub fn retain_targets(&mut self, targets: &HashSet<String>) {
        self.recs.retain(|r| match r {
            Rec::File { path, .. } | Rec::Struct { path, .. } | Rec::Function { path, .. } => {
                targets.contains(path)
            }
            Rec::Module { .. } => true,
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

    let mut emitter = Emitter::new(&root, &args.id_prefix);
    let mut err = io::stderr();
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("warning: could not read {}", path.display());
            continue;
        };
        for diag in emitter.scan_file(&path.to_string_lossy(), &bytes) {
            write_diagnostic(&mut err, &diag).map_err(|e| e.to_string())?;
        }
    }

    writeln!(err, "{}", emitter.error_summary()).map_err(|e| e.to_string())?;
    if let Some(set) = &targets {
        emitter.retain_targets(set);
    }
    let mut out = BufWriter::new(io::stdout().lock());
    emitter.write_jsonl(&mut out).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}
