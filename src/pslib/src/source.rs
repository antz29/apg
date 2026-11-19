//! Source reading: BOM-aware lossy decoding with a decoded→raw byte-offset map,
//! plus the line/offset machinery every record's location uses.

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

/// Decode `bytes` (UTF-8, or UTF-16LE/BE by BOM) lossily, recording raw offsets.
pub fn decode_mapped(bytes: &[u8]) -> Decoded {
    let mut d = Decoded {
        text: String::with_capacity(bytes.len()),
        offsets: Vec::with_capacity(bytes.len() + 1),
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(&mut d, rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(&mut d, rest, u16::from_be_bytes),
        [0xEF, 0xBB, 0xBF, rest @ ..] => utf8(&mut d, rest, 3),
        _ => utf8(&mut d, bytes, 0),
    }
    d.offsets.push(bytes.len());
    d
}

/// Decode `bytes` to text (see [`decode_mapped`]).
pub fn decode(bytes: &[u8]) -> String {
    decode_mapped(bytes).text
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

fn utf16(d: &mut Decoded, bytes: &[u8], unit: fn([u8; 2]) -> u16) {
    let (pairs, odd) = bytes.as_chunks::<2>();
    let mut raw = 2;
    for r in char::decode_utf16(pairs.iter().map(|&p| unit(p))) {
        let (c, units) = match r {
            Ok(c) => (c, c.len_utf16()),
            Err(_) => (char::REPLACEMENT_CHARACTER, 1),
        };
        d.push(c, raw);
        raw += 2 * units;
    }
    if !odd.is_empty() {
        d.push(char::REPLACEMENT_CHARACTER, raw);
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

/// The span of decoded bytes `start_byte..end_byte` (tree-sitter offsets).
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

/// 1-based (line, col) of decoded `byte`, col counted in characters.
pub fn position(text: &str, byte: usize) -> (usize, usize) {
    let before = &text.as_bytes()[..byte.min(text.len())];
    let line_start = before
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let col = before[line_start..]
        .iter()
        .filter(|&&b| b & 0xC0 != 0x80)
        .count();
    (line_of(text, byte), col + 1)
}

fn line_of(text: &str, byte: usize) -> usize {
    1 + text.as_bytes()[..byte.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}
