//! End-to-end parse of the vendored NIST CCVS85 corpus (`tests/nist/`). Every program whose
//! file stem starts with NC, IC, IF, IX, OB, RL, SG, SM, SQ or ST is read in place and parsed
//! through `parse_source` under `cobol85`: no error-level diagnostic may remain, and the OB and
//! SG groups must each give at least one obsolete-level warning. Run via
//! `cargo test e2e:: -- --ignored --test-threads=4`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cobolfrontend::diag::Diagnostic;
use cobolfrontend::dialect::Dialect;
use cobolfrontend::parser::parse_source;
use cobolfrontend::preprocess::{read_file, Search};

const PREFIXES: [&str; 10] = ["NC", "IC", "IF", "IX", "OB", "RL", "SG", "SM", "SQ", "ST"];

/// The obsolete-level warning is the only diagnostic that names its level, in its wording
/// (`parser/procedure.rs`, `feature`); every other diagnostic counts as error-level.
fn is_obsolete(d: &Diagnostic) -> bool {
    d.message.ends_with(" is Obsolete under this dialect")
}

/// Whether `path` is a corpus program: a `.CBL`/`.SUB` file (any case) whose stem starts with
/// an in-scope prefix (case-insensitive). `*.log`, `*.DAT` and copybooks never match.
fn in_scope(path: &Path) -> bool {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("CBL") || e.eq_ignore_ascii_case("SUB"));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    ext_ok && PREFIXES.iter().any(|p| stem.starts_with(p))
}

/// Recursively collects the in-scope programs under `dir` (a module folder and its `lib/`).
fn walk(dir: &Path, programs: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("NIST corpus missing at {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("read corpus entry").path();
        if path.is_dir() {
            walk(&path, programs);
        } else if in_scope(&path) {
            programs.push(path);
        }
    }
}

/// The in-scope programs of the ten module folders under `root`, sorted. The copybook folders
/// (`copy/`, `copyalt/`) sit beside the module folders and are never walked. Panics naming
/// `root` when no program is found.
fn corpus(root: &Path) -> Vec<PathBuf> {
    let mut programs = Vec::new();
    for module in PREFIXES {
        walk(&root.join(module), &mut programs);
    }
    programs.sort();
    assert!(
        !programs.is_empty(),
        "no in-scope NIST program (NC/IC/IF/IX/OB/RL/SG/SM/SQ/ST) under {}",
        root.display()
    );
    programs
}

/// The corpus group of a program: its matching prefix.
fn group(path: &Path) -> &'static str {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    PREFIXES
        .iter()
        .find(|p| stem.starts_with(*p))
        .copied()
        .unwrap_or("")
}

/// Parses one program through the in-memory reader, with the corpus dir as the scan root.
fn parse_program(path: &Path, dir: &Path) -> Vec<Diagnostic> {
    let text = read_file(path).unwrap_or_else(|| panic!("cannot read {}", path.display()));
    let search = Search {
        scan_root: dir.to_path_buf(),
        copy_paths: Vec::new(),
    };
    let (_, diags) = parse_source(
        &path.display().to_string(),
        &text,
        Dialect::Cobol85,
        &search,
        &read_file,
    );
    diags
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: reads the vendored NIST CCVS85 corpus; run via cargo test e2e:: -- --ignored"]
    fn nist_programs_parse_without_errors_and_obsolete_groups_warn() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nist");
        let programs = corpus(&dir);
        let mut failures = Vec::new();
        let mut obsolete: BTreeMap<&str, usize> = BTreeMap::new();
        let mut total_errors = 0;
        for path in &programs {
            let diags = parse_program(path, &dir);
            let (warnings, errors): (Vec<_>, Vec<_>) = diags.iter().partition(|d| is_obsolete(d));
            *obsolete.entry(group(path)).or_default() += warnings.len();
            if !errors.is_empty() {
                total_errors += errors.len();
                let first: Vec<String> = errors.iter().take(5).map(|d| d.to_string()).collect();
                failures.push(format!(
                    "{} ({} error-level):\n  {}",
                    path.display(),
                    errors.len(),
                    first.join("\n  ")
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} NIST programs have error-level diagnostics ({total_errors} in total):\n{}",
            failures.len(),
            programs.len(),
            failures.join("\n")
        );
        for name in ["OB", "SG"] {
            assert!(
                obsolete.get(name).copied().unwrap_or(0) > 0,
                "no obsolete-level warning in the {name} group; obsolete counts: {obsolete:?}"
            );
        }
    }
}
