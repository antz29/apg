//! Agreement with GnuCOBOL and the performance budget, over the vendored NIST CCVS85 corpus
//! (`tests/nist/`). Every in-scope program (the same selection as `cobollib_nist_e2e`) is run
//! through `cobc -fsyntax-only -std=cobol85`; each program with a COPY statement must be
//! cobc-accepted. The accepted programs, with the copybook folders, are then scanned by the release
//! `cobolfrontend`, which must report no file with errors. Finally one cobc batch over that
//! accepted tree (which must exit 0) is timed against the best of three release runs over it. Run via
//! `cargo test --test cobollib_cobc_e2e -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT: AtomicU64 = AtomicU64::new(0);

const PREFIXES: [&str; 10] = ["NC", "IC", "IF", "IX", "OB", "RL", "SG", "SM", "SQ", "ST"];

/// Whether `path` is a corpus program: a `.CBL`/`.SUB` file (any case) whose stem starts with an
/// in-scope prefix (case-insensitive). `*.log`, `*.DAT` and copybooks never match.
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

/// The in-scope programs of the ten module folders under `root`, sorted.
fn corpus(root: &Path) -> Vec<PathBuf> {
    let mut programs = Vec::new();
    for module in PREFIXES {
        walk(&root.join(module), &mut programs);
    }
    programs.sort();
    assert!(
        !programs.is_empty(),
        "no in-scope NIST program under {}",
        root.display()
    );
    programs
}

/// Whether the program has a COPY statement: the word COPY on a line that is not a comment line
/// (column 7 is not `*`).
fn has_copy(path: &Path) -> bool {
    let bytes = std::fs::read(path).expect("read program");
    String::from_utf8_lossy(&bytes).lines().any(|line| {
        line.as_bytes().get(6) != Some(&b'*')
            && line
                .split_whitespace()
                .any(|w| w.eq_ignore_ascii_case("COPY"))
    })
}

/// A scratch tree under the platform temp dir (canonical, so paths match), removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cobollib-cobc-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        let root = std::fs::canonicalize(&root).expect("canonicalize scratch root");
        Scratch { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Recursively copies the directory `from` to `to`.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create copy dir");
    for entry in std::fs::read_dir(from).expect("read copy dir") {
        let path = entry.expect("copy entry").path();
        let dest = to.join(path.file_name().expect("file name"));
        if path.is_dir() {
            copy_tree(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).expect("copy file");
        }
    }
}

/// A `cobc -fsyntax-only -std=cobol85` command over the vendored copybook folders, run from `cwd`.
fn cobc_command(cobc: &str, nist: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::new(cobc);
    cmd.args(["-fsyntax-only", "-std=cobol85"])
        .arg("-I")
        .arg(nist.join("copy"))
        .arg("-I")
        .arg(nist.join("copyalt"))
        .current_dir(cwd);
    cmd
}

/// The first cobc error line of a failed run, or a note when cobc printed none.
fn first_error(out: &Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .find(|l| l.contains(": error"))
        .unwrap_or("cobc exited without an error line")
        .to_string()
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: runs GnuCOBOL and the release cobolfrontend over the NIST corpus; run via cargo test-e2e"]
    fn cobc_agreement_and_performance_budget() {
        let nist = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nist");
        let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/cobolfrontend");
        assert!(
            frontend.exists(),
            "release cobolfrontend missing at {}: run `cargo build --release --manifest-path src/cobollib/Cargo.toml`",
            frontend.display()
        );
        let cobc = std::env::var("APG_COBC").unwrap_or_else(|_| "cobc".to_string());
        let runnable = Command::new(&cobc)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !runnable {
            eprintln!("skipped: cobc not runnable as `{cobc}` (set APG_COBC or put cobc on PATH)");
            return;
        }

        // Agreement: each in-scope program through cobc, from a scratch cwd.
        let programs = corpus(&nist);
        let scratch = Scratch::new();
        let mut accepted = Vec::new();
        let mut copy_failures = Vec::new();
        for path in &programs {
            let out = cobc_command(&cobc, &nist, &scratch.root)
                .arg(path)
                .output()
                .expect("spawn cobc");
            if out.status.success() {
                accepted.push(path.clone());
            } else if has_copy(path) {
                copy_failures.push(format!("{}: {}", path.display(), first_error(&out)));
            }
        }
        eprintln!(
            "cobc agreement: {} programs, {} accepted, {} rejected",
            programs.len(),
            accepted.len(),
            programs.len() - accepted.len()
        );
        assert!(
            copy_failures.is_empty(),
            "COPY-using programs rejected by cobc:\n{}",
            copy_failures.join("\n")
        );

        // The release frontend over the accepted programs, with the copybook folders beside them.
        let tree = Scratch::new();
        for dir in ["copy", "copyalt"] {
            copy_tree(&nist.join(dir), &tree.root.join(dir));
        }
        let mut accepted_copies = Vec::new();
        for path in &accepted {
            let rel = path.strip_prefix(&nist).expect("program under the corpus");
            let dest = tree.root.join(rel);
            std::fs::create_dir_all(dest.parent().expect("parent dir")).expect("create dir");
            std::fs::copy(path, &dest).expect("copy accepted program");
            accepted_copies.push(dest);
        }
        let out = Command::new(&frontend)
            .arg(&tree.root)
            .arg("-std=cobol85")
            .output()
            .expect("spawn cobolfrontend");
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let last = stderr.lines().last().unwrap_or_default();
        assert!(out.status.success(), "{stderr}");
        assert_eq!(last, "cobol: 0 files had errors", "{stderr}");

        // Budget: one cobc batch over the accepted tree against the best of three frontend runs over
        // it; a status-0 batch keeps a quickly failing cobc from flattering the ratio.
        let start = Instant::now();
        let batch = cobc_command(&cobc, &nist, &scratch.root)
            .args(&accepted_copies)
            .output()
            .expect("spawn cobc batch");
        let cobc_secs = start.elapsed().as_secs_f64();
        assert!(
            batch.status.success(),
            "cobc batch over the accepted programs failed ({}): {}",
            batch.status,
            first_error(&batch)
        );
        let frontend_secs = (0..3)
            .map(|_| {
                let start = Instant::now();
                let status = Command::new(&frontend)
                    .arg(&tree.root)
                    .arg("-std=cobol85")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .expect("spawn cobolfrontend");
                assert!(
                    status.success(),
                    "cobolfrontend failed over the accepted tree"
                );
                start.elapsed().as_secs_f64()
            })
            .fold(f64::INFINITY, f64::min);
        let ratio = frontend_secs / cobc_secs;
        eprintln!(
            "{} accepted programs: cobc batch {cobc_secs:.2}s; cobolfrontend best-of-3 {frontend_secs:.2}s; ratio {ratio:.2} (budget <= 2.00)",
            accepted.len()
        );
        assert!(
            ratio <= 2.0,
            "cobolfrontend {frontend_secs:.2}s vs cobc {cobc_secs:.2}s: ratio {ratio:.2} exceeds 2.00"
        );
    }
}
