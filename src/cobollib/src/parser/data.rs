//! DATA DIVISION parser. Reads the division's tokens into `ast` sections and entries. Entries
//! nest by level number; their clauses are scanned for the facts the AST keeps (PICTURE,
//! REDEFINES, RENAMES, OCCURS) and every other clause is skipped. Entry point:
//! [`parse_data_division`], called by the parser with the tokens from `DATA DIVISION.` to the
//! end of the division.

use crate::ast::{
    DataDivision, DataEntry, DataSection, DataSectionKind, FileName, Name, ObjectReference, Pic,
    Property, Span,
};
use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::source_map::{Mapped, Origin, SourceMap};

use super::exec::{exec_block_end, is_exec_block, EXEC_NOT_ENDED, EXEC_NOT_IBM};

/// Words that open a clause or are clause operands. A clause word is never a data name and
/// ends a PICTURE string.
const CLAUSE_WORDS: &[&str] = &[
    "PIC",
    "PICTURE",
    "IS",
    "REDEFINES",
    "RENAMES",
    "OCCURS",
    "VALUE",
    "VALUES",
    "USAGE",
    "COMP",
    "COMP-1",
    "COMP-2",
    "COMP-3",
    "COMP-4",
    "COMP-5",
    "COMPUTATIONAL",
    "BINARY",
    "DISPLAY",
    "PACKED-DECIMAL",
    "INDEX",
    "POINTER",
    "SIGN",
    "LEADING",
    "TRAILING",
    "SEPARATE",
    "SYNC",
    "SYNCHRONIZED",
    "JUSTIFIED",
    "JUST",
    "BLANK",
    "WHEN",
    "EXTERNAL",
    "GLOBAL",
    "DEPENDING",
    "ASCENDING",
    "DESCENDING",
    "INDEXED",
    "NATIONAL",
    "BASED",
];

/// The section headers (`<word> SECTION.`). `None` marks an opaque section, skipped whole.
const SECTIONS: [(&str, Option<DataSectionKind>); 7] = [
    ("FILE", Some(DataSectionKind::File)),
    ("WORKING-STORAGE", Some(DataSectionKind::WorkingStorage)),
    ("LOCAL-STORAGE", Some(DataSectionKind::LocalStorage)),
    ("LINKAGE", Some(DataSectionKind::Linkage)),
    ("SCREEN", None),
    ("REPORT", None),
    ("COMMUNICATION", None),
];

/// Parses the DATA DIVISION at the start of `tokens` (`DATA DIVISION.` through the end of the
/// division, before the PROCEDURE DIVISION or the next program). `map` locates the tokens and
/// the diagnostics. Returns `None` when `tokens` does not start with the division header.
/// Diagnostics do not stop the parse: a malformed entry is dropped with its own tokens only.
pub fn parse_data_division(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
) -> Option<(DataDivision, Vec<Diagnostic>)> {
    if !(is(tokens.first()?, "DATA") && is(tokens.get(1)?, "DIVISION")) {
        return None;
    }
    let mut p = Parser {
        map,
        dialect,
        tokens,
        diags: Vec::new(),
        files: Vec::new(),
    };
    let body = tokens
        .iter()
        .position(|t| t.token.kind == TokenKind::Period)
        .map_or(tokens.len(), |at| at + 1);
    let mut sections = Vec::new();
    let mut i = body;
    while i < tokens.len() {
        let Some(opened) = section_header(tokens, i) else {
            let at = tokens[i].origin;
            p.diag(at, "expected a section header in the DATA DIVISION");
            i = (i + 1..tokens.len())
                .find(|&j| section_header(tokens, j).is_some())
                .unwrap_or(tokens.len());
            continue;
        };
        let body_start = i + 3;
        let end = (body_start..tokens.len())
            .find(|&j| section_header(tokens, j).is_some())
            .unwrap_or(tokens.len());
        if let Some(kind) = opened {
            sections.push(DataSection {
                kind,
                span: Span {
                    start: tokens[i].origin,
                    end: tokens[end - 1].origin,
                },
                entries: p.entries(body_start, end),
            });
        }
        i = end;
    }
    let division = DataDivision {
        span: Span {
            start: tokens[0].origin,
            end: tokens[tokens.len() - 1].origin,
        },
        sections,
        files: p.files,
    };
    Some((division, p.diags))
}

struct Parser<'a> {
    map: &'a SourceMap,
    dialect: Dialect,
    tokens: &'a [Mapped],
    diags: Vec<Diagnostic>,
    files: Vec<FileName>,
}

impl Parser<'_> {
    fn origin(&self, i: usize) -> Origin {
        self.tokens[i].origin
    }

    fn diag(&mut self, at: Origin, message: &str) {
        let diag = self.map.diagnostic(at, message);
        self.diags.push(diag);
    }

    fn name(&self, i: usize) -> Name {
        let origin = self.origin(i);
        Name {
            text: self.tokens[i].token.text.clone(),
            span: Span {
                start: origin,
                end: origin,
            },
        }
    }

    /// The index after the next period before `end`, or `end` when there is none.
    fn skip_entry(&self, mut i: usize, end: usize) -> usize {
        while i < end {
            if self.tokens[i].token.kind == TokenKind::Period {
                return i + 1;
            }
            i += 1;
        }
        end
    }

    /// The index after the EXEC block at `i` and its sentence period. Under `ibm` it is no fact
    /// and no error; elsewhere it is one error. Without an `END-EXEC` it is an error and skips to
    /// the period.
    fn exec_block(&mut self, i: usize, end: usize) -> usize {
        let Some(after) = exec_block_end(self.tokens, i, end) else {
            let at = self.origin(i);
            self.diag(at, EXEC_NOT_ENDED);
            return self.skip_entry(i, end);
        };
        if self.dialect != Dialect::Ibm {
            let at = self.origin(i);
            self.diag(at, EXEC_NOT_IBM);
        }
        if after < end && self.tokens[after].token.kind == TokenKind::Period {
            after + 1
        } else {
            after
        }
    }

    /// The entries in `i..end`, nested by level number. A malformed entry is dropped.
    fn entries(&mut self, mut i: usize, end: usize) -> Vec<DataEntry> {
        let tokens = self.tokens;
        let mut roots = Vec::new();
        let mut stack = Vec::new();
        while i < end {
            if is_exec_block(tokens, i) {
                // An EXEC block is not a data entry: it neither opens nor closes a record.
                i = self.exec_block(i, end);
                continue;
            }
            if is(&tokens[i], "FD") || is(&tokens[i], "SD") {
                // File and sort descriptions are not AST entries; they close the open records.
                let next = self.skip_entry(i, end);
                if let Some(t) = tokens.get(i + 1).filter(|_| i + 1 < end) {
                    if t.token.kind == TokenKind::Word && !is_clause(t) {
                        let name = self.name(i + 1);
                        let global = (i + 2..next).any(|j| is(&tokens[j], "GLOBAL"));
                        self.files.push(FileName { name, global });
                    }
                }
                close(&mut stack, &mut roots, 0);
                i = next;
                continue;
            }
            let dropped = level_of(&tokens[i]);
            let (entry, next) = self.entry(i, end);
            match (entry, dropped) {
                (Some(entry), _) => place(&mut stack, &mut roots, entry),
                (None, Some(level)) => close(&mut stack, &mut roots, level),
                (None, None) => {}
            }
            i = next;
        }
        close(&mut stack, &mut roots, 0);
        roots
    }

    /// The entry starting at `i` and the index after it. A malformed entry is a diagnostic,
    /// yields `None`, and is skipped through its period.
    fn entry(&mut self, i: usize, end: usize) -> (Option<DataEntry>, usize) {
        let tokens = self.tokens;
        let Some(level) = level_of(&tokens[i]) else {
            let at = self.origin(i);
            self.diag(at, "expected a level number 01-49, 66, 77 or 88");
            return (None, self.skip_entry(i, end));
        };
        let mut j = i + 1;
        let name = match tokens.get(j).filter(|_| j < end) {
            Some(t) if t.token.kind == TokenKind::Word && !is_clause(t) => {
                j += 1;
                if is(t, "FILLER") {
                    None
                } else {
                    Some(self.name(j - 1))
                }
            }
            Some(t) if t.token.kind == TokenKind::Period || is_clause(t) => None,
            None => None,
            Some(t) => {
                let at = t.origin;
                self.diag(
                    at,
                    &format!("expected a data name, found `{}`", t.token.text),
                );
                return (None, self.skip_entry(j, end));
            }
        };
        let mut entry = DataEntry {
            level,
            name,
            span: Span {
                start: self.origin(i),
                end: self.origin(i),
            },
            redefines: false,
            renames: false,
            occurs: false,
            global: false,
            indexes: Vec::new(),
            pic: None,
            object_reference: None,
            property: None,
            children: Vec::new(),
        };
        let mut closed = false;
        while j < end {
            let t = &tokens[j];
            if t.token.kind == TokenKind::Period {
                closed = true;
                j += 1;
                break;
            }
            if is(t, "PIC") || is(t, "PICTURE") {
                let at = t.origin;
                j += 1;
                if j < end && is(&tokens[j], "IS") {
                    j += 1;
                }
                let start = j;
                while j < end && self.pic_part(j) {
                    j += 1;
                }
                if j == start {
                    self.diag(at, "PICTURE clause without a picture string");
                    return (None, self.skip_entry(j, end));
                }
                entry.pic = Some(self.pic(start, j));
                continue;
            }
            if is(t, "REDEFINES") {
                entry.redefines = true;
            }
            if is(t, "RENAMES") {
                entry.renames = true;
            }
            if is(t, "OCCURS") {
                entry.occurs = true;
            }
            if is(t, "GLOBAL") {
                entry.global = true;
            }
            if is(t, "INDEXED") {
                j += 1;
                if j < end && is(&tokens[j], "BY") {
                    j += 1;
                }
                while j < end && tokens[j].token.kind == TokenKind::Word && !is_clause(&tokens[j]) {
                    entry.indexes.push(self.name(j));
                    j += 1;
                }
                continue;
            }
            // OO words are contextual: `OBJECT REFERENCE` and `PROPERTY` are clauses only under
            // an OO dialect, so a data name spelled OBJECT or PROPERTY stays a name.
            if self.dialect.oo_enabled()
                && is(t, "OBJECT")
                && is_at(tokens, j + 1, end, "REFERENCE")
            {
                j += 2;
                let mut class = None;
                if j < end
                    && tokens[j].token.kind == TokenKind::Word
                    && !is_clause(&tokens[j])
                    && !is(&tokens[j], "PROPERTY")
                {
                    class = Some(self.name(j));
                    j += 1;
                }
                entry.object_reference = Some(ObjectReference { class });
                continue;
            }
            if self.dialect.oo_enabled() && is(t, "PROPERTY") {
                j += 1;
                let mut property = Property {
                    as_name: None,
                    no_set: false,
                };
                loop {
                    if j + 1 < end
                        && is(&tokens[j], "AS")
                        && tokens[j + 1].token.kind == TokenKind::Alnum
                    {
                        property.as_name = Some(self.name(j + 1));
                        j += 2;
                    } else if is_at(tokens, j, end, "WITH")
                        && is_at(tokens, j + 1, end, "NO")
                        && is_at(tokens, j + 2, end, "SET")
                    {
                        property.no_set = true;
                        j += 3;
                    } else {
                        break;
                    }
                }
                entry.property = Some(property);
                continue;
            }
            j += 1;
        }
        if !closed {
            let at = self.origin(i);
            self.diag(at, "data entry is not ended by a period");
        }
        entry.span.end = self.origin(j - 1);
        (Some(entry), j)
    }

    /// Whether token `j` continues a PICTURE string: a picture word, a number, parentheses or a
    /// symbol. A comma continues it only between two picture tokens with no space (`ZZ,ZZ9`).
    fn pic_part(&self, j: usize) -> bool {
        let tokens = self.tokens;
        let t = &tokens[j];
        match t.token.kind {
            TokenKind::Word => !is_clause(t),
            TokenKind::Numeric | TokenKind::LParen | TokenKind::RParen | TokenKind::Symbol(_) => {
                true
            }
            TokenKind::Comma => {
                let joined =
                    |a: &Mapped, b: &Mapped| a.token.offset + a.token.text.len() == b.token.offset;
                j > 0
                    && j + 1 < tokens.len()
                    && joined(&tokens[j - 1], t)
                    && joined(t, &tokens[j + 1])
                    && matches!(
                        tokens[j + 1].token.kind,
                        TokenKind::Word | TokenKind::Numeric
                    )
                    && self.pic_part(j + 1)
            }
            _ => false,
        }
    }

    /// The PICTURE string over tokens `start..end`, checked against the dialect's PIC limit.
    fn pic(&mut self, start: usize, end: usize) -> Pic {
        let tokens = self.tokens;
        let text: String = tokens[start..end]
            .iter()
            .map(|t| t.token.text.as_str())
            .collect();
        let len = text.chars().count();
        let limit = self.dialect.limits().pic;
        if len > limit {
            let at = self.origin(start);
            self.diag(
                at,
                &format!("PICTURE of {len} characters is over the dialect limit of {limit}"),
            );
        }
        Pic {
            text,
            span: Span {
                start: self.origin(start),
                end: self.origin(end - 1),
            },
        }
    }
}

/// Whether `t` is the word `word` (case-insensitive).
fn is(t: &Mapped, word: &str) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
        && t.token.text.eq_ignore_ascii_case(word)
}

/// Whether index `j` is before `end` and holds the word `word`.
fn is_at(tokens: &[Mapped], j: usize, end: usize, word: &str) -> bool {
    j < end && is(&tokens[j], word)
}

fn is_clause(t: &Mapped) -> bool {
    CLAUSE_WORDS.iter().any(|word| is(t, word))
}

/// The level number of `t` when it is one a data description takes: 01-49, 66, 77 or 88.
fn level_of(t: &Mapped) -> Option<u8> {
    let text = t.token.text.as_str();
    if t.token.kind != TokenKind::Numeric || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let level: u8 = text.parse().ok()?;
    matches!(level, 1..=49 | 66 | 77 | 88).then_some(level)
}

/// The section a `<word> SECTION.` header at `i` opens: `Some(kind)` for a data section, and
/// `Some(None)` for an opaque one. `None` when no header starts at `i`.
fn section_header(tokens: &[Mapped], i: usize) -> Option<Option<DataSectionKind>> {
    let word = tokens.get(i)?;
    let section = tokens.get(i + 1)?;
    let period = tokens.get(i + 2)?;
    if !is(section, "SECTION") || period.token.kind != TokenKind::Period {
        return None;
    }
    SECTIONS
        .iter()
        .find(|(name, _)| is(word, name))
        .map(|(_, kind)| *kind)
}

/// Adds `entry` under the open record it belongs to. A level-66 rename belongs to the record
/// that is open, a level-88 condition name to the item it follows.
fn place(stack: &mut Vec<DataEntry>, roots: &mut Vec<DataEntry>, entry: DataEntry) {
    match entry.level {
        66 => {
            // Fold the open items above the record first, so the rename follows them in source order.
            if let Some(open) = stack.get(1).map(|open| open.level) {
                close(stack, roots, open);
            }
            match stack.first_mut() {
                Some(record) => record.children.push(entry),
                None => roots.push(entry),
            }
        }
        88 => attach(stack, roots, entry),
        77 => {
            close(stack, roots, 0);
            roots.push(entry);
        }
        level => {
            close(stack, roots, level);
            stack.push(entry);
        }
    }
}

/// Pops the open entries at `level` or deeper, each into its parent (or the section's roots).
fn close(stack: &mut Vec<DataEntry>, roots: &mut Vec<DataEntry>, level: u8) {
    while let Some(done) = stack.pop_if(|top| top.level >= level) {
        attach(stack, roots, done);
    }
}

fn attach(stack: &mut [DataEntry], roots: &mut Vec<DataEntry>, entry: DataEntry) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(entry),
        None => roots.push(entry),
    }
}
