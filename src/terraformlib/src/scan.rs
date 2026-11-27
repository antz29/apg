//! Facts for the unified JSONL stream: a Module per configuration directory, a File per
//! claimed Terraform source (shadowed and failed files included), and each parsed module's
//! block Structs with their uses, file dependencies and unresolved references. Each failed
//! parse is one stderr diagnostic; the scan continues past it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::diag::{error_summary, write_diagnostic};
use crate::module;
use crate::parse::parse;
use crate::resolve::{self, Fact};
use crate::source;
use crate::units::{self, Parsed};

/// Unified JSONL records (SPEC §2), the `terraform` stream's wire shapes.
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
    Contains {
        from: String,
        to: String,
    },
    /// A Struct's file dependency (`properties.kind` `module-source` or `override`, `to` the
    /// absolute File path) or a Struct-to-Struct use (no properties).
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

/// The key of `name` under scope `parent` (the same form pslib keys its units by).
fn key(parent: &str, name: &str) -> String {
    format!("{parent}.{name}")
}

/// The records of the claimed `files` (absolute path and bytes, sorted) before any `--targets`
/// filter, with the count of files whose parse failed. Each failure's diagnostic goes to `err`.
fn facts(
    root: &Path,
    files: &[(PathBuf, Vec<u8>)],
    prefix: &str,
    err: &mut impl Write,
) -> io::Result<(Vec<Rec>, usize)> {
    let paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
    let modules = module::modules(root, &paths);
    let shadowed: HashSet<&str> = modules
        .values()
        .flat_map(|m| m.shadowed.iter().map(String::as_str))
        .collect();
    // Each file's absolute path and line count; the unshadowed ones also parse.
    let mut meta: HashMap<String, (String, usize)> = HashMap::new();
    let mut parsed: Vec<Parsed> = Vec::new();
    let mut failed = 0;
    for (p, bytes) in files {
        let rel = module::rel(root, p);
        let abs = p.to_string_lossy().into_owned();
        let text = source::decode_mapped(bytes);
        meta.insert(rel.clone(), (abs.clone(), source::line_count(&text.text)));
        if shadowed.contains(rel.as_str()) {
            continue;
        }
        match parse(p, &text.text) {
            Ok(body) => parsed.push(Parsed {
                path: rel,
                text,
                body,
            }),
            Err(e) => {
                write_diagnostic(err, &abs, e.line, e.column, &e.message)?;
                failed += 1;
            }
        }
    }

    let mut recs = Vec::new();
    let mut named = HashSet::new();
    let mut next = 0;
    for (dir, m) in &modules {
        recs.push(Rec::Module { fqn: dir.clone() });
        for rel in m
            .base
            .iter()
            .chain(&m.overrides)
            .chain(&m.tests)
            .chain(&m.tfvars)
            .chain(&m.locks)
            .chain(&m.shadowed)
        {
            let (abs, lines) = &meta[rel.as_str()];
            recs.push(Rec::File {
                path: abs.clone(),
                parent: dir.clone(),
                start_line: 1,
                end_line: *lines,
            });
        }
        let found = units::module_units(dir, m, &parsed);
        for d in &found.diagnostics {
            write_diagnostic(err, &meta[d.file.as_str()].0, d.line, d.column, &d.message)?;
        }
        let refs = resolve::resolve(dir, &modules, &found, &parsed);
        // Ids in emission order, so a `--targets` filter keeps the full scan's identities.
        let ids: Vec<String> = found
            .structs
            .iter()
            .map(|_| {
                next += 1;
                format!("{prefix}{next}")
            })
            .collect();
        for (i, u) in found.structs.iter().enumerate() {
            let (abs, _) = &meta[u.file];
            let parent = match u.parent {
                Some(p) => key(dir, &found.structs[p].name),
                None => dir.clone(),
            };
            recs.push(Rec::Struct {
                id: ids[i].clone(),
                parent,
                name: u.name.clone(),
                path: abs.clone(),
                start: u.span.start,
                end: u.span.end,
                start_line: u.span.start_line,
                end_line: u.span.end_line,
            });
            if let Some(p) = u.parent {
                recs.push(Rec::Contains {
                    from: ids[p].clone(),
                    to: ids[i].clone(),
                });
            }
        }
        for f in refs {
            match f {
                Fact::Uses { from, to } => recs.push(Rec::Uses {
                    from: ids[from].clone(),
                    to: ids[to].clone(),
                    properties: BTreeMap::new(),
                }),
                Fact::FileUses { from, path, kind } => recs.push(Rec::Uses {
                    from: ids[from].clone(),
                    to: meta[path.as_str()].0.clone(),
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
                        from: ids[from].clone(),
                        to: name,
                    });
                }
            }
        }
    }
    Ok((recs, failed))
}

/// Keeps only the facts of the `targets` files: their File and Struct records, a Module a kept
/// File sits in, a Contains when both ends are kept, a Uses/UnresolvedUse when its source is
/// kept (a Uses into a dropped Struct retargeted to that Struct's key; a File target is a path,
/// never rewritten), an Unresolved when a kept UnresolvedUse names it.
fn retain(recs: Vec<Rec>, targets: &HashSet<String>) -> Vec<Rec> {
    let keys: HashMap<String, String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::Struct {
                id, parent, name, ..
            } => Some((id.clone(), key(parent, name))),
            _ => None,
        })
        .collect();
    let mut recs: Vec<Rec> = recs
        .into_iter()
        .filter(|r| match r {
            Rec::File { path, .. } | Rec::Struct { path, .. } => targets.contains(path),
            _ => true,
        })
        .collect();
    let ids: HashSet<String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::Struct { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    recs.retain(|r| match r {
        Rec::Contains { from, to } => ids.contains(from) && ids.contains(to),
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
    let parents: HashSet<String> = recs
        .iter()
        .filter_map(|r| match r {
            Rec::File { parent, .. } => Some(parent.clone()),
            _ => None,
        })
        .collect();
    recs.retain(|r| match r {
        Rec::Module { fqn } => parents.contains(fqn),
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

const USAGE: &str = "usage: terraformfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
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

/// Runs the Terraform frontend over `argv` (the process arguments after the program name):
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
