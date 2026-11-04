//! Source map: pairs each expanded token with the original file it came from, so every
//! declaration and diagnostic is located in the original text (a copybook's own file for
//! text pulled in by COPY). `Token` carries no file, so the preprocessor maps per file.

use crate::diag::Diagnostic;
use crate::lexer::Token;

/// Index of a file registered in a `SourceMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId(usize);

/// The original `(file, line, column)` of a token; `line` and `col` are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub file: FileId,
    pub line: usize,
    pub col: usize,
}

/// A token together with where it sits in the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub token: Token,
    pub origin: Origin,
}

/// The registered source files, by name, for locating tokens and diagnostics.
#[derive(Debug, Default)]
pub struct SourceMap {
    names: Vec<String>,
}

impl SourceMap {
    /// Registers `name` and returns its id.
    pub fn add_file(&mut self, name: &str) -> FileId {
        self.names.push(name.to_string());
        FileId(self.names.len() - 1)
    }

    /// The name a `FileId` was registered under.
    pub fn name(&self, file: FileId) -> &str {
        &self.names[file.0]
    }

    /// Pairs every token of `file` with its origin; the token keeps its own line and column.
    pub fn map(&self, file: FileId, tokens: Vec<Token>) -> Vec<Mapped> {
        tokens
            .into_iter()
            .map(|token| {
                let origin = Origin {
                    file,
                    line: token.line,
                    col: token.col,
                };
                Mapped { token, origin }
            })
            .collect()
    }

    /// A diagnostic located at `at` in its original file.
    pub fn diagnostic(&self, at: Origin, message: &str) -> Diagnostic {
        Diagnostic {
            file: self.name(at.file).to_string(),
            line: at.line,
            col: at.col,
            message: message.to_string(),
        }
    }
}
