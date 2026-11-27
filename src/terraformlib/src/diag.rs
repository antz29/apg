//! Terraform diagnostics on stderr: one `terraform: file:line:col: message`
//! line per failed parse or duplicate declaration, and one closing count line.

use std::io::{self, Write};

/// Writes one diagnostic as a single `terraform: …` line; the caller keeps scanning.
pub fn write_diagnostic(
    out: &mut impl Write,
    file: &str,
    line: usize,
    column: usize,
    message: &str,
) -> io::Result<()> {
    writeln!(out, "terraform: {file}:{line}:{column}: {message}")
}

/// The closing line: `terraform: N file(s) had syntax errors`.
pub fn error_summary(files_with_errors: usize) -> String {
    let noun = if files_with_errors == 1 {
        "file"
    } else {
        "files"
    };
    format!("terraform: {files_with_errors} {noun} had syntax errors")
}
