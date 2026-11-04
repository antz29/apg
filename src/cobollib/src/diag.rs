//! COBOL diagnostics: one record per error, printed as `cobol: file:line:col: message`.

use std::fmt;
use std::io::{self, Write};

/// A diagnostic at a 1-based source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file, self.line, self.col, self.message
        )
    }
}

/// Writes one diagnostic as a single `cobol: …` line; the caller keeps scanning.
pub fn write_diagnostic<W: Write>(out: &mut W, diag: &Diagnostic) -> io::Result<()> {
    writeln!(out, "cobol: {diag}")
}
