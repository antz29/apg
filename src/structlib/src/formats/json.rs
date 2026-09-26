//! The `json` emitter: top-level object keys.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::{line_index_at, source_lines};

/// The `json` emitter: every key of the top-level object, in document order.
/// Nested keys are not emitted (minimal depth).
pub(crate) fn emit_json(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    for (key, offset) in json_top_level_keys(&text) {
        let line = &lines[line_index_at(&lines, offset)];
        b.add(None, &key, line.span());
    }
    b.finish()
}

/// The keys of a JSON document's top-level object, in order, each paired with
/// the byte offset of its opening quote. Empty for a non-object root.
fn json_top_level_keys(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            '"' => {
                let (s, end) = json_read_string(text, i);
                if depth == 1 && text[end..].trim_start().starts_with(':') {
                    out.push((s, i));
                }
                while let Some(&(j, _)) = chars.peek() {
                    if j < end {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Reads the JSON string whose opening quote is at `start`, returning the
/// decoded text and the index just past its closing quote.
#[allow(clippy::while_let_on_iterator)]
fn json_read_string(text: &str, start: usize) -> (String, usize) {
    let mut s = String::new();
    let mut escaped = false;
    let mut end = text.len();
    let mut iter = text[start + 1..].char_indices();
    while let Some((off, c)) = iter.next() {
        if escaped {
            match c {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                'b' => s.push('\u{8}'),
                'f' => s.push('\u{c}'),
                'u' => {
                    let mut hex = String::new();
                    for _ in 0..4 {
                        if let Some((_, h)) = iter.next() {
                            hex.push(h);
                        }
                    }
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            s.push(ch);
                        }
                    }
                }
                other => s.push(other),
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            end = start + 1 + off + 1;
            break;
        } else {
            s.push(c);
        }
    }
    (s, end)
}
