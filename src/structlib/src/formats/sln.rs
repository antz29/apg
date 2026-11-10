//! The `sln` emitter: Visual Studio solution Project entries and GlobalSections.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::{range_span, source_lines};

/// The `sln` emitter: one file-rooted `Struct` per `Project(...) = "Name", …`
/// entry (through `EndProject`) and per `GlobalSection(Name)` (through
/// `EndGlobalSection`). ProjectSections and the enclosing `Global` block get
/// no struct, so nothing nests.
pub(crate) fn emit_sln(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    // The open entry: (name, first line index, its closing marker).
    let mut open: Option<(String, usize, &str)> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.text.trim();
        if let Some((name, first, end)) = &open {
            if t == *end {
                b.add(None, name, range_span(&lines[*first], line));
                open = None;
            }
        } else if let Some(name) = sln_project(t) {
            open = Some((name, i, "EndProject"));
        } else if let Some(name) = sln_global_section(t) {
            open = Some((name, i, "EndGlobalSection"));
        }
    }
    // An unterminated entry runs to the end of the file.
    if let (Some((name, first, _)), Some(last)) = (open, lines.last()) {
        b.add(None, &name, range_span(&lines[first], last));
    }
    b.finish()
}

/// `Project("{type}") = "Name", "path", "{guid}"` → `Name`, or `None`.
fn sln_project(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Project(")?;
    let rest = rest[rest.find('=')? + 1..].trim_start().strip_prefix('"')?;
    let name = rest[..rest.find('"')?].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// `GlobalSection(Name) = preSolution` → `Name`, or `None`.
fn sln_global_section(line: &str) -> Option<String> {
    let rest = line.strip_prefix("GlobalSection(")?;
    let name = rest[..rest.find(')')?].trim();
    (!name.is_empty()).then(|| name.to_string())
}
