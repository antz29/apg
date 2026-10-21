//! e2e coverage for the live `apg scan` progress stream: the `Scan running...`
//! start line, the per-node kind symbols, and the closing per-language /
//! per-kind summary — plus the `--no-progress` and terminal gates that silence
//! them.
//!
//! The shared helpers live here at the crate root (never inside the `mod e2e`
//! tier submodule) and reuse the `apg::testutil` harness through `mod common`:
//! the scratch `/tmp` git repo is `common::Repo`, the candidate CLI is
//! `common::apg_bin`, and piped stdout/stderr capture is `common::ApgCommand`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use apg::progress::{KINDS, START_LINE};
use apg::specs::{LAYOUT, TRANS};
use common::{ApgCommand, Repo, apg_bin};

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

/// A scratch `/tmp` git repo with one Go module: the smallest tree that scans a
/// code stream (plus the structural streams) and admits every code kind.
fn go_repo(tag: &str) -> Repo {
    let repo = Repo::new(tag);
    repo.write("go.mod", "module scratch\n\ngo 1.21\n");
    repo.write(
        "main.go",
        "package main\n\nfunc main() {}\n\ntype Widget struct{}\n\nfunc (w Widget) Name() string { return \"w\" }\n",
    );
    repo.commit_all("source");
    repo
}

/// A node record's string property, panicking when the export omits it.
fn field<'a>(record: &'a serde_json::Value, key: &str) -> &'a str {
    record
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("graph record {record} has no `{key}`"))
}

/// The language a code FQN is rooted under: its first `.`-separated component
/// (the `lang_switch` id — `go.scratch`, `misc.`).
fn language_of(fqn: &str) -> &str {
    fqn.split('.').next().unwrap_or(fqn)
}

/// The scanned graph's own per-kind and per-language node counts, read from the
/// `graph.jsonl` export. A File's language comes from the module that contains
/// it (`Module -Contains-> File`); every other code kind is rooted under its
/// language, so its FQN's first component is the language.
fn graph_counts(root: &Path) -> Summary {
    let path = root.join(LAYOUT).join(TRANS).join("graph.jsonl");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("graph.jsonl line {line:?}: {e}"))
        })
        .collect();
    let is = |record: &serde_json::Value, kind: &str| field(record, "type") == kind;
    let modules: BTreeSet<String> = records
        .iter()
        .filter(|r| is(r, "module"))
        .map(|r| field(r, "fqn").to_string())
        .collect();
    let files: BTreeSet<String> = records
        .iter()
        .filter(|r| is(r, "file"))
        .map(|r| field(r, "fqn").to_string())
        .collect();
    let mut file_language: BTreeMap<String, String> = BTreeMap::new();
    for record in records.iter().filter(|r| is(r, "contains")) {
        let (from, to) = (field(record, "from"), field(record, "to"));
        if modules.contains(from) && files.contains(to) {
            file_language.insert(to.to_string(), language_of(from).to_string());
        }
    }
    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    for record in &records {
        let language = match field(record, "type") {
            "language" | "module" | "struct" | "function" => {
                language_of(field(record, "fqn")).to_string()
            }
            "file" => {
                let fqn = field(record, "fqn");
                file_language
                    .get(fqn)
                    .unwrap_or_else(|| panic!("graph File {fqn} has no module parent"))
                    .clone()
            }
            _ => continue,
        };
        *languages.entry(language).or_default() += 1;
    }
    Summary {
        kinds: KINDS
            .iter()
            .map(|(_, label)| {
                let kind = label.to_ascii_lowercase();
                let count = records.iter().filter(|r| is(r, &kind)).count();
                (label.to_string(), count)
            })
            .collect(),
        languages: languages.into_iter().collect(),
    }
}

/// e2e tier -- real I/O: these tests build scratch `/tmp` git repos, spawn the
/// candidate `apg` CLI (through a pty for the terminal-gated cases), and read
/// the scan's `apg-frontend.log`. Each is `#[ignore]`d, so a plain `cargo test`
/// never runs one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test e2e:: -- --ignored --test-threads=4`).
mod e2e {
    use super::*;

    /// The terminal gate: `Command::output()` pipes the child's stderr, so
    /// progress stays off and the run carries its ordinary log lines alone.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
    fn piped_stderr_carries_no_progress() {
        let repo = go_repo("progress-piped");
        let out = ApgCommand::new(&["scan", "."]).cwd(&repo.root).output();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "piped scan failed: {stderr}");
        assert!(
            stderr.contains("Project: "),
            "ordinary log lines must remain: {stderr}"
        );
        assert!(
            !stderr.contains(START_LINE),
            "piped stderr must carry no start line: {stderr}"
        );
        for (symbol, label) in KINDS {
            assert!(
                !stderr.contains(symbol),
                "piped stderr must carry no {label} symbol: {stderr}"
            );
        }
        assert!(
            summary_line(&stderr).is_none(),
            "piped stderr must carry no summary: {stderr}"
        );
        // The silence is the terminal gate, not an empty run.
        assert!(
            graph_counts(&repo.root).kind("File") > 0,
            "the piped scan must build a graph"
        );
        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// The progress stream reaches stderr alone: a pty run (gate on) streams
    /// its start line and symbols, yet none of them land in the run log.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary through a pty); run via cargo test-e2e"]
    fn pty_scan_keeps_progress_out_of_the_frontend_log() {
        let repo = go_repo("progress-log");
        let out = pty_output(&repo.root, &["scan", "."]);
        let stream = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "pty scan failed: {stream}");
        assert!(
            stream.contains(START_LINE),
            "the pty run must stream progress: {stream}"
        );
        let log_path = repo.root.join(LAYOUT).join(TRANS).join("apg-frontend.log");
        let log = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", log_path.display()));
        assert!(
            log.contains("Project: "),
            "the log must still carry the ordinary run lines: {log}"
        );
        assert!(
            !log.contains(START_LINE),
            "no progress line may reach apg-frontend.log: {log}"
        );
        for (symbol, label) in KINDS {
            assert!(
                !log.contains(symbol),
                "no {label} progress character may reach apg-frontend.log: {log}"
            );
        }
        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// `--no-progress` is accepted and silences the whole stream while the run
    /// still logs and still scans.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary through a pty); run via cargo test-e2e"]
    fn no_progress_flag_is_accepted_and_silent() {
        let repo = go_repo("progress-flag");
        let out = pty_output(&repo.root, &["scan", ".", "--no-progress"]);
        let stream = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "`--no-progress` run failed: {stream}");
        assert!(
            stream.contains("Project: "),
            "the flag silences progress, not the log: {stream}"
        );
        assert!(
            !stream.contains(START_LINE),
            "`--no-progress` must print no start line: {stream}"
        );
        for (symbol, label) in KINDS {
            assert!(
                !stream.contains(symbol),
                "`--no-progress` must print no {label} symbol: {stream}"
            );
        }
        assert!(
            summary_line(&stream).is_none(),
            "`--no-progress` must print no summary: {stream}"
        );
        assert!(
            graph_counts(&repo.root).kind("File") > 0,
            "`--no-progress` must still scan"
        );
        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// The Win-A freshness fast path admits no node, so it streams neither the
    /// start line nor a closing summary.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary through a pty); run via cargo test-e2e"]
    fn fresh_fast_path_is_progress_silent() {
        let repo = go_repo("progress-fresh");
        let first = ApgCommand::new(&["scan", "."]).cwd(&repo.root).output();
        let first_stderr = String::from_utf8_lossy(&first.stderr).into_owned();
        assert!(first.status.success(), "cold scan failed: {first_stderr}");
        let out = pty_output(&repo.root, &["scan", "."]);
        let stream = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "fast-path run failed: {stream}");
        assert!(
            stream.contains("fast-path"),
            "the unchanged tree must take the fast path: {stream}"
        );
        assert!(
            !stream.contains("[scan] running"),
            "the fast path must spawn no frontend: {stream}"
        );
        assert!(
            !stream.contains(START_LINE),
            "the fast path must print no start line: {stream}"
        );
        for (symbol, label) in KINDS {
            assert!(
                !stream.contains(symbol),
                "the fast path must print no {label} symbol: {stream}"
            );
        }
        assert!(
            summary_line(&stream).is_none(),
            "the fast path must print no summary: {stream}"
        );
        let _ = std::fs::remove_dir_all(&repo.root);
    }

    /// The positive TTY case: a pty run of a changed tree (nothing recorded
    /// yet, so the freshness fast path cannot fire) streams the start line
    /// before the first symbol, one symbol per admitted node, and a closing
    /// summary whose per-language and per-kind counts agree with the graph.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary through a pty); run via cargo test-e2e"]
    fn pty_scan_streams_one_symbol_per_node_and_a_matching_summary() {
        let repo = go_repo("progress-tty");
        let out = pty_output(&repo.root, &["scan", "."]);
        let stream = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "pty scan failed: {stream}");

        let summary = parse_summary(&stream);
        let line = summary_line(&stream).expect("summary line");
        let streamed = &stream[..stream.find(line).expect("summary offset")];
        let start = streamed
            .find(START_LINE)
            .unwrap_or_else(|| panic!("no start line: {stream}"));
        let first_symbol = KINDS
            .iter()
            .filter_map(|(symbol, _)| streamed.find(symbol))
            .min()
            .unwrap_or_else(|| panic!("no node symbol: {stream}"));
        assert!(
            start < first_symbol,
            "the start line must precede the first symbol: {stream}"
        );

        let graph = graph_counts(&repo.root);
        for (symbol, label) in KINDS {
            assert_eq!(
                streamed.matches(symbol).count(),
                graph.kind(label),
                "one {label} symbol per {label} node: {stream}"
            );
            assert_eq!(
                summary.kind(label),
                graph.kind(label),
                "the summary's {label} count must agree with the graph: {stream}"
            );
        }
        for (language, count) in &graph.languages {
            assert_eq!(
                summary.language(language),
                *count,
                "the summary's {language} count must agree with the graph: {stream}"
            );
        }
        let graph_languages: BTreeMap<&str, usize> = graph
            .languages
            .iter()
            .map(|(language, count)| (language.as_str(), *count))
            .collect();
        let summary_languages: BTreeMap<&str, usize> = summary
            .languages
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(language, count)| (language.as_str(), *count))
            .collect();
        assert_eq!(
            summary_languages, graph_languages,
            "the summary's per-language counts must agree with the graph: {stream}"
        );
        let _ = std::fs::remove_dir_all(&repo.root);
    }
}
