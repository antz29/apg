//! e2e coverage for the live `apg scan` progress stream: the `Scan running...`
//! start line, the per-node kind symbols, and the closing per-language /
//! per-kind summary — plus the `--no-progress` and terminal gates that silence
//! them.
//!
//! The shared helpers live here at the crate root (never inside the `mod e2e`
//! tier submodule) and reuse the `apg::testutil` harness through `mod common`:
//! the scratch `/tmp` git repo is `common::Repo`, the candidate CLI is
//! `common::apg_bin`, and piped stdout/stderr capture is `common::ApgCommand`.

#![allow(dead_code)] // the `mod e2e` test bodies that consume these helpers land in task-4

mod common;

use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::apg_bin;

// ---------------------------------------------------------------------------
// Progress-stream helpers, shared by the whole crate.
// ---------------------------------------------------------------------------

/// Runs `apg <args>` in `dir` on a pty and returns the captured [`Output`]:
/// `script -q /dev/null <apg> <args…>`, the BSD/darwin `script` form that
/// takes the command as trailing operands (util-linux spells the same run
/// `script -q -c '<cmd>' /dev/null`).
///
/// The pty is what makes the child's stderr a terminal, so the
/// terminal-gated progress stream turns on — `Command::output()` pipes stderr
/// and leaves it off. `script` merges the child's stdout and stderr onto its
/// own stdout, so the captured progress stream is `output.stdout`.
fn pty_output(dir: &Path, args: &[&str]) -> Output {
    Command::new("script")
        .arg("-q")
        .arg("/dev/null")
        .arg(apg_bin())
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to run `script -q /dev/null apg {args:?}`: {e}"))
}

/// The closing progress summary split into its per-kind counts (the legend
/// before ` | `) and its per-language counts (the section after it, empty when
/// the run recorded no language). Entries keep scan order.
#[derive(Debug, Default, PartialEq)]
struct Summary {
    kinds: Vec<(String, usize)>,
    languages: Vec<(String, usize)>,
}

impl Summary {
    /// The count tallied for `kind` (a legend label such as `Function`), or 0
    /// when the summary omitted it.
    fn kind(&self, kind: &str) -> usize {
        count(&self.kinds, kind)
    }

    /// The node count tallied for `language`, or 0 when the summary omitted it.
    fn language(&self, language: &str) -> usize {
        count(&self.languages, language)
    }
}

fn count(entries: &[(String, usize)], name: &str) -> usize {
    entries
        .iter()
        .find(|(entry, _)| entry == name)
        .map_or(0, |(_, count)| *count)
}

/// The closing summary line of a captured progress stream, `None` when the run
/// printed none (progress gated off, or the fresh fast path that admits no
/// node). `Progress::summary` writes the line starting with the first legend
/// entry, so that prefix identifies it.
fn summary_line(output: &str) -> Option<&str> {
    output
        .lines()
        .rev()
        .find(|line| line.starts_with("🌍 Language:"))
}

/// Parses the closing summary line out of a captured progress stream: the
/// per-kind counts before ` | ` and the per-language counts after it, each a
/// `name: count` entry. Panics when the stream carries no summary line.
fn parse_summary(output: &str) -> Summary {
    let line = summary_line(output)
        .unwrap_or_else(|| panic!("no progress summary line in captured output {output:?}"));
    let (kinds, languages) = line.split_once(" | ").unwrap_or((line, ""));
    Summary {
        kinds: parse_counts(kinds),
        languages: parse_counts(languages),
    }
}

/// Parses one summary section's `, `-separated `name: count` entries, keeping
/// the label alone: a kind entry's legend symbol (`🌍 Language`) is dropped so
/// callers look up `Language`.
fn parse_counts(section: &str) -> Vec<(String, usize)> {
    section
        .split(", ")
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (name, count) = entry
                .rsplit_once(": ")
                .unwrap_or_else(|| panic!("summary entry {entry:?} is not `name: count`"));
            let label = name.split_whitespace().last().unwrap_or(name);
            let count = count
                .parse()
                .unwrap_or_else(|e| panic!("summary entry {entry:?} has a bad count: {e}"));
            (label.to_string(), count)
        })
        .collect()
}

/// e2e tier -- real I/O: these tests build scratch `/tmp` git repos, spawn the
/// candidate `apg` CLI (through a pty for the terminal-gated cases), and read
/// the scan's `apg-frontend.log`. Each is `#[ignore]`d, so a plain `cargo test`
/// never runs one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test e2e:: -- --ignored --test-threads=4`).
mod e2e {
    // The `use super::*;` header and the `#[ignore]`d test bodies land here in
    // phase-03 task-4.
}
