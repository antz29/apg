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

use std::io::{self, Write};
use std::path::PathBuf;

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

/// Parsed command line: the project dir plus the scan driver's flags.
pub struct Args {
    pub root: PathBuf,
    pub id_prefix: String,
    pub excludes: Vec<String>,
}

fn usage() -> String {
    "usage: agentfrontend <dir> [--id-prefix <p>] [--exclude-path <glob>]... [exclude...]"
        .to_string()
}

/// Parses the frontend command line. The scan driver forwards `--exclude-path`
/// values as bare arguments; the explicit flag form and the driver's hand-off
/// flags are accepted too.
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
/// ```
pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        id_prefix: "n".to_string(),
        excludes: Vec::new(),
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
            // Accepted for the pinned front-end hand-off, then ignored: the
            // agent stream has no module subtree, target list or native cache.
            "--module" | "--targets" | "--cache-dir" | "--cache-key" => match next {
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

/// Emits the agent stream. The definition walk and record emitter are the
/// stream's next units; until they land the stream is empty.
pub fn run(_args: Args) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
