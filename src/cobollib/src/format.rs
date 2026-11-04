//! Source format detection and area A checks (SPEC §3, §19.1).
//! Fixed layout: columns 1-6 sequence area, 7 indicator, 8-72 text, 73-80 ignored.

use crate::diag::Diagnostic;
use crate::dialect::{Dialect, SourceFormat};

/// The layout a file is read through: a column layout, or free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Source(SourceFormat),
    Free,
}

const TEXT_START: usize = 8;
const AREA_A_END: usize = 11;
const TEXT_END: usize = 72;

/// Detects a file's layout and checks area A under the `cobol85` layout. Returns the layout
/// the file is read through and its diagnostics (directive, free-format warning, area A).
pub fn check(file: &str, text: &str, dialect: Dialect) -> (Layout, Vec<Diagnostic>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut diags = Vec::new();
    let directive = lines
        .iter()
        .enumerate()
        .find_map(|(i, line)| directive(line).map(|(col, word)| (i + 1, col, word)));
    let layout = match directive {
        Some((line, col, word)) => {
            match word.as_str() {
                "FIXED" => Layout::Source(SourceFormat::Fixed),
                "COBOL85" => Layout::Source(SourceFormat::Cobol85),
                "FREE" => Layout::Free,
                other => {
                    diags.push(diag(
                    file,
                    line,
                    col,
                    format!("source format `{other}` is not supported; reading as the dialect default"),
                ));
                    Layout::Source(dialect.default_format())
                }
            }
        }
        None => {
            let looks_free = lines
                .iter()
                .enumerate()
                .find_map(|(i, line)| free_reason(line).map(|why| (i + 1, why)));
            if let Some((line, why)) = looks_free {
                diags.push(diag(
                    file,
                    line,
                    1,
                    format!("warning: text looks free-format ({why}); reading as fixed format"),
                ));
            }
            Layout::Source(dialect.default_format())
        }
    };
    if layout == Layout::Source(SourceFormat::Cobol85) {
        diags.extend(area_a(file, &lines));
    }
    (layout, diags)
}

fn diag(file: &str, line: usize, col: usize, message: String) -> Diagnostic {
    Diagnostic {
        file: file.to_string(),
        line,
        col,
        message,
    }
}

/// A `>>SOURCE` directive line: its 1-based column and the upper-cased format word.
fn directive(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let col = line.len() - trimmed.len() + 1;
    let mut words = trimmed.strip_prefix(">>")?.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("SOURCE") {
        return None;
    }
    let word = words
        .find(|w| !(w.eq_ignore_ascii_case("FORMAT") || w.eq_ignore_ascii_case("IS")))
        .unwrap_or("");
    Some((col, word.to_ascii_uppercase()))
}

/// Why a line looks free-format: text past column 80 (columns 73-80 are the identification
/// area, used by fixed files), or a column-7 character that is not an indicator.
fn free_reason(line: &str) -> Option<&'static str> {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > 80 && chars[80..].iter().any(|c| !c.is_whitespace()) {
        return Some("text past column 80");
    }
    let indicator = chars.get(6).copied().unwrap_or(' ');
    (!chars.iter().all(|c| c.is_whitespace()) && !matches!(indicator, ' ' | '*' | '/' | '-' | 'D'))
        .then_some("column 7 is not an indicator")
}

/// Area A checks (§3). A line's first word is checked only where a sentence begins (after a
/// period, or at the top of the file); a separator period is checked at any line start.
/// Comment (`*` or `/` indicator), directive and continuation lines are not checked.
fn area_a(file: &str, lines: &[&str]) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut sentence_start = true;
    let mut procedure = false;
    for (i, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let indicator = chars.get(6).copied().unwrap_or(' ');
        if matches!(indicator, '*' | '/') || line.trim_start().starts_with(">>") {
            continue;
        }
        let body: String = chars.iter().skip(7).take(TEXT_END - 7).collect();
        let text = body.trim();
        if text.is_empty() {
            continue;
        }
        let col = TEXT_START + body.chars().take_while(|c| *c == ' ').count();
        if indicator != '-' {
            let mut words = text.split_whitespace();
            let first = words.next().unwrap_or_default().to_ascii_uppercase();
            let second = words.next().unwrap_or_default().to_ascii_uppercase();
            let in_a = col <= AREA_A_END;
            if let Some(why) =
                area_a_departure(text, &first, &second, in_a, sentence_start, procedure)
            {
                diags.push(diag(file, i + 1, col, why.to_string()));
            }
            if sentence_start && second.trim_end_matches('.') == "DIVISION" {
                procedure = first == "PROCEDURE";
            }
        }
        sentence_start = text.ends_with('.');
    }
    diags
}

/// The area A departure of a line that starts with `first`, if any. `procedure` is true inside
/// the PROCEDURE DIVISION, where a one-word sentence is a paragraph name.
fn area_a_departure(
    text: &str,
    first: &str,
    second: &str,
    in_a: bool,
    sentence_start: bool,
    procedure: bool,
) -> Option<&'static str> {
    if text.starts_with('.') {
        return in_a.then_some("separator period starts in area A");
    }
    if !sentence_start {
        return None;
    }
    if let Ok(level) = first.trim_end_matches('.').parse::<u32>() {
        return (matches!(level, 1 | 77) && !in_a)
            .then_some("level 01 or 77 name must start in area A");
    }
    if matches!(second.trim_end_matches('.'), "DIVISION" | "SECTION") {
        return (!in_a).then_some("division or section name must start in area A");
    }
    if procedure {
        if is_paragraph(first) {
            return (!in_a).then_some("paragraph name must start in area A");
        }
    } else if ID_ENV_PARAGRAPHS.contains(&first) {
        return (!in_a).then_some("paragraph name must start in area A");
    } else if is_paragraph(first) {
        return None;
    }
    (in_a && !matches!(first, "FD" | "SD" | "RD" | "CD" | "END"))
        .then_some("statement starts in area A")
}

/// Paragraph headers outside the PROCEDURE DIVISION (IDENTIFICATION and ENVIRONMENT).
const ID_ENV_PARAGRAPHS: [&str; 12] = [
    "PROGRAM-ID.",
    "AUTHOR.",
    "INSTALLATION.",
    "DATE-WRITTEN.",
    "DATE-COMPILED.",
    "SECURITY.",
    "REMARKS.",
    "SOURCE-COMPUTER.",
    "OBJECT-COMPUTER.",
    "SPECIAL-NAMES.",
    "FILE-CONTROL.",
    "I-O-CONTROL.",
];

/// COBOL-85 scope terminators: one-word sentences that are statements, not paragraph names.
const SCOPE_TERMINATORS: [&str; 19] = [
    "END-ADD.",
    "END-CALL.",
    "END-COMPUTE.",
    "END-DELETE.",
    "END-DIVIDE.",
    "END-EVALUATE.",
    "END-IF.",
    "END-MULTIPLY.",
    "END-PERFORM.",
    "END-READ.",
    "END-RECEIVE.",
    "END-RETURN.",
    "END-REWRITE.",
    "END-SEARCH.",
    "END-START.",
    "END-STRING.",
    "END-SUBTRACT.",
    "END-UNSTRING.",
    "END-WRITE.",
];

/// A paragraph name is one word ending in a period. Scope terminators and bare verbs that
/// end a sentence are not names.
fn is_paragraph(first: &str) -> bool {
    first.ends_with('.')
        && !SCOPE_TERMINATORS.contains(&first)
        && !matches!(first, "EXIT." | "CONTINUE." | "GOBACK." | "NEXT." | "ELSE.")
}
