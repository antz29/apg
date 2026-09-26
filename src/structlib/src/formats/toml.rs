//! The `toml` emitter: table headers and keys.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::{source_lines, unquote};

/// The `toml` emitter: one `Struct` per table header (`[t]` / `[[t]]`) plus one
/// per key, nested under the table that owns it (or the file before the first
/// header).
pub(crate) fn emit_toml(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut table: Option<(String, String)> = None;
    for line in &lines {
        if let Some(name) = toml_table_header(line.text) {
            table = Some(b.add(None, &name, line.span()));
        } else if let Some(key) = toml_key(line.text) {
            let parent = table.as_ref().map(|(id, fqn)| (id.as_str(), fqn.as_str()));
            b.add(parent, &key, line.span());
        }
    }
    b.finish()
}

/// A TOML table header on `line` (`[a.b]` or `[[a.b]]`) → its name, or `None`.
fn toml_table_header(line: &str) -> Option<String> {
    let s = line.trim();
    if let Some(rest) = s.strip_prefix("[[") {
        let name = rest.strip_suffix("]]")?.trim();
        return (!name.is_empty()).then(|| name.to_string());
    }
    if let Some(rest) = s.strip_prefix('[') {
        let name = rest.strip_suffix(']')?.trim();
        return (!name.is_empty()).then(|| name.to_string());
    }
    None
}

/// A TOML key assignment on `line` (`key = value`) → its key, or `None`.
fn toml_key(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() || s.starts_with('#') || s.starts_with('[') {
        return None;
    }
    let eq = s.find('=')?;
    let key = s[..eq].trim();
    if key.is_empty() {
        return None;
    }
    Some(unquote(key))
}
