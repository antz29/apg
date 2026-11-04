//! Tokens for the COBOL frontend: words, literals, numbers, separators and pseudo-text,
//! read through the source layout. Positions refer to the original text.

use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::format::Layout;

/// What a token is. `Reserved` is a word the dialect reserves (see `Dialect::is_reserved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Reserved,
    Alnum,
    National,
    Numeric,
    Period,
    Comma,
    Semicolon,
    LParen,
    RParen,
    PseudoDelim,
    Symbol(char),
}

/// A token. `text` is the word or number as written, the value of a literal (doubled quotes
/// collapsed, continuations joined) or the punctuation itself. `offset` is the byte offset of
/// the first character in the original text; `line` and `col` are 1-based (`col` counts
/// characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub offset: usize,
    pub line: usize,
    pub col: usize,
}

/// A forward-only cursor over one file's tokens, consumed by the parser.
#[derive(Debug)]
pub struct Cursor {
    tokens: Vec<Token>,
    pos: usize,
}

impl Cursor {
    pub fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// The token `n` places after the next one (`peek_nth(0)` is `peek`).
    pub fn peek_nth(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.pos + n)
    }

    pub fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }
}

#[derive(Debug, Clone, Copy)]
struct Ch {
    c: char,
    offset: usize,
    line: usize,
    col: usize,
}

/// The program text as the layout reads it: each character, and a marker at the start of every
/// line read (`true` when the line continues the previous one).
#[derive(Debug, Clone, Copy)]
enum Unit {
    Char(Ch),
    Line(bool),
}

/// Splits the text into units by the layout's column rules. Fixed layout drops columns 1-6 and
/// 73-80, comment lines (`*` and `/` page eject, and `D` as a debugging line) and `>>` directive
/// lines; an `-` indicator marks a continuation. Free layout keeps every line whole.
fn units(text: &str, layout: Layout) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut base = 0;
    for (n, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let chars: Vec<Ch> = line
            .char_indices()
            .enumerate()
            .map(|(k, (b, c))| Ch {
                c,
                offset: base + b,
                line: n + 1,
                col: k + 1,
            })
            .collect();
        base += raw.len() + 1;
        let (skip, continued, body): (bool, bool, Vec<Ch>) = match layout {
            Layout::Free => (false, false, chars),
            Layout::Source(_) => {
                let indicator = chars.get(6).map_or(' ', |ch| ch.c);
                let mut body: Vec<Ch> = chars
                    .into_iter()
                    .filter(|ch| (8..=72).contains(&ch.col))
                    .collect();
                if indicator == '-' {
                    // A continuation's leading blanks are insignificant in every context.
                    let blanks = body.iter().take_while(|ch| ch.c == ' ').count();
                    body.drain(..blanks);
                }
                (matches!(indicator, '*' | '/' | 'D'), indicator == '-', body)
            }
        };
        let body_text: String = body.iter().map(|ch| ch.c).collect();
        if skip || body_text.trim_start().starts_with(">>") {
            continue;
        }
        units.push(Unit::Line(continued));
        units.extend(body.into_iter().map(Unit::Char));
    }
    units
}

/// Lexes `text` read through `layout` under `dialect`. Returns the token cursor and the
/// diagnostics (word and literal lengths over the dialect limits, unterminated literals).
pub fn lex(file: &str, text: &str, layout: Layout, dialect: Dialect) -> (Cursor, Vec<Diagnostic>) {
    let mut lexer = Lexer {
        file: file.to_string(),
        dialect,
        free: layout == Layout::Free,
        units: units(text, layout),
        i: 0,
        tokens: Vec::new(),
        diags: Vec::new(),
    };
    lexer.run();
    (
        Cursor {
            tokens: lexer.tokens,
            pos: 0,
        },
        lexer.diags,
    )
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

struct Lexer {
    file: String,
    dialect: Dialect,
    free: bool,
    units: Vec<Unit>,
    i: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
}

impl Lexer {
    fn run(&mut self) {
        while self.i < self.units.len() {
            match self.peek() {
                Some(ch) => self.token(ch),
                None => self.i += 1,
            }
        }
    }

    /// The character at unit `i`, looking past continuation markers; `None` at a line break.
    fn peek_from(&self, mut i: usize) -> Option<Ch> {
        loop {
            match self.units.get(i)? {
                Unit::Char(ch) => return Some(*ch),
                Unit::Line(true) => i += 1,
                Unit::Line(false) => return None,
            }
        }
    }

    /// The index just past the character found at or after `i`.
    fn next_index(&self, mut i: usize) -> usize {
        while matches!(self.units.get(i), Some(Unit::Line(true))) {
            i += 1;
        }
        i + 1
    }

    /// The character `n` places ahead of the next one, within the same line or continuation.
    /// Blanks left before a continuation are passed over, as `join_continuation` passes them.
    fn ahead(&self, n: usize) -> Option<char> {
        let mut i = self.i;
        for _ in 0..n {
            self.peek_from(i)?;
            let next = self.next_index(i);
            i = self.before_continuation(next).unwrap_or(next);
        }
        self.peek_from(i).map(|ch| ch.c)
    }

    /// The `Line(true)` marker that the blanks from `i` lead to, when a continuation follows them.
    fn before_continuation(&self, mut i: usize) -> Option<usize> {
        let start = i;
        while matches!(self.units.get(i), Some(Unit::Char(ch)) if ch.c == ' ') {
            i += 1;
        }
        (i > start && matches!(self.units.get(i), Some(Unit::Line(true)))).then_some(i)
    }

    fn peek(&self) -> Option<Ch> {
        self.peek_from(self.i)
    }

    fn bump(&mut self) {
        self.i = self.next_index(self.i);
    }

    fn token(&mut self, ch: Ch) {
        match ch.c {
            c if c.is_whitespace() => self.bump(),
            '*' if self.free && self.ahead(1) == Some('>') => self.skip_line(),
            '"' | '\'' => self.literal(ch.c, ch, TokenKind::Alnum),
            c if c.is_ascii_alphabetic() => {
                let text = self.take_word();
                self.word(ch, text);
            }
            c if c.is_ascii_digit() => self.digits(ch),
            '+' | '-' if self.starts_number(1) => {
                self.bump();
                let text = self.numeric(ch.c.to_string());
                self.push(TokenKind::Numeric, text, ch);
            }
            '.' if self.starts_number(0) => {
                let text = self.numeric(String::new());
                self.push(TokenKind::Numeric, text, ch);
            }
            '=' if self.ahead(1) == Some('=') => {
                self.bump();
                self.bump();
                self.push(TokenKind::PseudoDelim, "==".to_string(), ch);
            }
            ',' | ';' if self.ahead(1).is_none_or(char::is_whitespace) => self.bump(),
            '.' => self.single(TokenKind::Period, ch),
            ',' => self.single(TokenKind::Comma, ch),
            ';' => self.single(TokenKind::Semicolon, ch),
            '(' => self.single(TokenKind::LParen, ch),
            ')' => self.single(TokenKind::RParen, ch),
            c => self.single(TokenKind::Symbol(c), ch),
        }
    }

    fn single(&mut self, kind: TokenKind, ch: Ch) {
        self.bump();
        self.push(kind, ch.c.to_string(), ch);
    }

    fn push(&mut self, kind: TokenKind, text: String, at: Ch) {
        self.tokens.push(Token {
            kind,
            text,
            offset: at.offset,
            line: at.line,
            col: at.col,
        });
    }

    fn diag(&mut self, at: Ch, message: String) {
        self.diags.push(Diagnostic {
            file: self.file.clone(),
            line: at.line,
            col: at.col,
            message,
        });
    }

    /// Free-format `*>`: the rest of the line is a comment.
    fn skip_line(&mut self) {
        while matches!(self.units.get(self.i), Some(Unit::Char(_))) {
            self.i += 1;
        }
    }

    /// Moves past the blanks, continuation marker and leading blanks a word, number or PICTURE
    /// string runs across. Blanks not followed by a continuation are left in place.
    fn join_continuation(&mut self) {
        let mut i = self.i;
        while matches!(self.units.get(i), Some(Unit::Char(ch)) if ch.c == ' ') {
            i += 1;
        }
        if !matches!(self.units.get(i), Some(Unit::Line(true))) {
            return;
        }
        i += 1;
        while matches!(self.units.get(i), Some(Unit::Char(ch)) if ch.c == ' ') {
            i += 1;
        }
        self.i = i;
    }

    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> String {
        let mut text = String::new();
        loop {
            self.join_continuation();
            let Some(c) = self.ahead(0).filter(|c| keep(*c)) else {
                break;
            };
            text.push(c);
            self.bump();
        }
        text
    }

    /// Whether the characters `n` ahead start a number: digits, or a decimal point then a digit.
    fn starts_number(&self, n: usize) -> bool {
        match self.ahead(n) {
            Some(c) if c.is_ascii_digit() => true,
            Some('.') => self.ahead(n + 1).is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        }
    }

    /// Appends the digits and an optional decimal part to `text`.
    fn numeric(&mut self, mut text: String) -> String {
        text.push_str(&self.take_while(|c| c.is_ascii_digit()));
        if self.ahead(0) == Some('.') && self.ahead(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            text.push('.');
            text.push_str(&self.take_while(|c| c.is_ascii_digit()));
        }
        text
    }

    /// Whether the `.` next is inside a word, number or PICTURE string: a separator period is
    /// one followed by a space, the end of the line or the end of the text.
    fn inner_dot(&self) -> bool {
        self.ahead(0) == Some('.') && self.ahead(1).is_some_and(|c| !c.is_whitespace())
    }

    /// Moves past the blanks a PICTURE clause or a separator may hold.
    fn skip_spaces(&mut self) {
        while self.ahead(0) == Some(' ') {
            self.bump();
        }
    }

    /// The PICTURE string after `PIC`/`PICTURE [IS]`: one token up to the next space. An inner
    /// period stays in; a final period before a space or the end of the line is a separator.
    fn picture(&mut self) {
        self.skip_spaces();
        if self.ahead(0).is_some_and(|c| c.eq_ignore_ascii_case(&'I'))
            && self.ahead(1).is_some_and(|c| c.eq_ignore_ascii_case(&'S'))
            && self.ahead(2).is_none_or(char::is_whitespace)
        {
            self.bump();
            self.bump();
            self.skip_spaces();
        }
        let Some(start) = self.peek() else { return };
        let mut text = String::new();
        loop {
            self.join_continuation();
            match self.ahead(0) {
                Some(c) if !c.is_whitespace() && c != '=' && (c != '.' || self.inner_dot()) => {
                    text.push(c);
                    self.bump();
                }
                _ => break,
            }
        }
        if !text.is_empty() {
            self.push(TokenKind::Word, text, start);
        }
    }

    /// A word, number or PICTURE string: word characters and inner periods.
    fn take_word(&mut self) -> String {
        let mut text = String::new();
        loop {
            self.join_continuation();
            let Some(c) = self
                .ahead(0)
                .filter(|&c| is_word_char(c) || self.inner_dot())
            else {
                break;
            };
            text.push(c);
            self.bump();
        }
        text
    }

    /// A run starting with a digit is a number when it is digits with at most one decimal
    /// point, else a word (a PICTURE such as `9.X`).
    fn digits(&mut self, ch: Ch) {
        let text = self.take_word();
        let numeric = text.matches('.').count() <= 1
            && text
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        if numeric {
            self.push(TokenKind::Numeric, text, ch);
        } else {
            self.word(ch, text);
        }
    }

    fn word(&mut self, ch: Ch, text: String) {
        match self.ahead(0) {
            Some(quote @ ('"' | '\'')) if text.eq_ignore_ascii_case("N") => {
                self.literal(quote, ch, TokenKind::National);
            }
            _ => {
                let kind = if self.dialect.is_reserved(&text) {
                    TokenKind::Reserved
                } else {
                    TokenKind::Word
                };
                let limit = self.dialect.limits().word;
                let len = text.chars().count();
                if len > limit {
                    self.diag(
                        ch,
                        format!(
                            "word `{text}` is {len} characters, over the dialect limit of {limit}"
                        ),
                    );
                }
                let picture =
                    text.eq_ignore_ascii_case("PIC") || text.eq_ignore_ascii_case("PICTURE");
                self.push(kind, text, ch);
                if picture {
                    self.picture();
                }
            }
        }
    }

    /// The index past a continuation's reopening quote and the quote after it, when the line
    /// `Line(true)` at `self.i` opens with that pair: a quote ending the previous line then pairs
    /// with the reopening quote into one embedded quote.
    fn paired_quote_at_continuation(&self, quote: char) -> Option<usize> {
        if !matches!(self.units.get(self.i), Some(Unit::Line(true))) {
            return None;
        }
        let mut i = self.i + 1;
        while matches!(self.units.get(i), Some(Unit::Char(ch)) if ch.c == ' ') {
            i += 1;
        }
        let reopen = matches!(self.units.get(i), Some(Unit::Char(ch)) if ch.c == quote);
        let second = matches!(self.units.get(i + 1), Some(Unit::Char(ch)) if ch.c == quote);
        (reopen && second).then_some(i + 2)
    }

    /// Reads a literal whose opening `quote` is next. A doubled quote is one quote; a
    /// continuation line resumes after the quote that opens its area B.
    fn literal(&mut self, quote: char, start: Ch, kind: TokenKind) {
        self.bump();
        let mut value = String::new();
        let mut closed = false;
        loop {
            match self.units.get(self.i).copied() {
                Some(Unit::Char(ch)) if ch.c == quote => {
                    self.i += 1;
                    if matches!(self.units.get(self.i), Some(Unit::Char(next)) if next.c == quote) {
                        value.push(quote);
                        self.i += 1;
                    } else if let Some(resume) = self.paired_quote_at_continuation(quote) {
                        value.push(quote);
                        self.i = resume;
                    } else {
                        closed = true;
                        break;
                    }
                }
                Some(Unit::Char(ch)) => {
                    value.push(ch.c);
                    self.i += 1;
                }
                Some(Unit::Line(true)) => {
                    self.i += 1;
                    while matches!(self.units.get(self.i), Some(Unit::Char(ch)) if ch.c == ' ') {
                        self.i += 1;
                    }
                    if !matches!(self.units.get(self.i), Some(Unit::Char(ch)) if ch.c == quote) {
                        break;
                    }
                    self.i += 1;
                }
                Some(Unit::Line(false)) | None => break,
            }
        }
        if !closed {
            self.diag(start, "unterminated literal".to_string());
        }
        let limit = self.dialect.limits().literal;
        let len = value.chars().count();
        if len > limit {
            self.diag(
                start,
                format!("literal of {len} characters is over the dialect limit of {limit}"),
            );
        }
        self.push(kind, value, start);
    }
}
