//! PowerShell diagnostics: one record per syntax-error region, printed as
//! `ps: file:line:col: message`.

use std::fmt;
use std::io::{self, Write};

use tree_sitter::Tree;

use crate::source::position;

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

/// Writes one diagnostic as a single `ps: …` line; the caller keeps scanning.
pub fn write_diagnostic(out: &mut impl Write, diag: &Diagnostic) -> io::Result<()> {
    writeln!(out, "ps: {diag}")
}

/// Each error region of `tree` once, in source order: an outermost ERROR node
/// (the root included) as `syntax error`, or a MISSING node outside any
/// ERROR as `missing <kind>`, positioned at the region's start in `text`.
pub fn error_regions(file: &str, text: &str, tree: &Tree) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    // Iterative pre-order walk: deep trees must not overflow the stack.
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        let message = if node.is_error() {
            Some("syntax error".to_string())
        } else if node.is_missing() {
            Some(format!("missing {}", node.kind()))
        } else {
            None
        };
        let descend = match message {
            Some(message) => {
                let (line, col) = position(text, node.start_byte());
                out.push(Diagnostic {
                    file: file.to_string(),
                    line,
                    col,
                    message,
                });
                false
            }
            None => true,
        };
        if descend && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// The closing line: `ps: N file(s) had syntax errors`.
pub fn error_summary(files_with_errors: usize) -> String {
    let noun = if files_with_errors == 1 {
        "file"
    } else {
        "files"
    };
    format!("ps: {files_with_errors} {noun} had syntax errors")
}
