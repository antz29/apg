//! The `makefile` emitter: targets, variables and include directives.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::source_lines;

/// The `makefile` emitter: targets, variable assignments and include
/// directives, all file-rooted (a Makefile has no enclosing structure).
pub(crate) fn emit_makefile(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    for line in &lines {
        let t = line.text;
        if t.starts_with('\t') || t.trim().is_empty() || t.trim_start().starts_with('#') {
            continue;
        }
        if let Some(name) = makefile_include(t) {
            b.add(None, &name, line.span());
        } else if let Some(name) = makefile_assignment(t) {
            b.add(None, &name, line.span());
        } else if let Some(names) = makefile_targets(t) {
            for name in names {
                b.add(None, &name, line.span());
            }
        }
    }
    b.finish()
}

/// An include directive on `line` (`include`/`-include`/`sinclude`) → its
/// argument, or `None`.
fn makefile_include(line: &str) -> Option<String> {
    let s = line.trim_start();
    let rest = ["-include", "sinclude", "include"].iter().find_map(|kw| {
        s.strip_prefix(kw)
            .filter(|r| r.starts_with([' ', '\t']))
            .map(str::trim)
    })?;
    (!rest.is_empty()).then(|| rest.to_string())
}

/// A variable assignment on `line` (`VAR = x`, `VAR := x`, `VAR ?= x`, …) → its
/// name, or `None`.
fn makefile_assignment(line: &str) -> Option<String> {
    if line.is_empty() || line.starts_with(char::is_whitespace) || line.starts_with('#') {
        return None;
    }
    let eq = line.find('=')?;
    let name = line[..eq]
        .trim_end()
        .trim_end_matches([':', '?', '+', '!'])
        .trim_end();
    if name.is_empty() || name.contains(':') || name.contains(' ') {
        return None;
    }
    Some(name.to_string())
}

/// The targets of a rule line (`targets: prereqs`) → one name per target, or
/// `None`.
fn makefile_targets(line: &str) -> Option<Vec<String>> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let colon = line.find(':')?;
    if line[colon + 1..].starts_with('=') {
        return None; // `:=` assignment
    }
    let left = line[..colon].trim();
    if left.is_empty() || left.contains('=') {
        return None;
    }
    Some(left.split_whitespace().map(str::to_string).collect())
}
