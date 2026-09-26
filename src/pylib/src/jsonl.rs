//! The unified JSONL record writer.

use std::io::Write;

pub(crate) fn write_record(out: &mut impl Write, value: &serde_json::Value) {
    let _ = serde_json::to_writer(&mut *out, value);
    let _ = out.write_all(b"\n");
}
