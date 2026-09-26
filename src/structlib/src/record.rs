//! The unified JSONL record types (SPEC §2) and the record writer.

use std::io::{self, Write};

/// Unified JSONL records (SPEC §2). `id` is a scanner-local opaque counter; the
/// ingestor maps ids to canonical FQNs and renders `parent.name`.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: u32,
        end_line: u32,
    },
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },
    Contains {
        from: String,
        to: String,
    },
}

/// Serializes one record as a single JSONL line, terminated by `\n`.
pub(crate) fn write_rec<W: Write>(w: &mut W, rec: &Rec) -> io::Result<()> {
    let line = serde_json::to_string(rec).expect("serialize record");
    writeln!(w, "{line}")
}
