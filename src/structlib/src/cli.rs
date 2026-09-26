//! Command-line parsing for the pinned frontend hand-off.

use std::path::PathBuf;

/// Parsed command line (the pinned frontend hand-off plus `--module` /
/// `--id-prefix` / the per-stream selector). The cache flags are consumed and
/// dropped — there is no native artifact to key.
pub(crate) struct Args {
    pub(crate) root: PathBuf,
    /// Per-stream selector: `Some(id)` emits only that stream's records (how
    /// the scan driver spawns one stream at a time); `None` runs the full
    /// structural walk and emits the union of every stream's records.
    pub(crate) stream: Option<String>,
    pub(crate) module_dirs: Vec<String>,
    pub(crate) id_prefix: String,
    pub(crate) targets_path: Option<String>,
    pub(crate) excludes: Vec<String>,
}

fn usage() -> String {
    "usage: structfrontend <dir> [--stream <id>] [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]"
        .to_string()
}

pub(crate) fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        stream: None,
        module_dirs: Vec::new(),
        id_prefix: "n".to_string(),
        targets_path: None,
        excludes: Vec::new(),
    };
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--stream" => match next {
                Some(v) => {
                    args.stream = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--module" => match next {
                Some(v) => {
                    args.module_dirs.push(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
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
            "--targets" => match next {
                Some(v) => {
                    args.targets_path = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            // Accepted for the pinned hand-off, then ignored (no native cache).
            "--cache-dir" | "--cache-key" => match next {
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
