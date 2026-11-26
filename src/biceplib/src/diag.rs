//! Bicep diagnostics on stderr: one `bicep: file:line:col: message` line per
//! syntax error, one closing count line, and the crash reason as the last
//! stderr line of a panicking run.

use crate::parse::ParseError;
use std::io::{self, Write};

/// Writes one syntax error as a single `bicep: …` line; the caller keeps scanning.
pub fn write_parse_error(out: &mut impl Write, file: &str, err: &ParseError) -> io::Result<()> {
    writeln!(
        out,
        "bicep: {file}:{}:{}: {}",
        err.line, err.column, err.message
    )
}

/// The closing line: `bicep: N file(s) had syntax errors`.
pub fn error_summary(files_with_errors: usize) -> String {
    let noun = if files_with_errors == 1 {
        "file"
    } else {
        "files"
    };
    format!("bicep: {files_with_errors} {noun} had syntax errors")
}

/// Replaces the default multi-line panic report so the reason is the last stderr line.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let reason = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        eprintln!("bicepfrontend: {reason}");
    }));
}
