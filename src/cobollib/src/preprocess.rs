//! COPY expansion for the COBOL frontend. Each COPY statement is replaced by its copybook's
//! text (its REPLACING pairs applied through `replace`), then the file's REPLACE statements
//! apply over the expanded stream. Every token keeps its original file, line and column.
//! Copybook text comes from an injected lookup, so the logic runs in memory.

use std::path::{Path, PathBuf};

use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::format;
use crate::lexer::{self, Token, TokenKind};
use crate::replace;
use crate::source_map::{FileId, Mapped, Origin, SourceMap};

/// The deepest COPY nesting: a COPY inside a file already `MAX_DEPTH` files deep is refused.
pub const MAX_DEPTH: usize = 10;

/// Where copybooks are searched beyond the including file's directory.
pub struct Search {
    pub scan_root: PathBuf,
    pub copy_paths: Vec<PathBuf>,
}

/// Suffixes tried after the bare name; each known extension in lower and upper case.
const EXTENSIONS: [&str; 9] = [
    "", ".cpy", ".CPY", ".cbl", ".CBL", ".cob", ".COB", ".cobol", ".COBOL",
];

/// The filesystem lookup: a file's text, or `None` when it is absent or unreadable.
pub fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Expands COPY and REPLACE in `text`, the file named `name`. Returns the tokens in expanded
/// order, each with its original position, and every diagnostic.
pub fn preprocess(
    map: &mut SourceMap,
    name: &str,
    text: &str,
    dialect: Dialect,
    search: &Search,
    lookup: &dyn Fn(&Path) -> Option<String>,
) -> (Vec<Mapped>, Vec<Diagnostic>) {
    let mut pre = Pre {
        map,
        dialect,
        search,
        lookup,
        side: Vec::new(),
        diags: Vec::new(),
    };
    let mut stack = vec![PathBuf::from(name)];
    let tokens = pre.stream(Path::new(name), text, &mut stack);
    let mapped = tokens
        .into_iter()
        .map(|t| {
            let (file, orig) = &pre.side[t.offset];
            Mapped {
                origin: Origin {
                    file: *file,
                    line: orig.line,
                    col: orig.col,
                },
                token: Token {
                    kind: t.kind,
                    text: t.text,
                    ..orig.clone()
                },
            }
        })
        .collect();
    (mapped, pre.diags)
}

fn is_word(t: &Token, word: &str) -> bool {
    matches!(t.kind, TokenKind::Word | TokenKind::Reserved) && t.text.eq_ignore_ascii_case(word)
}

struct Pre<'a> {
    map: &'a mut SourceMap,
    dialect: Dialect,
    search: &'a Search,
    lookup: &'a dyn Fn(&Path) -> Option<String>,
    /// Every token handed to `replace`, by index: the token's `offset` and `line` (= index + 1)
    /// name its slot, so the original file and position come back after `replace` runs.
    side: Vec<(FileId, Token)>,
    diags: Vec<Diagnostic>,
}

impl Pre<'_> {
    fn intern(&mut self, file: FileId, token: Token) -> Token {
        let index = self.side.len();
        self.side.push((file, token.clone()));
        Token {
            offset: index,
            line: index + 1,
            ..token
        }
    }

    fn origin(&self, index: usize) -> Origin {
        let (file, at) = &self.side[index];
        Origin {
            file: *file,
            line: at.line,
            col: at.col,
        }
    }

    fn diag(&mut self, index: usize, message: &str) {
        let origin = self.origin(index);
        let diag = self.map.diagnostic(origin, message);
        self.diags.push(diag);
    }

    /// Records the diagnostics `replace` produced: their `line` is an interned index.
    fn lift(&mut self, diags: Vec<Diagnostic>) {
        for d in diags {
            let origin = self.origin(d.line - 1);
            let diag = self.map.diagnostic(origin, &d.message);
            self.diags.push(diag);
        }
    }

    /// The tokens of one file, COPY-expanded and REPLACE-applied. `stack` ends with `path`.
    fn stream(&mut self, path: &Path, text: &str, stack: &mut Vec<PathBuf>) -> Vec<Token> {
        let name = path.display().to_string();
        let file = self.map.add_file(&name);
        let (layout, diags) = format::check(&name, text, self.dialect);
        self.diags.extend(diags);
        let (mut cursor, diags) = lexer::lex(&name, text, layout, self.dialect);
        self.diags.extend(diags);
        let mut tokens = Vec::new();
        while let Some(token) = cursor.bump() {
            tokens.push(self.intern(file, token));
        }
        let expanded = self.expand(tokens, stack);
        let (out, diags) = replace::replace("", expanded);
        self.lift(diags);
        out
    }

    /// `tokens` with each COPY statement replaced by its copybook text.
    fn expand(&mut self, tokens: Vec<Token>, stack: &mut Vec<PathBuf>) -> Vec<Token> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            if !is_word(&tokens[i], "COPY") {
                out.push(tokens[i].clone());
                i += 1;
                continue;
            }
            let end = tokens[i..]
                .iter()
                .position(|t| t.kind == TokenKind::Period)
                .map_or(tokens.len(), |p| i + p + 1);
            out.extend(self.copy(&tokens[i..end], stack));
            i = end;
        }
        out
    }

    /// The text one COPY statement (`stmt`, through its period) expands to; empty when it cannot.
    fn copy(&mut self, stmt: &[Token], stack: &mut Vec<PathBuf>) -> Vec<Token> {
        let at = stmt[0].offset;
        let words = match stmt.last() {
            Some(t) if t.kind == TokenKind::Period => &stmt[1..stmt.len() - 1],
            _ => &stmt[1..],
        };
        let Some(name) = words
            .first()
            .filter(|t| matches!(t.kind, TokenKind::Word | TokenKind::Alnum))
        else {
            self.diag(at, "COPY needs a copybook name");
            return Vec::new();
        };
        let mut rest = &words[1..];
        if rest.len() >= 2 && (is_word(&rest[0], "OF") || is_word(&rest[0], "IN")) {
            rest = &rest[2..];
        }
        let clause: &[Token] = match rest.split_first() {
            None => &[],
            Some((first, tail)) if is_word(first, "REPLACING") => tail,
            Some(_) => {
                self.diag(at, "COPY must be followed by OF/IN, REPLACING or nothing");
                return Vec::new();
            }
        };
        if stack.len() > MAX_DEPTH {
            self.diag(at, &format!("COPY nests deeper than {MAX_DEPTH} levels"));
            return Vec::new();
        }
        let including = stack
            .last()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let Some((path, text)) = self.find(&name.text, including) else {
            self.diag(at, &format!("copybook `{}` not found", name.text));
            return Vec::new();
        };
        if stack.contains(&path) {
            self.diag(at, &format!("circular COPY of `{}`", name.text));
            return Vec::new();
        }
        stack.push(path.clone());
        let body = self.stream(&path, &text, stack);
        stack.pop();
        if clause.is_empty() {
            return body;
        }
        let (file, copy_token) = self.side[at].clone();
        let mut all = vec![self.synth(file, &copy_token, TokenKind::Word, "REPLACE")];
        all.extend(self.pairs(file, &copy_token, clause));
        all.push(self.synth(file, &copy_token, TokenKind::Period, "."));
        all.extend(body);
        let (out, diags) = replace::replace("", all);
        self.lift(diags);
        out
    }

    /// A new token of `kind` and `text` at the position of `at`.
    fn synth(&mut self, file: FileId, at: &Token, kind: TokenKind, text: &str) -> Token {
        self.intern(
            file,
            Token {
                kind,
                text: text.to_string(),
                ..at.clone()
            },
        )
    }

    /// The REPLACING clause as REPLACE pairs: a bare `A BY B` becomes `==A== BY ==B==`.
    fn pairs(&mut self, file: FileId, at: &Token, clause: &[Token]) -> Vec<Token> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < clause.len() {
            let t = &clause[i];
            if t.kind == TokenKind::PseudoDelim {
                out.push(t.clone());
                i += 1;
                while i < clause.len() && clause[i].kind != TokenKind::PseudoDelim {
                    out.push(clause[i].clone());
                    i += 1;
                }
                if i < clause.len() {
                    out.push(clause[i].clone());
                    i += 1;
                }
            } else if is_word(t, "BY") || t.kind == TokenKind::Comma {
                out.push(t.clone());
                i += 1;
            } else {
                out.push(self.synth(file, at, TokenKind::PseudoDelim, "=="));
                out.push(t.clone());
                out.push(self.synth(file, at, TokenKind::PseudoDelim, "=="));
                i += 1;
            }
        }
        out
    }

    /// The first copybook `name` (under an extension from `EXTENSIONS`) in the search order:
    /// the including file's directory, the scan root, `copybooks/`, `copy/`, then `copy_paths`.
    fn find(&self, name: &str, including: PathBuf) -> Option<(PathBuf, String)> {
        let root = &self.search.scan_root;
        let dirs = [
            including,
            root.clone(),
            root.join("copybooks"),
            root.join("copy"),
        ]
        .into_iter()
        .chain(self.search.copy_paths.iter().cloned());
        dirs.flat_map(|dir| {
            EXTENSIONS
                .iter()
                .map(move |ext| dir.join(format!("{name}{ext}")))
        })
        .find_map(|path| (self.lookup)(&path).map(|text| (path, text)))
    }
}
