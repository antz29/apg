//! The `ini` emitter: sections and keys.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::source_lines;

/// The `ini` emitter: one `Struct` per `[section]` plus one per key, nested
/// under the section that owns it (or the file before the first section).
pub(crate) fn emit_ini(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut section: Option<(String, String)> = None;
    for line in &lines {
        if let Some(name) = ini_section(line.text) {
            section = Some(b.add(None, &name, line.span()));
        } else if let Some(key) = ini_key(line.text) {
            let parent = section
                .as_ref()
                .map(|(id, fqn)| (id.as_str(), fqn.as_str()));
            b.add(parent, &key, line.span());
        }
    }
    b.finish()
}

/// A `[section]` header on `line` → its name, or `None`.
fn ini_section(line: &str) -> Option<String> {
    let s = line.trim();
    let rest = s.strip_prefix('[')?;
    let name = rest.strip_suffix(']')?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// A `key = value` / `key: value` entry on `line` → its key, or `None`.
fn ini_key(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() || s.starts_with('#') || s.starts_with(';') || s.starts_with('[') {
        return None;
    }
    let sep = s.find(['=', ':'])?;
    let key = s[..sep].trim();
    (!key.is_empty()).then(|| key.to_string())
}
