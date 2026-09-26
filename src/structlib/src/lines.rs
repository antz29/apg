//! Line indexing and byte/line spans shared by the structural emitters.

/// One physical source line: its 1-based number and the byte range of its
/// content (line terminator excluded, a trailing `\r` stripped).
pub(crate) struct SourceLine<'a> {
    pub(crate) no: u32,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) text: &'a str,
}

/// A byte + 1-based-line range, the location shape every `Struct` record
/// carries.
#[derive(Clone, Copy)]
pub(crate) struct Span {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
}

impl SourceLine<'_> {
    pub(crate) fn span(&self) -> Span {
        Span {
            start: self.start,
            end: self.end,
            start_line: self.no,
            end_line: self.no,
        }
    }
}

/// The span covering `first..=last` (first line's start through last line's
/// end).
pub(crate) fn range_span(first: &SourceLine<'_>, last: &SourceLine<'_>) -> Span {
    Span {
        start: first.start,
        end: last.end,
        start_line: first.no,
        end_line: last.no,
    }
}

/// Splits `text` into [`SourceLine`]s. Byte ranges exclude the line terminator;
/// line numbers are 1-based, matching `parse_headings`/`line_count`.
pub(crate) fn source_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    for (no, raw) in (1u32..).zip(text.split_inclusive('\n')) {
        let without_nl = raw.strip_suffix('\n').unwrap_or(raw);
        let line = without_nl.strip_suffix('\r').unwrap_or(without_nl);
        let start = offset;
        let end = offset + line.len() as u32;
        offset += raw.len() as u32;
        out.push(SourceLine {
            no,
            start,
            end,
            text: line,
        });
    }
    out
}

/// The index of the line containing byte `offset` (the last line whose start
/// precedes it).
pub(crate) fn line_index_at(lines: &[SourceLine<'_>], offset: usize) -> usize {
    let mut idx = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if l.start as usize <= offset {
            idx = i;
        } else {
            break;
        }
    }
    idx
}

/// Strips one matching pair of surrounding single/double quotes.
pub(crate) fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Mirrors the other frontends' file line count: `"a\nb\n"` -> 2, `""` -> 1.
pub(crate) fn line_count(text: &str) -> u32 {
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
