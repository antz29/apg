//! The absorbed Markdown (`md` stream) heading-Struct mechanism.

use std::collections::HashSet;
use std::path::Path;

use unicode_normalization::UnicodeNormalization;

use crate::paths::repo_relative_dir;

/// One ATX heading found in a document.
pub(crate) struct RawHeading {
    pub(crate) level: u8,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    pub(crate) text: String,
}

/// A section symbol: one `Struct` record plus the nesting needed for its
/// `contains` edge.
pub(crate) struct Section {
    pub(crate) level: u8,
    pub(crate) id: String,
    /// The parent FQN for the record (the File path, or the parent section's).
    pub(crate) parent: String,
    /// `parent.name` — the section's rendered FQN (used to parent its children).
    pub(crate) fqn: String,
    pub(crate) name: String,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    pub(crate) parent_index: Option<usize>,
}

/// A document's emitted facts.
pub(crate) struct Doc {
    pub(crate) path: String,
    /// The repo-relative module identity of the document's directory (empty at
    /// the repository base).
    pub(crate) dir: String,
    pub(crate) sections: Vec<Section>,
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
pub(crate) fn parse_headings(text: &str) -> Vec<RawHeading> {
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
/// independent of FQN rendering (`requirements.constraint.markdown-slug-rule`).
fn slug_segments(headings: &[&str]) -> Vec<String> {
    let mut assigned: HashSet<String> = HashSet::new();
    headings
        .iter()
        .map(|h| assign_segment(&bare_slug(h), &mut assigned))
        .collect()
}

/// Builds one document's facts: slug the headings, nest by heading level (the
/// nearest preceding lower-level heading, else the File), assign opaque ids,
/// and render the module identity repo-relative to `base` (empty at the base).
pub(crate) fn build_doc(
    path: &Path,
    bytes: &[u8],
    base: &Path,
    id_prefix: &str,
    next_id: &mut u64,
) -> Doc {
    let text = String::from_utf8_lossy(bytes);
    let headings = parse_headings(&text);
    let texts: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
    let slugs = slug_segments(&texts);
    let path_str = path.to_string_lossy().into_owned();
    let dir = repo_relative_dir(base, path);

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
        sections,
    }
}
