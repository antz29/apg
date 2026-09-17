//! apg Markdown frontend (`mdfrontend`).
//!
//! Discovers `.md`/`.markdown` documents and streams the unified JSONL facts
//! (SPEC §2) the Rust ingestor consumes: one `Module` per directory, one `File`
//! per document, one `Struct` section per heading, plus the `contains` edges
//! that nest a section under its parent section (`File -> Struct` containment
//! is derived ingestor-side from every located unit's `path`, so a top-level
//! section needs no explicit edge — the same model as the other frontends).
//! It emits **facts only**: it never computes FQNs and never assembles the
//! graph.
//!
//! Identity (phase-07 task-2):
//! * a Module's FQN is its **directory's absolute path verbatim** — the `md.`
//!   language root is applied ingestor-side (P9), and a frontend-baked prefix
//!   would double-root;
//! * a File's FQN is its absolute path, extension retained;
//! * a section Struct's FQN extends its parent (`<parent>.<slug>`), the parent
//!   being the File for a top-level heading and the enclosing section
//!   otherwise.
//!
//! Slug rule: `requirements.constraint.markdown-slug-rule` (GitHub-style). The
//! normalization + assigned-segment dedup is factored into the pure seam
//! [`slug_segments`] so a section's segment can be asserted directly, with no
//! FQN rendering involved (phase-07 task-3/-12).
//!
//! Usage:
//! `mdfrontend <dir> [--module <dir>]... [--id-prefix <p>] [--targets <file>]
//!  [--cache-dir <dir>] [--cache-key <key>] [exclude...]`.
//!
//! Win-B (`--targets`, the pinned phase-02 task-9 hand-off): a UTF-8,
//! newline-delimited file of absolute source paths. It is an EMISSION filter
//! only — markdown facts are per-file, so each target file is resolved against
//! itself (nothing is reduced) and only target `.md`/`.markdown` files are
//! emitted. `--cache-dir`/`--cache-key` are accepted but unused: markdown has
//! no native incremental artifact, so it reuses through the shared
//! content-addressed per-file fact cache (phase-07 task-4).

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// Directories never scanned (`domain.entity.scan-exclusion`, phase-07
/// task-8), on top of the always-pruned hidden dot entries. Generated trees
/// (`gen`, `generated`, `dist`, `build`, `out`) are deliberately NOT here:
/// every accepted file stays in the graph and is filtered by `code_type`.
const EXCLUDED_DIRS: &[&str] = &["target", "vendor", "node_modules", ".worktrees"];

/// Unified JSONL records (SPEC §2). `id` is a scanner-local opaque counter; the
/// ingestor maps ids to canonical FQNs and renders `parent.name`.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: u32,
        end_line: u32,
    },
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },
    Contains {
        from: String,
        to: String,
    },
}

/// Parsed command line (the pinned phase-02 hand-off plus `--module` /
/// `--id-prefix`). The cache flags are consumed and dropped — there is no
/// native artifact to key.
struct Args {
    root: PathBuf,
    module_dirs: Vec<String>,
    id_prefix: String,
    targets_path: Option<String>,
    excludes: Vec<String>,
}

fn usage() -> String {
    "usage: mdfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]"
        .to_string()
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        module_dirs: Vec::new(),
        id_prefix: "n".to_string(),
        targets_path: None,
        excludes: Vec::new(),
    };
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
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

/// Lexical-ish absolute path: `canonicalize` when it exists (so a symlinked
/// scan root still matches the target list), else the path made absolute.
fn absolutize(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

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
        .map(|l| absolutize(Path::new(l)))
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

fn is_pruned_dir(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIRS.contains(&name)
}

/// True when `path` carries a discovery-excluded directory component BELOW
/// `root` (so a scan root that itself lives under `.worktrees/` is still
/// scanned, while a `target/`, `vendor/`, `node_modules/` or nested
/// `.worktrees/` tree inside it is not).
fn under_excluded_tree(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| match c {
        std::path::Component::Normal(s) => {
            let s = s.to_string_lossy();
            is_pruned_dir(&s)
        }
        _ => false,
    })
}

fn in_scope(path: &Path, root: &Path, module_dirs: &[PathBuf], excludes: &[String]) -> bool {
    if under_excluded_tree(path, root) {
        return false;
    }
    if !module_dirs.is_empty() && !module_dirs.iter().any(|m| path.starts_with(m)) {
        return false;
    }
    let s = path.to_string_lossy();
    !excludes.iter().any(|x| s.contains(x.as_str()))
}

/// Every markdown file at or below `dir` (a full-scan discovery walk).
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if is_pruned_dir(&entry.file_name().to_string_lossy()) {
                continue;
            }
            walk(&path, out);
        } else if ft.is_file() && is_markdown(&path) {
            out.push(path);
        }
    }
}

/// One ATX heading found in a document.
struct RawHeading {
    level: u8,
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
    text: String,
}

/// A section symbol: one `Struct` record plus the nesting needed for its
/// `contains` edge.
struct Section {
    level: u8,
    id: String,
    /// The parent FQN for the record (the File path, or the parent section's).
    parent: String,
    /// `parent.name` — the section's rendered FQN (used to parent its children).
    fqn: String,
    name: String,
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
    parent_index: Option<usize>,
}

/// A document's emitted facts.
struct Doc {
    path: String,
    dir: String,
    end_line: u32,
    sections: Vec<Section>,
}

/// A heading whose trailing closing `#`s are stripped (CommonMark: a trailing
/// run of `#` preceded by whitespace is an optional closing sequence).
fn strip_closing_hashes(s: &str) -> &str {
    let t = s.trim_end();
    let run = t.bytes().rev().take_while(|&b| b == b'#').count();
    if run == 0 {
        return t;
    }
    let before = &t[..t.len() - run];
    if before.is_empty() || before.ends_with(' ') || before.ends_with('\t') {
        before.trim_end()
    } else {
        t
    }
}

/// ATX headings (`#`..`######`, up to 3 leading spaces, space-or-EOL after the
/// run), skipping fenced code blocks so a `#` comment in a fence is not a
/// section. Byte ranges exclude the line terminator; line numbers are 1-based.
fn parse_headings(text: &str) -> Vec<RawHeading> {
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    let mut line_no: u32 = 1;
    let mut fence: Option<(u8, usize)> = None;
    for raw in text.split_inclusive('\n') {
        let without_nl = raw.strip_suffix('\n').unwrap_or(raw);
        let line = without_nl.strip_suffix('\r').unwrap_or(without_nl);
        let line_start = offset;
        let line_end = offset + line.len() as u32;
        offset += raw.len() as u32;

        let lead = line.bytes().take_while(|&b| b == b' ').count();
        let rest = if lead <= 3 { &line[lead..] } else { "" };

        if lead <= 3 {
            if let Some((fch, flen)) = fence {
                // A closing fence is the same char, >= the opening length, with
                // only whitespace after it.
                let run = rest.bytes().take_while(|&b| b == fch).count();
                if run >= flen && rest[run..].trim().is_empty() {
                    fence = None;
                    line_no += 1;
                    continue;
                }
            } else if let Some(fch) = rest.bytes().next().filter(|b| *b == b'`' || *b == b'~') {
                let run = rest.bytes().take_while(|&b| b == fch).count();
                if run >= 3 {
                    fence = Some((fch, run));
                    line_no += 1;
                    continue;
                }
            }
        }
        if fence.is_some() {
            line_no += 1;
            continue;
        }

        if lead <= 3 {
            let hashes = rest.bytes().take_while(|&b| b == b'#').count();
            if (1..=6).contains(&hashes) {
                let after = &rest[hashes..];
                if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
                    out.push(RawHeading {
                        level: hashes as u8,
                        start: line_start,
                        end: line_end,
                        start_line: line_no,
                        end_line: line_no,
                        text: strip_closing_hashes(after.trim()).to_string(),
                    });
                }
            }
        }
        line_no += 1;
    }
    out
}

/// Collapses runs of `-` to one and trims leading/trailing `-`.
fn collapse_and_trim(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch == '-' {
            if out.is_empty() || prev_dash {
                prev_dash = true;
                continue;
            }
            out.push('-');
            prev_dash = true;
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A heading's RAW bare slug: trim, NFC, lowercase, whitespace runs -> `-`,
/// drop punctuation except `-`/`_`, preserve non-ASCII letters/digits, then
/// collapse + trim `-`. Empty normalizations fall back to the pinned literal
/// `section` (an ordinary bare slug that participates in dedup).
fn bare_slug(heading: &str) -> String {
    let nfc: String = heading.trim().nfc().collect();
    let lower = nfc.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut pending_space = false;
    for ch in lower.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if !(ch.is_alphanumeric() || ch == '-' || ch == '_') {
            continue;
        }
        if pending_space {
            out.push('-');
            pending_space = false;
        }
        out.push(ch);
    }
    let collapsed = collapse_and_trim(&out);
    if collapsed.is_empty() {
        "section".to_string()
    } else {
        collapsed
    }
}

/// Splits a raw bare slug ending in `-<digits>` into (`stem-`, n); `None`
/// otherwise. The candidate sequence evolves the slug's OWN trailing number.
fn split_trailing_number(bare: &str) -> Option<(&str, u64)> {
    let dash = bare.rfind('-')?;
    let digits = &bare[dash + 1..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    Some((&bare[..=dash], n))
}

/// The first free candidate in the raw slug's candidate sequence, inserted into
/// `assigned`. Injective within a document by construction.
fn assign_segment(bare: &str, assigned: &mut HashSet<String>) -> String {
    match split_trailing_number(bare) {
        Some((stem, n)) => {
            let mut k = n;
            loop {
                let candidate = format!("{stem}{k}");
                if assigned.insert(candidate.clone()) {
                    return candidate;
                }
                k += 1;
            }
        }
        None => {
            let mut k: u64 = 0;
            loop {
                let candidate = if k == 0 {
                    bare.to_string()
                } else {
                    format!("{bare}-{k}")
                };
                if assigned.insert(candidate.clone()) {
                    return candidate;
                }
                k += 1;
            }
        }
    }
}

/// The PURE seam: heading texts -> assigned FQN segments, in document order,
/// independent of FQN rendering. `requirements.constraint.markdown-slug-rule`.
pub fn slug_segments(headings: &[&str]) -> Vec<String> {
    let mut assigned: HashSet<String> = HashSet::new();
    headings
        .iter()
        .map(|h| assign_segment(&bare_slug(h), &mut assigned))
        .collect()
}

/// Mirrors the other frontends' file line count: `"a\nb\n"` -> 2, `""` -> 1.
fn line_count(text: &str) -> u32 {
    if text.is_empty() {
        return 1;
    }
    let n = text.bytes().filter(|b| *b == b'\n').count() as u32;
    if text.ends_with('\n') {
        n
    } else {
        n + 1
    }
}

/// Builds one document's facts: slug the headings, nest by heading level (the
/// nearest preceding lower-level heading, else the File), and assign opaque ids.
fn build_doc(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Doc {
    let text = String::from_utf8_lossy(bytes);
    let headings = parse_headings(&text);
    let texts: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
    let slugs = slug_segments(&texts);
    let path_str = path.to_string_lossy().into_owned();
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut sections: Vec<Section> = Vec::with_capacity(headings.len());
    let mut stack: Vec<usize> = Vec::new();
    for (h, slug) in headings.into_iter().zip(slugs) {
        while let Some(&top) = stack.last() {
            if sections[top].level >= h.level {
                stack.pop();
            } else {
                break;
            }
        }
        let parent_index = stack.last().copied();
        let parent = match parent_index {
            Some(pi) => sections[pi].fqn.clone(),
            None => path_str.clone(),
        };
        let fqn = format!("{parent}.{slug}");
        let n = *next_id;
        *next_id += 1;
        sections.push(Section {
            level: h.level,
            id: format!("{id_prefix}{n}"),
            parent,
            fqn,
            name: slug,
            start: h.start,
            end: h.end,
            start_line: h.start_line,
            end_line: h.end_line,
            parent_index,
        });
        stack.push(sections.len() - 1);
    }
    Doc {
        path: path_str,
        dir,
        end_line: line_count(&text),
        sections,
    }
}

fn write_rec<W: Write>(w: &mut W, rec: &Rec) -> io::Result<()> {
    let line = serde_json::to_string(rec).expect("serialize record");
    writeln!(w, "{line}")
}

fn run(args: Args) -> io::Result<()> {
    let root = absolutize(&args.root);
    let module_dirs: Vec<PathBuf> = args
        .module_dirs
        .iter()
        .map(|d| absolutize(&root.join(d)))
        .collect();
    let target_set = read_target_set(args.targets_path.as_deref());

    let mut files: Vec<PathBuf> = match &target_set {
        Some(set) => set
            .iter()
            .filter(|p| is_markdown(p) && p.is_file() && p.starts_with(&root))
            .cloned()
            .collect(),
        None => {
            let mut out = Vec::new();
            walk(&root, &mut out);
            out
        }
    };
    files.retain(|p| in_scope(p, &root, &module_dirs, &args.excludes));
    files.sort();
    files.dedup();

    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());

    let mut modules: BTreeSet<String> = BTreeSet::new();
    let mut docs: Vec<Doc> = Vec::new();
    let mut next_id: u64 = 1;
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("warning: could not read {}", path.display());
            continue;
        };
        let doc = build_doc(path, &bytes, &args.id_prefix, &mut next_id);
        modules.insert(doc.dir.clone());
        docs.push(doc);
    }

    for fqn in &modules {
        write_rec(&mut writer, &Rec::Module { fqn: fqn.clone() })?;
    }
    for doc in &docs {
        write_rec(
            &mut writer,
            &Rec::File {
                path: doc.path.clone(),
                parent: doc.dir.clone(),
                start_line: 1,
                end_line: doc.end_line,
            },
        )?;
    }
    for doc in &docs {
        for s in &doc.sections {
            write_rec(
                &mut writer,
                &Rec::Struct {
                    id: s.id.clone(),
                    parent: s.parent.clone(),
                    name: s.name.clone(),
                    path: doc.path.clone(),
                    start: s.start,
                    end: s.end,
                    start_line: s.start_line,
                    end_line: s.end_line,
                },
            )?;
        }
    }
    for doc in &docs {
        for s in &doc.sections {
            if let Some(pi) = s.parent_index {
                write_rec(
                    &mut writer,
                    &Rec::Contains {
                        from: doc.sections[pi].id.clone(),
                        to: s.id.clone(),
                    },
                )?;
            }
        }
    }
    writer.flush()
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("mdfrontend: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod unit {
        use super::*;

        #[test]
        fn normalization_trims_lowercases_whitespace_and_drops_punctuation() {
            assert_eq!(slug_segments(&["  Hello, World!  "]), vec!["hello-world"]);
            assert_eq!(slug_segments(&["a  b"]), vec!["a-b"]);
            // `.`/`/`/`:` are dropped, never kept as separators.
            assert_eq!(slug_segments(&["a.b/c: d"]), vec!["abc-d"]);
            // `-` and `_` survive.
            assert_eq!(slug_segments(&["snake_case-name"]), vec!["snake_case-name"]);
        }

        #[test]
        fn normalization_preserves_non_ascii_and_applies_nfc() {
            assert_eq!(slug_segments(&["Über Straße"]), vec!["über-straße"]);
            // "Cafe" + U+0301 COMBINING ACUTE composes to "Café" under NFC.
            assert_eq!(slug_segments(&["Cafe\u{0301}"]), vec!["café"]);
        }

        #[test]
        fn hyphens_collapse_and_trim() {
            assert_eq!(slug_segments(&["a - b"]), vec!["a-b"]);
            assert_eq!(slug_segments(&["-x-"]), vec!["x"]);
            assert_eq!(slug_segments(&["a---b"]), vec!["a-b"]);
            // An all-hyphens heading normalizes to empty.
            assert_eq!(slug_segments(&["---"]), vec!["section"]);
        }

        #[test]
        fn empty_normalization_falls_back_to_section() {
            assert_eq!(slug_segments(&["   "]), vec!["section"]);
            assert_eq!(slug_segments(&["!!!"]), vec!["section"]);
            assert_eq!(
                slug_segments(&["", "", ""]),
                vec!["section", "section-1", "section-2"]
            );
        }

        #[test]
        fn duplicate_headings_dedup_over_the_raw_slug_space() {
            assert_eq!(
                slug_segments(&["Overview", "Overview"]),
                vec!["overview", "overview-1"]
            );
            // The third heading's RAW `overview-1` is taken, so its trailing
            // number is incremented: overview, overview-1, overview-2.
            assert_eq!(
                slug_segments(&["Overview", "Overview", "Overview 1"]),
                vec!["overview", "overview-1", "overview-2"]
            );
            assert_eq!(
                slug_segments(&["section", "section-1"]),
                vec!["section", "section-1"]
            );
        }

        #[test]
        fn atx_headings_strip_closing_hashes_and_skip_code_fences() {
            let md = "# Title ##\n\nsome text\n\n```\n# not a heading\n```\n\n## Real\n";
            let headings = parse_headings(md);
            let texts: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
            assert_eq!(texts, vec!["Title", "Real"]);
            assert_eq!(headings[0].level, 1);
            assert_eq!(headings[1].level, 2);
            assert_eq!(headings[0].start_line, 1);
            assert_eq!(headings[1].start_line, 9);
        }

        #[test]
        fn build_doc_nests_sections_and_extends_the_parent_fqn() {
            let md = "# Title\n## Child\n### Grandchild\n## Sibling\n";
            let mut next_id = 1;
            let doc = build_doc(
                Path::new("/root/docs/a.md"),
                md.as_bytes(),
                "n",
                &mut next_id,
            );
            let names: Vec<&str> = doc.sections.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, vec!["title", "child", "grandchild", "sibling"]);
            assert_eq!(doc.sections[0].parent, "/root/docs/a.md");
            assert_eq!(doc.sections[0].parent_index, None);
            assert_eq!(doc.sections[1].parent, "/root/docs/a.md.title");
            assert_eq!(doc.sections[2].parent, "/root/docs/a.md.title.child");
            // The second `##` is a sibling of the first, so it hangs off `# Title`.
            assert_eq!(doc.sections[3].parent, "/root/docs/a.md.title");
            assert_eq!(doc.sections[3].parent_index, Some(0));
            assert_eq!(
                doc.sections[2].fqn,
                "/root/docs/a.md.title.child.grandchild"
            );
            assert_eq!(doc.end_line, 4);
        }

        #[test]
        fn build_doc_dedups_duplicate_headings_per_document() {
            let md = "# Overview\n## Overview\n## Overview 1\n";
            let mut next_id = 1;
            let doc = build_doc(
                Path::new("/root/docs/a.md"),
                md.as_bytes(),
                "n",
                &mut next_id,
            );
            let names: Vec<&str> = doc.sections.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, vec!["overview", "overview-1", "overview-2"]);
        }

        #[test]
        fn assigned_segments_are_pairwise_distinct() {
            let headings = [
                "Overview",
                "Overview",
                "Overview 1",
                "section",
                "section-1",
                "a - b",
                "-x-",
                "!!!",
                "Cafe\u{0301}",
                "Cafe\u{0301}",
            ];
            let assigned = slug_segments(&headings);
            let unique: HashSet<&String> = assigned.iter().collect();
            assert_eq!(
                unique.len(),
                assigned.len(),
                "heading -> segment must be injective, got {assigned:?}"
            );
        }
    }
}
