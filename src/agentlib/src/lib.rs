//! apg agent frontend (`agentfrontend`).
//!
//! One self-contained Rust binary that graphs a checkout's agent definitions:
//! one `module` record for the definition directory's identity, one `file`
//! record per definition file, and one `agent` record per definition carrying
//! its extension-stripped stem with the file's path and line span. It streams
//! the unified JSONL facts (SPEC §2) the Rust ingestor consumes — facts only,
//! never FQNs and never graph assembly — and is self-contained: it never shells
//! out and needs nothing on `PATH` at scan time.
//!
//! Usage: `agentfrontend <dir> [--id-prefix <p>] [--exclude-path <glob>]...
//! [exclude...]`.

use std::collections::HashSet;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Unified JSONL records (SPEC §2), the `agent` stream's wire shapes.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: u32,
        end_line: u32,
    },
    Agent {
        name: String,
        path: String,
        start_line: u32,
        end_line: u32,
    },
}

/// Serializes one record as a single JSONL line, terminated by `\n`.
pub fn write_rec<W: Write>(w: &mut W, rec: &Rec) -> io::Result<()> {
    let line = serde_json::to_string(rec).expect("serialize record");
    writeln!(w, "{line}")
}

/// The agent frontend's claim scope: from the checkout's repo-relative file
/// listing (each file's line count supplied by the stream writer's walk), the
/// definition files directly in `.opencode/agents/` alone — never a path
/// outside it. Yields `(repo-relative path, extension-stripped stem,
/// start_line, end_line)` per definition, sorted by path.
pub fn collect_definitions(files: &[(String, u32)]) -> Vec<(String, String, u32, u32)> {
    const DIR: &str = ".opencode/agents/";
    let mut defs: Vec<(String, String, u32, u32)> = files
        .iter()
        .filter_map(|(path, lines)| {
            let name = path.strip_prefix(DIR)?;
            // One normal component: a direct child, never a traversal.
            if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                return None;
            }
            let stem = Path::new(name).file_stem()?.to_string_lossy().into_owned();
            Some((path.clone(), stem, 1, (*lines).max(1)))
        })
        .collect();
    defs.sort();
    defs
}

/// Emits the agent stream over the collected definition list: one `module`
/// record for the `.opencode/agents` directory identity (the ingestor roots it
/// `agent..opencode/agents`), then per definition one `file` record parented on
/// that module plus one `agent` record carrying the extension-stripped stem and
/// the definition's path and line span. Writes nothing for an empty list.
pub fn emit_stream<W: Write>(w: &mut W, defs: &[(String, String, u32, u32)]) -> io::Result<()> {
    if defs.is_empty() {
        return Ok(());
    }
    let module = ".opencode/agents";
    write_rec(
        w,
        &Rec::Module {
            fqn: module.to_string(),
        },
    )?;
    for (path, stem, start_line, end_line) in defs {
        write_rec(
            w,
            &Rec::File {
                path: path.clone(),
                parent: module.to_string(),
                start_line: *start_line,
                end_line: *end_line,
            },
        )?;
        write_rec(
            w,
            &Rec::Agent {
                name: stem.clone(),
                path: path.clone(),
                start_line: *start_line,
                end_line: *end_line,
            },
        )?;
    }
    Ok(())
}

/// Parsed command line: the project dir, the scan driver's flags, and its
/// `--targets <file>` hand-off path in `targets_path` (newline-delimited
/// absolute source paths under the pinned FrontendHandoff contract).
pub struct Args {
    pub root: PathBuf,
    pub id_prefix: String,
    pub excludes: Vec<String>,
    pub targets_path: Option<String>,
}

fn usage() -> String {
    "usage: agentfrontend <dir> [--id-prefix <p>] [--exclude-path <glob>]... [exclude...]"
        .to_string()
}

/// Parses the frontend command line into `Args`. The scan driver forwards
/// `--exclude-path` values as bare arguments; the explicit flag form and the
/// driver's hand-off flags are accepted too. `targets_path` is `None` by
/// default and carries the driver's `--targets <file>` hand-off path.
///
/// ```
/// let argv: Vec<String> = ["repo", "--id-prefix", "a1", "--exclude-path", "*.md", "vendor"]
///     .iter()
///     .map(|s| s.to_string())
///     .collect();
/// let args = agentfrontend::parse_args(&argv).unwrap();
/// assert_eq!(args.root, std::path::PathBuf::from("repo"));
/// assert_eq!(args.id_prefix, "a1");
/// assert_eq!(args.excludes, vec!["*.md".to_string(), "vendor".to_string()]);
/// assert_eq!(args.targets_path, None);
/// ```
pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        id_prefix: "n".to_string(),
        excludes: Vec::new(),
        targets_path: None,
    };
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--id-prefix" => match next {
                Some(v) => {
                    args.id_prefix = v;
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--exclude-path" => match next {
                Some(v) => {
                    args.excludes.push(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            // The driver's target-list hand-off, kept for the run-time filter.
            "--targets" => match next {
                Some(v) => {
                    args.targets_path = Some(v);
                    i += 2;
                }
                None => i += 1,
            },
            // Accepted for the pinned front-end hand-off, then ignored: the
            // agent stream has no module subtree or native cache.
            "--module" | "--cache-dir" | "--cache-key" => match next {
                Some(_) => i += 2,
                None => i += 1,
            },
            other => {
                args.excludes.push(other.to_string());
                i += 1;
            }
        }
    }
    Ok(args)
}

/// Runs the agent frontend against the process arguments (the binary entry
/// point delegates here).
pub fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("agentfrontend: {e}");
        std::process::exit(1);
    }
}

/// Emits the agent stream: probe the claim path one component at a time
/// (`.opencode` then `.opencode/agents`) with `symlink_metadata`, declining the
/// run when a component is absent, a symlink or not a directory; otherwise read
/// the directory's direct regular children, apply `--exclude-path` globs and the
/// `--targets` list, and emit over a buffered stdout.
pub fn run(args: Args) -> io::Result<()> {
    let agents_dir = args.root.join(".opencode/agents");
    for dir in [args.root.join(".opencode"), agents_dir.clone()] {
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            return Ok(());
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Ok(());
        }
    }

    // The pinned `--targets` hand-off: newline-delimited absolute paths. An
    // absent flag, an unreadable file or an empty list is no filter.
    let target_set = args.targets_path.as_deref().and_then(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        let set: HashSet<PathBuf> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        (!set.is_empty()).then_some(set)
    });

    // The driver's `--exclude-path` form: a whole-value glob where `*` matches
    // any run (including `/`) and `?` a single character.
    let excluded = |identity: &str| -> bool {
        args.excludes.iter().any(|pat| {
            let pat: Vec<char> = pat.chars().collect();
            let txt: Vec<char> = identity.chars().collect();
            let (mut pi, mut ti) = (0usize, 0usize);
            let (mut star, mut star_ti) = (None, 0usize);
            while ti < txt.len() {
                if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
                    pi += 1;
                    ti += 1;
                } else if pi < pat.len() && pat[pi] == '*' {
                    star = Some(pi);
                    star_ti = ti;
                    pi += 1;
                    while pi < pat.len() && pat[pi] == '*' {
                        pi += 1;
                    }
                } else if let Some(sp) = star {
                    pi = sp + 1;
                    star_ti += 1;
                    ti = star_ti;
                } else {
                    return false;
                }
            }
            while pi < pat.len() && pat[pi] == '*' {
                pi += 1;
            }
            pi == pat.len()
        })
    };

    let mut files: Vec<(String, u32)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type` never follows a symlink: only a regular direct
        // child is a definition.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let identity = format!(".opencode/agents/{}", entry.file_name().to_string_lossy());
        if excluded(&identity) {
            continue;
        }
        let abs = args.root.join(&identity);
        if let Some(set) = &target_set {
            if !set.contains(&abs) {
                continue;
            }
        }
        let Ok(bytes) = std::fs::read(&abs) else {
            continue;
        };
        let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u32;
        let lines = if bytes.last() == Some(&b'\n') {
            newlines
        } else {
            newlines + 1
        };
        files.push((identity, lines));
    }

    let defs = collect_definitions(&files);
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    emit_stream(&mut writer, &defs)?;
    writer.flush()
}

#[cfg(test)]
mod tests;
