//! Source reading: the claimed-file walk (pruning `.terraform`), UTF-8 decoding
//! with a decoded→raw byte-offset map, and the byte-offset line spans every
//! record's location uses.

use std::path::{Path, PathBuf};

/// Decoded source text plus, per decoded byte position, the raw on-disk
/// byte offset where that position's char begins.
pub struct Decoded {
    pub text: String,
    /// `offsets[i]` is the raw start of the char covering decoded byte `i`;
    /// `offsets[text.len()]` is the raw length.
    offsets: Vec<usize>,
}

impl Decoded {
    /// The raw on-disk byte offset of decoded byte `decoded_byte` (a position
    /// inside a char maps to that char's raw start; past the end clamps).
    pub fn raw_offset(&self, decoded_byte: usize) -> usize {
        self.offsets[decoded_byte.min(self.text.len())]
    }

    fn push(&mut self, c: char, raw: usize) {
        self.text.push(c);
        self.offsets.extend(std::iter::repeat_n(raw, c.len_utf8()));
    }
}

/// Decode `bytes` (UTF-8 with an optional BOM) lossily, recording raw offsets.
pub fn decode_mapped(bytes: &[u8]) -> Decoded {
    let mut d = Decoded {
        text: String::with_capacity(bytes.len()),
        offsets: Vec::with_capacity(bytes.len() + 1),
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => utf8(&mut d, rest, 3),
        _ => utf8(&mut d, bytes, 0),
    }
    d.offsets.push(bytes.len());
    d
}

fn utf8(d: &mut Decoded, bytes: &[u8], base: usize) {
    let mut raw = base;
    for chunk in bytes.utf8_chunks() {
        for (i, c) in chunk.valid().char_indices() {
            d.push(c, raw + i);
        }
        raw += chunk.valid().len();
        if !chunk.invalid().is_empty() {
            d.push(char::REPLACEMENT_CHARACTER, raw);
            raw += chunk.invalid().len();
        }
    }
}

/// Lines in `text`: minimum 1; a trailing newline opens no extra line.
pub fn line_count(text: &str) -> usize {
    text.lines().count().max(1)
}

/// A record location: raw on-disk byte offsets and 1-based inclusive lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub start_line: usize,
    pub end_line: usize,
}

/// The span of decoded byte offsets `start_byte..end_byte` into `Decoded.text`.
pub fn span(src: &Decoded, start_byte: usize, end_byte: usize) -> Span {
    // An end just past a line break stays on the preceding line.
    let last = if end_byte > start_byte {
        end_byte - 1
    } else {
        start_byte
    };
    Span {
        start: src.raw_offset(start_byte),
        end: src.raw_offset(end_byte),
        start_line: line_of(&src.text, start_byte),
        end_line: line_of(&src.text, last),
    }
}

fn line_of(text: &str, byte: usize) -> usize {
    1 + text.as_bytes()[..byte.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// The Bicep sources (`.bicep` templates and `.bicepparam` parameter files),
/// in any letter case.
pub fn is_bicep_source(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["bicep", "bicepparam"]
            .iter()
            .any(|c| e.eq_ignore_ascii_case(c))
    })
}

/// Every claimed source at or below `root`, sorted. Build, VCS, worktree,
/// Terraform cache (`.terraform`) and the top-level `apg/` layout trees are pruned.
pub fn claimed_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pruned = matches!(
                name.as_str(),
                "target" | "node_modules" | ".git" | ".worktrees" | ".terraform"
            );
            if pruned || (dir == root && name == "apg") {
                continue;
            }
            walk(root, &path, out);
        } else if ft.is_file() && is_bicep_source(&path) {
            out.push(path);
        }
    }
}
