//! The `xml` emitter: element occurrences, nested by document position.

use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::lines::{line_index_at, source_lines};

/// The `xml` emitter: every element occurrence, nested by its position in the
/// document. Repeated sibling element names are disambiguated (`dependency`,
/// `dependency-1`, …). Comments, processing instructions, doctypes and CDATA
/// are skipped.
pub(crate) fn emit_xml(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut rest: &str = &text;
    let mut offset = 0usize;
    while let Some(lt) = rest.find('<') {
        offset += lt;
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            match after.find("-->") {
                Some(k) => {
                    let adv = 4 + k + 3;
                    rest = &rest[adv..];
                    offset += adv;
                }
                None => break,
            }
            continue;
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            match after.find("]]>") {
                Some(k) => {
                    let adv = 9 + k + 3;
                    rest = &rest[adv..];
                    offset += adv;
                }
                None => break,
            }
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            match rest.find('>') {
                Some(k) => {
                    let adv = k + 1;
                    rest = &rest[adv..];
                    offset += adv;
                }
                None => break,
            }
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let inner = &rest[1..gt];
        let self_closing = inner.trim_end().ends_with('/');
        let inner = inner.trim_end().trim_end_matches('/');
        let closing = inner.starts_with('/');
        let name = inner
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if !name.is_empty() {
            if closing {
                stack.pop();
            } else {
                let parent = stack.last().map(|(id, fqn)| (id.as_str(), fqn.as_str()));
                let added = b.add(parent, name, lines[line_index_at(&lines, offset)].span());
                if !self_closing {
                    stack.push(added);
                }
            }
        }
        let adv = gt + 1;
        rest = &rest[adv..];
        offset += adv;
    }
    b.finish()
}
