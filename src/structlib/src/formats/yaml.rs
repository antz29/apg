//! The `yaml` emitter: top-level mapping keys, jobs and steps.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::{source_lines, unquote, SourceLine};

/// The `yaml` emitter: every top-level (column-0) mapping key, plus each job
/// under a top-level `jobs:` and each step (list item) under a job's `steps:`.
/// Jobs nest under `jobs`; steps nest under their job and are named by their
/// ordinal (`step-1`, …) — a minimal, collision-free normalization.
pub(crate) fn emit_yaml(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);

    let mut i = 0usize;
    while i < lines.len() {
        let Some((indent, key)) = yaml_mapping_key(lines[i].text) else {
            i += 1;
            continue;
        };
        if indent != 0 {
            i += 1;
            continue;
        }
        let added = b.add(None, &key, lines[i].span());
        if key == "jobs" {
            let (jobs_id, jobs_fqn) = added;
            let end = yaml_block_end(&lines, i + 1);
            if let Some(job_indent) = lines[i + 1..end]
                .iter()
                .filter_map(|l| yaml_mapping_key(l.text))
                .map(|(ind, _)| ind)
                .min()
            {
                let jobs: Vec<usize> = (i + 1..end)
                    .filter(|&k| {
                        matches!(yaml_mapping_key(lines[k].text), Some((ind, _)) if ind == job_indent)
                    })
                    .collect();
                for (ji, &job_line) in jobs.iter().enumerate() {
                    let job_end = jobs.get(ji + 1).copied().unwrap_or(end);
                    let Some((_, job_key)) = yaml_mapping_key(lines[job_line].text) else {
                        continue;
                    };
                    let (job_id, job_fqn) = b.add(
                        Some((&jobs_id, &jobs_fqn)),
                        &job_key,
                        lines[job_line].span(),
                    );
                    let steps_line = (job_line + 1..job_end).find(|&k| {
                        matches!(
                            yaml_mapping_key(lines[k].text),
                            Some((ind, key)) if ind > job_indent && key == "steps"
                        )
                    });
                    if let Some(step_line) = steps_line {
                        let step_indent = yaml_mapping_key(lines[step_line].text)
                            .map(|(ind, _)| ind)
                            .unwrap_or(job_indent);
                        let mut n = 0u32;
                        for line in &lines[step_line + 1..job_end] {
                            if let Some((ind, _)) = yaml_list_item(line.text) {
                                if ind > step_indent {
                                    n += 1;
                                    b.add(
                                        Some((&job_id, &job_fqn)),
                                        &format!("step-{n}"),
                                        line.span(),
                                    );
                                }
                            } else if let Some((ind, _)) = yaml_mapping_key(line.text) {
                                if ind <= step_indent {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }
    b.finish()
}

/// A YAML block-mapping key on `line`: `(indent, key)`, or `None` for a blank,
/// comment, list-item or non-key line. The `:` must be followed by whitespace
/// or end-of-line (so a `http://` value does not read as a key).
fn yaml_mapping_key(line: &str) -> Option<(usize, String)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    let rest = &line[indent..];
    if rest.is_empty() || rest.starts_with('#') || rest.starts_with('-') {
        return None;
    }
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\t')) {
        return None;
    }
    let key = rest[..colon].trim();
    if key.is_empty() {
        return None;
    }
    Some((indent, unquote(key)))
}

/// A YAML sequence entry on `line`: `(indent, item-text)`, or `None`.
fn yaml_list_item(line: &str) -> Option<(usize, String)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    let rest = &line[indent..];
    let body = rest.strip_prefix('-')?;
    if !(body.is_empty() || body.starts_with(' ')) {
        return None;
    }
    Some((indent, body.trim().to_string()))
}

/// The end index (exclusive) of the block starting at `start`: the lines up to
/// the next column-0 mapping key, blank lines and comments included.
fn yaml_block_end(lines: &[SourceLine<'_>], start: usize) -> usize {
    let mut end = start;
    while end < lines.len() {
        let t = lines[end].text;
        if t.trim().is_empty() || t.trim_start().starts_with('#') {
            end += 1;
            continue;
        }
        match yaml_mapping_key(t) {
            Some((0, _)) => break,
            Some(_) => end += 1,
            None => {
                if yaml_list_item(t).is_some() {
                    end += 1;
                } else {
                    break;
                }
            }
        }
    }
    end
}
