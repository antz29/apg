// apg Python scanner frontend (`pyfrontend`).
//
// Exact-fidelity tier (like the Go/Java/Rust/TypeScript/C# frontends): parses
// and type-checks the project with Astral's `ty` type checker engine (the
// same engine that powers the `ty` CLI and LSP, used by Ruff/Pylance-class
// tooling), so Calls/Uses edges land on the real resolved declaration.
// Anything `ty` cannot resolve statically (dynamic dispatch via `getattr`,
// metaclass magic, etc.) becomes an `unresolved_call` / `unresolved_use` edge
// with a category, never a fabricated FQN — the same safety valve every other
// frontend has.
//
// `ty_ide`/`ty_project` are internal (`publish = false`) crates of the Ruff
// monorepo, pulled straight from the `astral-sh/ruff` repository at a pinned
// release tag (see src/pylib/Cargo.toml), the same pattern as the Rust
// frontend's rust-analyzer pin.
//
// Module model: the identity boundary is the REPO BASE — the nearest
// `.git`-bearing ancestor of the scan root (the git toplevel; a directory in a
// normal checkout, a file in a linked worktree), or the scan root itself when
// the tree is not a git checkout — never the scan root. `<repo>` and
// `<repo>/subdir` scans therefore mint the same identity for the same file
// (`requirements.requirement.portable-graph-identity`,
// `domain.value.module-identity`). Every directory between the base and the
// file contributes a dotted component — an `__init__.py`/`__init__.pyi`-bearing
// regular package and a PEP-420 namespace directory alike — and the base itself
// is the import root and never contributes its own name, so no module is named
// after the checkout/scan-root directory. The frontend emits the DOTTED
// IDENTITY VERBATIM (`pkg`, `pkg.sub`, `foo`) with NO `py.` prefix: the `py.`
// language root is applied INGESTOR-SIDE, so a frontend-baked root would
// double-root. FQNs:
//
//   pkg/sub/__init__.py         -> module `pkg.sub`
//   pkg/sub/mod.py              -> module `pkg.sub.mod`
//   pkg/sub/mod.py class Foo    -> struct `pkg.sub.mod.Foo`
//   pkg/sub/mod.py Foo.method   -> function `pkg.sub.mod.Foo.method`
//
// start/end are 0-based UTF-8 byte offsets (Ruff's native `TextSize`);
// start_line/end_line are 1-based inclusive line numbers.
//
// Discovery (task-3): `.py`/`.pyi` are accepted under the `py` language id
// (`.pyx` is NOT accepted). The frontend's own walk never descends into a
// non-project tree — `__pycache__`, `.venv`, `venv`, `.tox`, `.mypy_cache`,
// `.pytest_cache`, `.ruff_cache`, `site-packages`, `.eggs`, `*.egg-info`,
// `node_modules`, and every hidden dot entry (`.git`/`.hg`/…) — so no file
// under a venv/site-packages tree ever becomes a code node. That same tree is
// still READ as a ty resolution input (task-4).
//
// Environment (task-4): the ty project context is built from FILESYSTEM
// MARKERS only — a uv project (`pyproject.toml`/`uv.lock`), a virtualenv
// (`pyvenv.cfg`), or a bare src root. Detection never probes for a Python
// interpreter and never shells out to `python`/`uv`, so a scan works with NO
// Python runtime present. The excluded site-packages/venv tree is a
// RESOLUTION INPUT ONLY, never a scan root.
//
// Unresolved category precedence (task-6, first match wins): a project symbol
// resolves to a real Calls/Uses edge; a `ty` tool-bundled vendored typeshed
// file is `stdlib`; dependency code (a project `vendor`/`third_party` tree or
// an installed `System`/`SystemVirtual` environment such as a `.venv`
// site-packages) is `external`; in-root unbound/dynamic residue is `unknown`.
// A reference is never guessed into a fabricated FQN or edge.
//
// Incremental contract (task-7, the pinned phase-02 task-9 hand-off):
// `--targets <file>` is a UTF-8 newline-delimited list of absolute source
// paths. It is an EMISSION filter at package/module granularity: every target
// is resolved against the FULL ty project context, but only the target
// modules' per-file facts are emitted. Module records are GLOBAL scaffolding
// (they carry no location) and are emitted verbatim for the whole project so
// the incremental graph equals a full scan; an edge whose target is outside
// the emitted set carries that target's canonical FQN instead of an opaque id,
// which the ingestor's cached-fact splice resolves against the reused unit.
// `--cache-dir`/`--cache-key` locate `<cache-dir>/py/<cache-key>/`: ty
// resolves through an in-process salsa database, so (exactly as the Rust
// frontend does for rust-analyzer) the directory carries no separate on-disk
// compiler cache and creating it keeps the shared store's Python location
// keyed by the global cache key. Cross-scan fact reuse rides the shared
// content-addressed per-file fact store, and a fact unit is reusable only when
// the file's bytes AND its resolution inputs are unchanged.
//
// Usage: pyfrontend <dir> [--module <dir>]... [--id-prefix <p>]
//        [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]

mod environment;
mod exclusions;
mod identity;
mod jsonl;
mod scanner;
mod unresolved;

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::environment::detect_project_kind;
use crate::identity::identities_from_targets;
use crate::scanner::Scanner;

/// Reads the pinned `--targets` list (phase-02 task-9): absolute source paths,
/// one per line, blanks ignored. `None` — an absent flag, an unreadable file or
/// an empty list — means NO emission filter (the byte-identical full scan).
fn read_target_set(path: Option<&str>) -> Option<HashSet<PathBuf>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p = PathBuf::from(l);
            std::fs::canonicalize(&p).unwrap_or(p)
        })
        .collect();
    if set.is_empty() { None } else { Some(set) }
}

/// The pinned per-language native-artifact location for the Python frontend,
/// `<cache-dir>/py/<cache-key>/` (phase-02 task-9 NATIVE-ARTIFACT RULE). ty
/// resolves through an in-process salsa database, so (exactly as the Rust
/// frontend does for rust-analyzer) the directory carries no separate on-disk
/// compiler cache; creating it makes the shared store's Python location exist
/// and keeps it keyed by the global cache key, so a key drift lands in a fresh
/// directory.
fn ensure_artifact_dir(cache_dir: Option<&str>, cache_key: Option<&str>) {
    let Some(dir) = cache_dir else { return };
    let mut p = PathBuf::from(dir);
    p.push("py");
    if let Some(k) = cache_key {
        p.push(k);
    }
    if let Err(e) = std::fs::create_dir_all(&p) {
        eprintln!(
            "warning: could not create py artifact dir {}: {e}",
            p.display()
        );
    }
}

pub struct Args {
    root: PathBuf,
    module_dirs: Vec<PathBuf>,
    excludes: Vec<String>,
    id_prefix: String,
    targets_path: Option<String>,
    cache_dir: Option<String>,
    cache_key: Option<String>,
}

fn usage() -> String {
    "usage: pyfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]"
        .to_string()
}

pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        module_dirs: Vec::new(),
        excludes: Vec::new(),
        id_prefix: "n".to_string(),
        targets_path: None,
        cache_dir: None,
        cache_key: None,
    };
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--module" => match next {
                Some(v) => {
                    let p = if v == "." {
                        args.root.clone()
                    } else {
                        args.root.join(v)
                    };
                    args.module_dirs.push(p);
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
            "--cache-dir" => match next {
                Some(v) => {
                    args.cache_dir = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--cache-key" => match next {
                Some(v) => {
                    args.cache_key = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            // Rust-only flag threaded through by `apg scan` for every
            // frontend command line in a multi-language scan; harmless no-op
            // here.
            "--no-build-scripts" => i += 1,
            other => {
                args.excludes.push(other.to_string());
                i += 1;
            }
        }
    }
    Ok(args)
}

pub fn run(args: Args) -> io::Result<()> {
    let root = std::fs::canonicalize(&args.root).unwrap_or(args.root);
    let module_dirs: Vec<PathBuf> = args
        .module_dirs
        .iter()
        .map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()))
        .collect();
    let target_set = read_target_set(args.targets_path.as_deref());
    ensure_artifact_dir(args.cache_dir.as_deref(), args.cache_key.as_deref());

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    // Resolve the target paths to module identities with the same rule the
    // discovery walk uses (package/module granularity, task-7).
    let mut scanner = Scanner::new(
        root.clone(),
        module_dirs,
        args.excludes,
        args.id_prefix,
        None,
    );
    let target_identities = target_set.as_ref().map(|set| {
        let targets: Vec<PathBuf> = set.iter().cloned().collect();
        identities_from_targets(&targets, |p| scanner.module_fqn_for(p))
    });
    scanner.target_identities = target_identities;

    let kind = detect_project_kind(&root);
    let file_count = scanner.discover_files().len();
    eprintln!(
        "pyfrontend: {kind:?} project at {} ({file_count} Python source files)",
        root.display()
    );

    scanner.run(&mut out);
    out.flush()
}

#[cfg(test)]
mod tests;
