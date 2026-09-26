//! The `dockerfile` emitter: build stages and instructions.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::source_lines;

/// The `dockerfile` emitter: one `Struct` per build stage (`FROM`, named by its
/// `AS` alias or `stage-N`) plus one per `RUN`/`COPY`/`ENV` instruction, nested
/// under the current stage. The `FROM` line is the stage record itself.
pub(crate) fn emit_dockerfile(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut stage: Option<(String, String)> = None;
    let mut stage_no = 0u32;
    for line in &lines {
        let t = line.text.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((kw, rest)) = dockerfile_instruction(t) else {
            continue;
        };
        match kw {
            "FROM" => {
                stage_no += 1;
                let name = dockerfile_stage_name(rest, stage_no);
                stage = Some(b.add(None, &name, line.span()));
            }
            "RUN" | "COPY" | "ENV" => {
                let parent = stage.as_ref().map(|(id, fqn)| (id.as_str(), fqn.as_str()));
                b.add(parent, kw, line.span());
            }
            _ => {}
        }
    }
    b.finish()
}

/// The instruction keyword and its arguments for a Dockerfile line, or `None`
/// for an instruction the emitter does not name.
fn dockerfile_instruction(line: &str) -> Option<(&'static str, &str)> {
    let (kw, rest) = match line.split_once(char::is_whitespace) {
        Some((k, r)) => (k, r.trim_start()),
        None => (line, ""),
    };
    let known = match kw.to_ascii_uppercase().as_str() {
        "FROM" => "FROM",
        "RUN" => "RUN",
        "COPY" => "COPY",
        "ENV" => "ENV",
        _ => return None,
    };
    Some((known, rest))
}

/// A build stage's name: its `AS <alias>` alias when present, else `stage-N`.
fn dockerfile_stage_name(rest: &str, stage_no: u32) -> String {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if let Some(pos) = tokens.iter().position(|t| t.eq_ignore_ascii_case("AS")) {
        if let Some(alias) = tokens.get(pos + 1) {
            return alias.to_string();
        }
    }
    format!("stage-{stage_no}")
}
