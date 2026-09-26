//! apg structural frontend (`structfrontend`).
//!
//! One self-contained Rust binary that walks a checkout, claims every tracked
//! file no code frontend claims — shell, YAML, JSON, TOML, XML, Dockerfile,
//! Makefile, the INI family, Markdown, and the residual `misc` — and streams
//! the unified JSONL facts (SPEC §2) the Rust ingestor consumes: one `Module`
//! per directory identity, one `File` per claimed file, one `Struct` per
//! declared structure, plus the `contains` edges that nest them.
//!
//! It absorbs the retired `mdfrontend` Markdown frontend: the `md` stream id is
//! preserved and the heading-Struct mechanism is unchanged. The one deliberate
//! identity change is the emitted **module identity**: it is REPO-RELATIVE to
//! the repository base (empty at the repo root) instead of the absolute parent
//! directory, so the repo-root Markdown module renders `md.` rather than the
//! checkout basename (`md.apg`); every non-root `md.*` module and heading
//! Struct FQN is unchanged.
//!
//! It emits **facts only** — it never computes FQNs and never assembles the
//! graph — and is self-contained: it never shells out and needs nothing on
//! `PATH` at scan time.
//!
//! Stream ids (the scan driver injects one `lang_switch` per spawned stream,
//! and passes the matching `--stream <id>` selector): `md`, `sh`, `yaml`,
//! `json`, `toml`, `xml`, `dockerfile`, `makefile`, `ini`, and the residual
//! `misc`.
//!
//! Usage:
//! `structfrontend <dir> [--stream <id>] [--module <dir>]... [--id-prefix <p>]
//!  [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]`.

mod builder;
mod cli;
mod config;
mod discovery;
mod formats;
mod lines;
mod md;
mod paths;
mod record;

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::cli::{parse_args, Args};
use crate::config::load_structural_scope;
use crate::discovery::{in_scope, read_target_set, stream_for_path, walk};
use crate::formats::dockerfile::emit_dockerfile;
use crate::formats::emit_misc;
use crate::formats::ini::emit_ini;
use crate::formats::json::emit_json;
use crate::formats::makefile::emit_makefile;
use crate::formats::sh::emit_sh;
use crate::formats::toml::emit_toml;
use crate::formats::xml::emit_xml;
use crate::formats::yaml::emit_yaml;
use crate::lines::line_count;
use crate::md::build_doc;
use crate::paths::{absolutize, repo_base, repo_relative_dir};
use crate::record::{write_rec, Rec};

fn run(args: Args) -> io::Result<()> {
    let root = absolutize(&args.root);
    let base = repo_base(&root);
    let scope = load_structural_scope(&base);
    let module_dirs: Vec<PathBuf> = args
        .module_dirs
        .iter()
        .map(|d| absolutize(&root.join(d)))
        .collect();
    let target_set = read_target_set(args.targets_path.as_deref());
    let selected = args.stream.as_deref();

    // The configured structural `code_type` names the structural classification
    // — Markdown keeps `docs`, every other structural stream defaults to
    // `config`. The scanner emits facts only (the ingestor computes `code_type`
    // from the stream id + the same config section), so this read completes the
    // scope section's consumption without emitting a `code_type` field.
    let _structural_code_type = scope.code_type.as_deref().unwrap_or("config");

    let mut files: Vec<(PathBuf, &'static str)> = match &target_set {
        Some(set) => set
            .iter()
            .filter_map(|p| stream_for_path(p).map(|s| (p.clone(), s)))
            .filter(|(p, _)| p.is_file() && p.starts_with(&root))
            .collect(),
        None => {
            let mut out = Vec::new();
            walk(&root, &mut out);
            out
        }
    };
    files.retain(|(p, _)| in_scope(p, &root, &base, &module_dirs, &args.excludes, &scope));
    // The per-stream selector: a spawn emits only its own stream's records.
    if let Some(sel) = selected {
        files.retain(|(_, s)| *s == sel);
    }
    files.sort();
    files.dedup();

    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());

    let mut modules: BTreeSet<String> = BTreeSet::new();
    let mut file_recs: Vec<Rec> = Vec::new();
    let mut struct_recs: Vec<Rec> = Vec::new();
    let mut contains_recs: Vec<Rec> = Vec::new();
    let mut next_id: u64 = 1;

    for (path, stream) in &files {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("warning: could not read {}", path.display());
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let (module, structure) = match *stream {
            // The absorbed Markdown emitter: `build_doc` slugs and nests the
            // heading sections and yields the repo-relative module identity.
            "md" => {
                let doc = build_doc(path, &bytes, &base, &args.id_prefix, &mut next_id);
                let structure = crate::builder::Structure {
                    structs: doc
                        .sections
                        .iter()
                        .map(|s| Rec::Struct {
                            id: s.id.clone(),
                            parent: s.parent.clone(),
                            name: s.name.clone(),
                            path: doc.path.clone(),
                            start: s.start,
                            end: s.end,
                            start_line: s.start_line,
                            end_line: s.end_line,
                        })
                        .collect(),
                    contains: doc
                        .sections
                        .iter()
                        .filter_map(|s| {
                            s.parent_index.map(|pi| Rec::Contains {
                                from: doc.sections[pi].id.clone(),
                                to: s.id.clone(),
                            })
                        })
                        .collect(),
                };
                (doc.dir, structure)
            }
            // The per-format dispatch table (`emit_sh`..`emit_misc`): each
            // returns this file's `Struct`/`contains` facts, and `run` owns the
            // surrounding `Module`/`File` records.
            "sh" => (
                repo_relative_dir(&base, path),
                emit_sh(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "yaml" => (
                repo_relative_dir(&base, path),
                emit_yaml(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "json" => (
                repo_relative_dir(&base, path),
                emit_json(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "toml" => (
                repo_relative_dir(&base, path),
                emit_toml(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "xml" => (
                repo_relative_dir(&base, path),
                emit_xml(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "dockerfile" => (
                repo_relative_dir(&base, path),
                emit_dockerfile(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "makefile" => (
                repo_relative_dir(&base, path),
                emit_makefile(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "ini" => (
                repo_relative_dir(&base, path),
                emit_ini(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            // The residual `misc` stream emits a `File` record only.
            _ => (
                repo_relative_dir(&base, path),
                emit_misc(path, &bytes, &args.id_prefix, &mut next_id),
            ),
        };
        modules.insert(module.clone());
        file_recs.push(Rec::File {
            path: path.to_string_lossy().into_owned(),
            parent: module,
            start_line: 1,
            end_line: line_count(&text),
        });
        struct_recs.extend(structure.structs);
        contains_recs.extend(structure.contains);
    }

    for fqn in &modules {
        write_rec(&mut writer, &Rec::Module { fqn: fqn.clone() })?;
    }
    for rec in &file_recs {
        write_rec(&mut writer, rec)?;
    }
    for rec in &struct_recs {
        write_rec(&mut writer, rec)?;
    }
    for rec in &contains_recs {
        write_rec(&mut writer, rec)?;
    }
    writer.flush()
}

/// Runs the structural frontend against the process arguments (the binary entry
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
        eprintln!("structfrontend: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
