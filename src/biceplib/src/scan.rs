//! Facts for the unified JSONL stream: a Module per Bicep file, a File per claimed
//! `.bicep`/`.bicepparam` source, and each file's declaration Structs and Functions with
//! their uses, calls, file dependencies and unresolved references. A syntax error or a
//! duplicate declaration is one stderr diagnostic; the scan continues past it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::diag::{error_summary, write_parse_error};
use crate::parse::{self, Kind};
use crate::resolve::{self, Fact, Source};
use crate::source;
use crate::units::{self, Units};

/// Unified JSONL records (SPEC §2), the `bicep` stream's wire shapes.
#[derive(Debug, serde::Serialize)]
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
    Calls {
        from: String,
        to: String,
    },
    /// A Struct-to-Struct use (no properties) or a Struct-to-File use (`properties.kind`
    /// `module`, `using`, `extends` or `import`, `to` the absolute File path).
    Uses {
        from: String,
        to: String,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
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

/// A claimed file: its repo-relative and absolute paths, line count and parse.
struct Parsed {
    rel: String,
    abs: String,
    lines: usize,
    file: parse::File,
}

/// The ARM `.json` templates a module statement names that exist under `root`, repo-relative.
/// They draw module edges only, so they enter the resolver as sources with no units.
fn json_targets(root: &Path, parsed: &[Parsed]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in parsed {
        for decl in &f.file.decls {
            let Some(src) = decl.literal.as_ref().and_then(|l| l.value.as_deref()) else {
                continue;
            };
            if decl.kind != Kind::Module
                || src.starts_with('/')
                || !src.to_ascii_lowercase().ends_with(".json")
            {
                continue;
            }
            let path = resolve::normalise(resolve::directory(&f.rel), src);
            if !path.split('/').any(|s| s == "..")
                && root.join(&path).is_file()
                && !out.contains(&path)
            {
                out.push(path);
            }
        }
    }
    out
}

/// The records of the claimed `files` (absolute path and bytes, sorted) before any `--targets`
/// filter, with the count of files with syntax errors. Each diagnostic goes to `err`.
fn facts(
    root: &Path,
    files: &[(PathBuf, Vec<u8>)],
    prefix: &str,
    err: &mut impl Write,
) -> io::Result<(Vec<Rec>, usize)> {
    let parsed: Vec<Parsed> = files
        .iter()
        .map(|(p, bytes)| {
            let text = source::decode_mapped(bytes);
            Parsed {
                rel: p
                    .strip_prefix(root)
                    .unwrap_or(p.as_path())
                    .to_string_lossy()
                    .into_owned(),
                abs: p.to_string_lossy().into_owned(),
                lines: source::line_count(&text.text),
                file: parse::parse(&text),
            }
        })
        .collect();
    let mut failed = 0;
    for f in &parsed {
        if !f.file.errors.is_empty() {
            failed += 1;
        }
        for e in &f.file.errors {
            write_parse_error(err, &f.abs, e)?;
        }
    }

    let json = json_targets(root, &parsed);
    let mut abs: HashMap<String, String> = parsed
        .iter()
        .map(|f| (f.rel.clone(), f.abs.clone()))
        .collect();
    for j in &json {
        abs.insert(j.clone(), root.join(j).to_string_lossy().into_owned());
    }
    let mut sources: Vec<Source> = parsed
        .iter()
        .map(|f| Source {
            path: &f.rel,
            units: units::file_units(&f.file),
        })
        .collect();
    sources.extend(json.iter().map(|j| Source {
        path: j,
        units: Units::default(),
    }));
    let resolved = resolve::resolve(&sources);

    // Ids are global over every source, in unit order, so a cross-file `uses` names its target.
    let mut offset = Vec::with_capacity(sources.len());
    let mut total = 0;
    for s in &sources {
        offset.push(total);
        total += s.units.units.len();
    }
    let ident = |file: usize, unit: usize| format!("{prefix}{}", offset[file] + unit + 1);
    // Each unit's key: its scope (the file module, or its parent's key) plus its name.
    let keys: Vec<Vec<String>> = sources
        .iter()
        .map(|s| {
            let mut keys: Vec<String> = Vec::with_capacity(s.units.units.len());
            for u in &s.units.units {
                let scope = u
                    .parent
                    .map_or_else(|| s.path.to_string(), |p| keys[p].clone());
                keys.push(format!("{scope}.{}", u.name));
            }
            keys
        })
        .collect();

    let mut recs = Vec::new();
    let mut named = HashSet::new();
    for (i, f) in parsed.iter().enumerate() {
        recs.push(Rec::Module { fqn: f.rel.clone() });
        recs.push(Rec::File {
            path: f.abs.clone(),
            parent: f.rel.clone(),
            start_line: 1,
            end_line: f.lines,
        });
        for d in &sources[i].units.duplicates {
            writeln!(
                err,
                "bicep: {}:{}: duplicate declaration {}",
                f.abs, d.line, d.name
            )?;
        }
        for (u_i, u) in sources[i].units.units.iter().enumerate() {
            let parent = u
                .parent
                .map_or_else(|| f.rel.clone(), |p| keys[i][p].clone());
            let id = ident(i, u_i);
            let (path, span) = (f.abs.clone(), u.span);
            let name = u.name.clone();
            recs.push(if u.function {
                Rec::Function {
                    id: id.clone(),
                    parent,
                    name,
                    path,
                    start: span.start,
                    end: span.end,
                    start_line: span.start_line,
                    end_line: span.end_line,
                }
            } else {
                Rec::Struct {
                    id: id.clone(),
                    parent,
                    name,
                    path,
                    start: span.start,
                    end: span.end,
                    start_line: span.start_line,
                    end_line: span.end_line,
                }
            });
            if let Some(p) = u.parent {
                recs.push(Rec::Contains {
                    from: ident(i, p),
                    to: id,
                });
            }
        }
        for fact in &resolved[i] {
            match fact {
                Fact::Uses { from, file, to } => recs.push(Rec::Uses {
                    from: ident(i, *from),
                    to: ident(*file, *to),
                    properties: BTreeMap::new(),
                }),
                Fact::Calls { from, to } => recs.push(Rec::Calls {
                    from: ident(i, *from),
                    to: ident(i, *to),
                }),
                Fact::FileUses { from, path, kind } => recs.push(Rec::Uses {
                    from: ident(i, *from),
                    to: abs[path].clone(),
                    properties: BTreeMap::from([("kind".to_string(), kind.to_string())]),
                }),
                Fact::Unresolved {
                    from,
                    name,
                    category,
                } => {
                    if named.insert(name.clone()) {
                        recs.push(Rec::Unresolved {
                            fqn: name.clone(),
                            category: category.to_string(),
                        });
                    }
                    recs.push(Rec::UnresolvedUse {
                        from: ident(i, *from),
                        to: name.clone(),
                    });
                }
            }
        }
    }
    Ok((recs, failed))
}

/// Keeps only the facts of the `targets` files: their File, Struct and Function records, a
/// Module a kept File sits in, a Contains or Calls when both ends are kept, a Uses/UnresolvedUse
/// when its source is kept (a Uses into a dropped Struct retargeted to that Struct's key; a File
/// target is a path, never rewritten), an Unresolved when a kept UnresolvedUse names it.
fn retain(recs: Vec<Rec>, targets: &HashSet<String>) -> Vec<Rec> {
    let keys: HashMap<String, String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::Struct {
                id, parent, name, ..
            }
            | Rec::Function {
                id, parent, name, ..
            } => Some((id.clone(), format!("{parent}.{name}"))),
            _ => None,
        })
        .collect();
    let mut recs: Vec<Rec> = recs
        .into_iter()
        .filter(|r| match r {
            Rec::File { path, .. } | Rec::Struct { path, .. } | Rec::Function { path, .. } => {
                targets.contains(path)
            }
            _ => true,
        })
        .collect();
    let ids: HashSet<String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::Struct { id, .. } | Rec::Function { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    recs.retain(|r| match r {
        Rec::Contains { from, to } | Rec::Calls { from, to } => {
            ids.contains(from) && ids.contains(to)
        }
        Rec::Uses { from, .. } | Rec::UnresolvedUse { from, .. } => ids.contains(from),
        _ => true,
    });
    for r in &mut recs {
        if let Rec::Uses { to, .. } = r {
            if let Some(k) = keys.get(to.as_str()).filter(|_| !ids.contains(to.as_str())) {
                *to = k.clone();
            }
        }
    }
    let named: HashSet<String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::UnresolvedUse { to, .. } => Some(to.clone()),
            _ => None,
        })
        .collect();
    recs.retain(|r| match r {
        Rec::Unresolved { fqn, .. } => named.contains(fqn),
        _ => true,
    });
    let modules: HashSet<String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::File { parent, .. } => Some(parent.clone()),
            _ => None,
        })
        .collect();
    recs.retain(|r| match r {
        Rec::Module { fqn } => modules.contains(fqn),
        _ => true,
    });
    recs
}

/// Writes every record as one JSON line.
fn write_jsonl(recs: &[Rec], w: &mut impl Write) -> io::Result<()> {
    for rec in recs {
        writeln!(
            w,
            "{}",
            serde_json::to_string(rec).expect("serialize record")
        )?;
    }
    Ok(())
}

const USAGE: &str = "usage: bicepfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]";

/// The parsed command line (see [`run`]).
#[derive(Debug)]
struct Args {
    root: String,
    modules: Vec<String>,
    id_prefix: String,
    targets: Option<String>,
    excludes: Vec<String>,
}

/// Parses the argv after the program name. A value flag left without its value, and any
/// unknown argument, is an exclude substring.
fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some((root, rest)) = argv.split_first() else {
        return Err(USAGE.to_string());
    };
    let mut args = Args {
        root: root.clone(),
        modules: Vec::new(),
        id_prefix: "bp".to_string(),
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

/// Runs the Bicep frontend over `argv` (the process arguments after the program name):
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

    let mut paths = source::claimed_files(&root);
    paths.retain(|p| {
        let s = p.to_string_lossy();
        (module_dirs.is_empty() || module_dirs.iter().any(|m| p.starts_with(m)))
            && !args.excludes.iter().any(|x| s.contains(x.as_str()))
    });
    let files: Vec<(PathBuf, Vec<u8>)> = paths
        .into_iter()
        .filter_map(|p| match std::fs::read(&p) {
            Ok(bytes) => Some((p, bytes)),
            Err(_) => {
                eprintln!("warning: could not read {}", p.display());
                None
            }
        })
        .collect();

    let mut err = io::stderr();
    let (recs, failed) =
        facts(&root, &files, &args.id_prefix, &mut err).map_err(|e| e.to_string())?;
    writeln!(err, "{}", error_summary(failed)).map_err(|e| e.to_string())?;
    let recs = match &targets {
        Some(set) => retain(recs, set),
        None => recs,
    };
    let mut out = BufWriter::new(io::stdout().lock());
    write_jsonl(&recs, &mut out).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}
