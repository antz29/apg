//! The `sh` emitter: shell function definitions.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::{range_span, source_lines, SourceLine};

/// The `sh` emitter: shell function definitions (POSIX `name()` / `name ()`
/// and the bash `function name` keyword) → one `Struct` each, spanning the
/// definition line through its closing `}`.
pub(crate) fn emit_sh(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    for (i, line) in lines.iter().enumerate() {
        if let Some(name) = sh_function_name(line.text) {
            let end = sh_block_end(&lines, i);
            b.add(None, &name, range_span(line, &lines[end]));
        }
    }
    b.finish()
}

/// The name of a shell function defined on `line`, or `None`.
fn sh_function_name(line: &str) -> Option<String> {
    let s = line.trim_start();
    if let Some(rest) = s.strip_prefix("function") {
        if rest.starts_with([' ', '\t']) {
            return shell_identifier(rest.trim_start());
        }
    }
    let name = shell_identifier(s)?;
    let after = s[name.len()..].trim_start();
    if after.starts_with('(') && after[1..].trim_start().starts_with(')') {
        return Some(name);
    }
    None
}

/// A POSIX shell name (`[A-Za-z_][A-Za-z0-9_]*`) at the start of `s`.
fn shell_identifier(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut end = first.len_utf8();
    for c in chars {
        if c.is_ascii_alphanumeric() || c == '_' {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    Some(s[..end].to_string())
}

/// The index of the line holding the closing `}` of the function whose
/// definition starts at `def` — a depth-only brace match (shell `${…}`
/// expansions are balanced, so they do not disturb the depth). Falls back to
/// `def` when no block opens.
fn sh_block_end(lines: &[SourceLine<'_>], def: usize) -> usize {
    let mut depth: i32 = 0;
    let mut opened = false;
    for (i, line) in lines.iter().enumerate().skip(def) {
        for ch in line.text.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' if opened => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            return i;
        }
    }
    def
}
