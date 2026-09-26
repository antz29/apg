//! The scan run log (`apg-frontend.log` + stderr) and the first-class timing
//! report emitter.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — the same messages, in the
//! same order, to the same destinations.

use std::io::Write;
use std::path::Path;

use crate::timing;

/// Mirrors every run message to stderr *and* to `apg-frontend.log`, so the log
/// is a complete high-resolution record of the run (the frontend's stderr is
/// also redirected there, so one file has the whole pipeline).
pub struct Log {
    f: std::fs::File,
}

impl Log {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Log {
        Log {
            f: std::fs::File::create("apg-frontend.log")
                .expect("failed to create apg-frontend.log"),
        }
    }

    pub fn ln(&mut self, msg: &str) {
        eprintln!("{msg}");
        let _ = writeln!(self.f, "{msg}");
    }

    /// Appends a spooled file's contents to the log only (no terminal echo),
    /// used to fold a frontend's captured stderr into `apg-frontend.log`.
    pub fn append_file(&mut self, path: &Path) {
        if let Ok(content) = std::fs::read_to_string(path) {
            let _ = write!(self.f, "{content}");
        }
    }
}

/// Emits the per-phase timing report first-class (phase-04 task-1/task-2): the
/// human `[timing]` line plus the machine-readable `[timing-json]` line, both
/// through the scan log (stderr *and* `apg-frontend.log`).
pub fn emit_timing(log: &mut Log, report: &timing::TimingReport) {
    log.ln(&report.human_line());
    log.ln(&report.machine_line());
}

/// Last `n` non-empty-ish lines of a file, oldest first (a short tail for
/// reporting why a frontend failed).
pub fn tail_of(path: &Path, n: usize) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}
