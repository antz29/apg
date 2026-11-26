//! Bicep source parsing: a newline-significant lexer and a hand-written
//! recursive-descent parser over one decoded file. Every span is a raw on-disk
//! byte range with 1-based inclusive lines. A malformed statement is reported
//! and skipped to its line's end, so the rest of the file still yields its
//! declarations. Example: `resource stg 'Microsoft.Storage/storageAccounts@2023-01-01' = {`
//! parses to one `Resource` declaration named `stg`.

use crate::source::{Decoded, Span};

/// A parsed Bicep file: its top-level declarations, then every syntax error in source order.
#[derive(Debug)]
pub struct File {
    pub decls: Vec<Decl>,
    pub errors: Vec<ParseError>,
}

/// The declaration keywords the scanner models. A `.bicepparam` assignment
/// `param x = 'v'` is a `Param` with no type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Param,
    Var,
    Resource,
    Module,
    Output,
    Type,
    Func,
    Import,
    Using,
    Extends,
}

/// One declaration. `refs` are its own body's identifier references (a child
/// resource's references sit on the child); `children` are the resources nested
/// in its body, at any depth.
#[derive(Debug)]
pub struct Decl {
    pub kind: Kind,
    /// Absent for `using`, `extends` and `import` statements.
    pub name: Option<Name>,
    pub span: Span,
    /// A resource's type string, a module's path, an import's source, or a
    /// using/extends path; `None` only for `using none`. Example:
    /// `'Microsoft.Storage/storageAccounts@2023-01-01'`.
    pub literal: Option<Str>,
    /// `existing` after a resource's type: `resource stg 'Microsoft.Storage/storageAccounts' existing = {`.
    pub existing: bool,
    /// The symbols an `import` binds; empty for other kinds.
    pub imports: Vec<ImportSym>,
    pub refs: Vec<Ref>,
    pub children: Vec<Decl>,
}

/// An identifier with its span.
#[derive(Debug, Clone)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

/// A string literal: its text when it has no `${…}` interpolation, else `None`.
#[derive(Debug, Clone)]
pub struct Str {
    pub value: Option<String>,
    pub span: Span,
}

/// One symbol an `import` binds. `remote` is the exported name (`None` for a
/// wildcard `import * as ns from 'x'`), `local` the name it binds (`as` alias or the same name).
#[derive(Debug, Clone)]
pub struct ImportSym {
    pub remote: Option<Name>,
    pub local: Name,
}

/// An identifier reference. `name` is the leading identifier; `accessors` are
/// the `a::b::c` names after it; `call` marks `name(...)`; `local` marks a name
/// bound by an enclosing lambda parameter or `for` variable (shadowing).
#[derive(Debug, Clone)]
pub struct Ref {
    pub name: String,
    pub span: Span,
    pub accessors: Vec<Name>,
    pub call: bool,
    pub local: bool,
}

/// A syntax error: 1-based line and column (bytes), and the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Parse a decoded Bicep file (`.bicep` or `.bicepparam`). Never panics.
pub fn parse(src: &Decoded) -> File {
    let mut lx = Lexer {
        src: &src.text,
        pos: 0,
        errors: Vec::new(),
    };
    let toks = lx.lex(false);
    let mut lines = vec![0];
    lines.extend(src.text.match_indices('\n').map(|(i, _)| i + 1));
    let mut p = Parser {
        src,
        lines: &lines,
        toks,
        pos: 0,
        last_end: 0,
        refs: Vec::new(),
        binders: Vec::new(),
        nl_free: Vec::new(),
    };
    let mut errors: Vec<ParseError> = lx
        .errors
        .iter()
        .map(|(at, msg)| p.error_at(*at, msg))
        .collect();
    let mut decls = Vec::new();
    loop {
        p.skip_nl();
        if matches!(p.cur(), Tok::Eof) {
            break;
        }
        let (refs, binders, nl) = (p.refs.len(), p.binders.len(), p.nl_free.len());
        match p.statement() {
            Ok(Some(decl)) => decls.push(decl),
            Ok(None) => {}
            Err(e) => {
                p.refs.truncate(refs);
                p.binders.truncate(binders);
                p.nl_free.truncate(nl);
                errors.push(e);
                p.recover();
            }
        }
    }
    errors.sort_by_key(|e| (e.line, e.column));
    File { decls, errors }
}

// ---------------------------------------------------------------- lexer

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
enum Tok {
    Ident(String),
    Num,
    Str(Vec<Part>),
    Op(&'static str),
    Newline,
    Eof,
}

/// A string literal segment: literal text, or the tokens of a `${…}` expression.
#[derive(Debug, Clone)]
enum Part {
    Text(String),
    Expr(Vec<Token>),
}

/// Two-character and three-character operators, tried before `SINGLE`.
const OPS: &[&str] = &[
    "...", "==", "!=", "=~", "!~", "<=", ">=", "&&", "||", "??", "?.", "::", "=>",
];
const SINGLE: &str = "()[]{},:.=<>+-*/%!?|@";

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    errors: Vec<(usize, String)>,
}

impl Lexer<'_> {
    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn span_while(&self, keep: impl Fn(char) -> bool) -> usize {
        self.rest()
            .find(|ch: char| !keep(ch))
            .unwrap_or(self.rest().len())
    }

    fn error(&mut self, at: usize, msg: &str) {
        self.errors.push((at, msg.to_string()));
    }

    /// Lex to end of input, or (when `nested`) to the `}` closing a `${…}`.
    /// Newlines are tokens; `//` and `/* */` comments are skipped.
    fn lex(&mut self, nested: bool) -> Vec<Token> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        while let Some(c) = self.rest().chars().next() {
            let start = self.pos;
            if c == '\n' {
                self.pos += 1;
                out.push(Token {
                    tok: Tok::Newline,
                    start,
                    end: self.pos,
                });
            } else if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else if self.rest().starts_with("//") {
                self.pos += self.span_while(|ch| ch != '\n');
            } else if self.rest().starts_with("/*") {
                let len = match self.rest()[2..].find("*/") {
                    Some(i) => i + 4,
                    None => {
                        self.error(start, "unterminated comment");
                        self.rest().len()
                    }
                };
                if self.rest()[..len].contains('\n') {
                    out.push(Token {
                        tok: Tok::Newline,
                        start,
                        end: start + len,
                    });
                }
                self.pos += len;
            } else if c == '\'' {
                let parts = self.string();
                out.push(Token {
                    tok: Tok::Str(parts),
                    start,
                    end: self.pos,
                });
            } else if c.is_ascii_digit() {
                self.pos += self.span_while(|ch| ch.is_ascii_digit());
                if self.rest().starts_with('.')
                    && self.rest()[1..].starts_with(|ch: char| ch.is_ascii_digit())
                {
                    self.pos += 1 + self.span_while(|ch| ch.is_ascii_digit());
                }
                out.push(Token {
                    tok: Tok::Num,
                    start,
                    end: self.pos,
                });
            } else if c.is_alphabetic() || c == '_' {
                let n = self.span_while(|ch| ch.is_alphanumeric() || ch == '_');
                let word = self.rest()[..n].to_string();
                self.pos += n;
                out.push(Token {
                    tok: Tok::Ident(word),
                    start,
                    end: self.pos,
                });
            } else if nested && c == '}' && depth == 0 {
                self.pos += 1;
                break;
            } else if let Some(op) = OPS.iter().find(|op| self.rest().starts_with(**op)).copied() {
                self.pos += op.len();
                out.push(Token {
                    tok: Tok::Op(op),
                    start,
                    end: self.pos,
                });
            } else if let Some(i) = SINGLE.find(c) {
                match c {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                self.pos += 1;
                out.push(Token {
                    tok: Tok::Op(&SINGLE[i..i + 1]),
                    start,
                    end: self.pos,
                });
            } else {
                self.error(start, "unexpected character");
                self.pos += c.len_utf8();
            }
        }
        out.push(Token {
            tok: Tok::Eof,
            start: self.pos,
            end: self.pos,
        });
        out
    }

    /// A `'…'` or `'''…'''` literal with escapes and `${…}` interpolation.
    fn string(&mut self) -> Vec<Part> {
        let start = self.pos;
        let multi = self.rest().starts_with("'''");
        let close = if multi { "'''" } else { "'" };
        self.pos += close.len();
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            if self.rest().is_empty() || (!multi && self.rest().starts_with('\n')) {
                self.error(start, "unterminated string");
                break;
            }
            if self.rest().starts_with(close) {
                self.pos += close.len();
                break;
            }
            if self.rest().starts_with("${") {
                self.pos += 2;
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(Part::Expr(self.lex(true)));
                continue;
            }
            let Some(c) = self.rest().chars().next() else {
                break;
            };
            if c == '\\' {
                self.pos += 1;
                self.escape(start, &mut text);
            } else {
                text.push(c);
                self.pos += c.len_utf8();
            }
        }
        if !text.is_empty() || parts.is_empty() {
            parts.push(Part::Text(text));
        }
        parts
    }

    fn escape(&mut self, start: usize, text: &mut String) {
        let Some(c) = self.rest().chars().next() else {
            self.error(start, "unterminated escape");
            return;
        };
        self.pos += c.len_utf8();
        match c {
            'n' => text.push('\n'),
            'r' => text.push('\r'),
            't' => text.push('\t'),
            '\\' | '\'' | '$' => text.push(c),
            'u' => match self.unicode() {
                Some(ch) => text.push(ch),
                None => self.error(start, "invalid unicode escape"),
            },
            _ => {
                self.error(start, "unrecognized escape");
                text.push(c);
            }
        }
    }

    /// The `{hex}` of a `\u{…}` escape, after the `u`.
    fn unicode(&mut self) -> Option<char> {
        let body = self.rest().strip_prefix('{')?;
        let close = body.find('}')?;
        let ch = char::from_u32(u32::from_str_radix(&body[..close], 16).ok()?)?;
        self.pos += close + 2;
        Some(ch)
    }
}

// ---------------------------------------------------------------- parser

type PResult<T> = Result<T, ParseError>;

/// Binary operators by ascending precedence tier.
const LEVELS: &[&[&str]] = &[
    &["??", "||", "|"],
    &["&&"],
    &["==", "!=", "=~", "!~"],
    &["<", "<=", ">", ">="],
    &["+", "-"],
    &["*", "/", "%"],
];

struct Parser<'a> {
    src: &'a Decoded,
    /// Byte offset of each line's start; `lines[0] == 0`.
    lines: &'a [usize],
    toks: Vec<Token>,
    pos: usize,
    last_end: usize,
    refs: Vec<Ref>,
    /// Lambda parameter and `for` variable names in scope.
    binders: Vec<String>,
    /// One entry per open bracket: `true` makes newlines insignificant (inside parentheses).
    nl_free: Vec<bool>,
}

impl Parser<'_> {
    // ---- token access

    fn skip_nl(&mut self) {
        // The last token is always `Eof`, which is never skipped.
        while matches!(self.toks[self.pos].tok, Tok::Newline) {
            self.pos += 1;
        }
    }

    fn cur(&mut self) -> &Tok {
        if self.nl_free.last() == Some(&true) {
            self.skip_nl();
        }
        &self.toks[self.pos].tok
    }

    /// Consume the current token (never past `Eof`) and return its start.
    fn bump(&mut self) -> usize {
        self.cur();
        let t = &self.toks[self.pos];
        let (start, end) = (t.start, t.end);
        if !matches!(t.tok, Tok::Eof) {
            self.pos += 1;
        }
        self.last_end = end;
        start
    }

    fn cur_start(&mut self) -> usize {
        self.cur();
        self.toks[self.pos].start
    }

    fn kind_op(&mut self) -> Option<&'static str> {
        match self.cur() {
            Tok::Op(o) => Some(*o),
            _ => None,
        }
    }

    fn is_op(&mut self, op: &str) -> bool {
        self.kind_op() == Some(op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> PResult<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            self.err(&format!("expected `{op}`"))
        }
    }

    fn is_ident(&mut self, word: &str) -> bool {
        matches!(self.cur(), Tok::Ident(s) if s == word)
    }

    fn eat_ident(&mut self, word: &str) -> bool {
        if self.is_ident(word) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_ident(&mut self, word: &str) -> PResult<()> {
        if self.eat_ident(word) {
            Ok(())
        } else {
            self.err(&format!("expected `{word}`"))
        }
    }

    fn ident_text(&mut self) -> Option<String> {
        match self.cur() {
            Tok::Ident(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn at_eol(&mut self) -> bool {
        matches!(self.cur(), Tok::Newline | Tok::Eof)
    }

    fn at_newline(&self) -> bool {
        matches!(self.toks[self.pos].tok, Tok::Newline)
    }

    /// The separator after a list element: `,`, the closer, or a line break.
    fn separator(&mut self, close: &str) -> PResult<()> {
        if self.eat_op(",") || self.is_op(close) || self.at_newline() {
            Ok(())
        } else {
            self.err("expected a line break, `,` or the closing bracket")
        }
    }

    // ---- errors and spans

    fn err<T>(&mut self, msg: &str) -> PResult<T> {
        let at = self.cur_start();
        Err(self.error_at(at, msg))
    }

    fn error_at(&self, at: usize, msg: &str) -> ParseError {
        let line = self.line_of(at);
        ParseError {
            line,
            column: at - self.lines[line - 1] + 1,
            message: msg.to_string(),
        }
    }

    fn line_of(&self, at: usize) -> usize {
        self.lines.partition_point(|&s| s <= at)
    }

    /// The raw-offset span of decoded bytes `start..end`.
    fn sp(&self, start: usize, end: usize) -> Span {
        let last = if end > start { end - 1 } else { start };
        Span {
            start: self.src.raw_offset(start),
            end: self.src.raw_offset(end),
            start_line: self.line_of(start),
            end_line: self.line_of(last),
        }
    }

    fn push_ref(&mut self, name: &str, start: usize, end: usize, call: bool) -> usize {
        let local = self.binders.iter().any(|b| b == name);
        let span = self.sp(start, end);
        self.refs.push(Ref {
            name: name.to_string(),
            span,
            accessors: Vec::new(),
            call,
            local,
        });
        self.refs.len() - 1
    }

    /// A declaration spanning `start` to the last consumed token, owning the
    /// references recorded since `mark`.
    fn decl(&mut self, kind: Kind, name: Option<Name>, mark: usize, start: usize) -> Decl {
        Decl {
            kind,
            name,
            span: self.sp(start, self.last_end),
            literal: None,
            existing: false,
            imports: Vec::new(),
            refs: self.refs.split_off(mark),
            children: Vec::new(),
        }
    }

    /// Error recovery: skip to the next newline at bracket depth zero (consuming it).
    fn recover(&mut self) {
        let mut depth = 0i32;
        loop {
            match &self.toks[self.pos].tok {
                Tok::Eof => return,
                Tok::Newline if depth <= 0 => {
                    self.pos += 1;
                    return;
                }
                Tok::Op("(" | "[" | "{") => depth += 1,
                Tok::Op(")" | "]" | "}") => depth -= 1,
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn name(&mut self) -> PResult<Name> {
        let start = self.cur_start();
        let Some(text) = self.ident_text() else {
            return self.err("expected a name");
        };
        self.bump();
        Ok(Name {
            text,
            span: self.sp(start, self.last_end),
        })
    }

    /// A string literal; `${…}` expressions inside it are parsed for references.
    fn string(&mut self) -> PResult<Str> {
        let start = self.cur_start();
        let Tok::Str(parts) = self.cur().clone() else {
            return self.err("expected a string");
        };
        self.bump();
        let span = self.sp(start, self.last_end);
        let value = literal(&parts);
        for part in parts {
            if let Part::Expr(toks) = part {
                self.interpolate(toks)?;
            }
        }
        Ok(Str { value, span })
    }

    /// Parse the tokens of one `${…}` as an expression, sharing this parser's references.
    fn interpolate(&mut self, toks: Vec<Token>) -> PResult<()> {
        let mut sub = Parser {
            src: self.src,
            lines: self.lines,
            toks,
            pos: 0,
            last_end: 0,
            refs: std::mem::take(&mut self.refs),
            binders: self.binders.clone(),
            nl_free: vec![true],
        };
        let result = match sub.expr() {
            Ok(()) if matches!(sub.cur(), Tok::Eof) => Ok(()),
            Ok(()) => sub.err("unexpected token in interpolation"),
            Err(e) => Err(e),
        };
        self.refs = sub.refs;
        result
    }

    // ---- statements

    fn statement(&mut self) -> PResult<Option<Decl>> {
        let mark = self.refs.len();
        let start = self.cur_start();
        self.decorators()?;
        let Some(word) = self.ident_text() else {
            return self.err("expected a declaration");
        };
        let decl = match word.as_str() {
            "metadata" | "targetScope" | "extension" | "assert" | "test" => {
                self.recover();
                self.refs.truncate(mark);
                return Ok(None);
            }
            "param" => self.typed(Kind::Param, mark, start)?,
            "var" => self.typed(Kind::Var, mark, start)?,
            "output" => self.typed(Kind::Output, mark, start)?,
            "type" => self.type_decl(mark, start)?,
            "func" => self.func(mark, start)?,
            "resource" => self.literal_decl(Kind::Resource, mark, start)?,
            "module" => self.literal_decl(Kind::Module, mark, start)?,
            "import" => self.import(mark, start)?,
            "using" => self.using(mark, start)?,
            "extends" => self.extends(mark, start)?,
            other => return self.err(&format!("unexpected `{other}`")),
        };
        self.end_stmt()?;
        Ok(Some(decl))
    }

    fn end_stmt(&mut self) -> PResult<()> {
        if self.at_eol() {
            Ok(())
        } else {
            self.err("unexpected token after declaration")
        }
    }

    /// Zero or more `@name`, `@a.name` or `@name(args)` decorators, each followed by line breaks.
    fn decorators(&mut self) -> PResult<()> {
        while self.is_op("@") {
            self.bump();
            self.name()?;
            while self.eat_op(".") {
                self.name()?;
            }
            if self.is_op("(") {
                self.args()?;
            }
            self.skip_nl();
        }
        Ok(())
    }

    /// `param`, `var`, `output`: `name [type] [= value]`. A `.bicepparam`
    /// assignment is `param name = value`.
    fn typed(&mut self, kind: Kind, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let name = self.name()?;
        if !self.is_op("=") && !self.at_eol() {
            self.type_expr()?;
        }
        if self.eat_op("=") {
            self.expr()?;
        }
        Ok(self.decl(kind, Some(name), mark, start))
    }

    fn type_decl(&mut self, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let name = self.name()?;
        self.expect_op("=")?;
        self.type_expr()?;
        Ok(self.decl(Kind::Type, Some(name), mark, start))
    }

    /// `func name(p type, …) [returnType] => body`; the parameters bind in the body.
    fn func(&mut self, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let name = self.name()?;
        self.expect_op("(")?;
        self.nl_free.push(true);
        let mut params = Vec::new();
        if !self.is_op(")") {
            loop {
                params.push(self.name()?.text);
                self.type_expr()?;
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        self.nl_free.pop();
        if !self.is_op("=>") {
            self.type_expr()?;
        }
        self.expect_op("=>")?;
        let base = self.binders.len();
        self.binders.extend(params);
        self.expr()?;
        self.binders.truncate(base);
        Ok(self.decl(Kind::Func, Some(name), mark, start))
    }

    /// `resource`/`module`: `name 'literal' [existing] = value`. Also a child resource in a body.
    fn literal_decl(&mut self, kind: Kind, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let name = self.name()?;
        let literal = self.string()?;
        let existing = self.eat_ident("existing");
        self.expect_op("=")?;
        let mut children = Vec::new();
        self.decl_value(&mut children)?;
        let mut d = self.decl(kind, Some(name), mark, start);
        d.literal = Some(literal);
        d.existing = existing;
        d.children = children;
        Ok(d)
    }

    /// `import { a, b as c } from 'src'` or `import * as ns from 'src'`.
    fn import(&mut self, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let mut imports = Vec::new();
        if self.eat_op("*") {
            self.expect_ident("as")?;
            let local = self.name()?;
            imports.push(ImportSym {
                remote: None,
                local,
            });
        } else if self.eat_op("{") {
            self.nl_free.push(true);
            while !self.is_op("}") {
                let remote = self.name()?;
                let local = if self.eat_ident("as") {
                    self.name()?
                } else {
                    remote.clone()
                };
                imports.push(ImportSym {
                    remote: Some(remote),
                    local,
                });
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op("}")?;
            self.nl_free.pop();
        } else {
            return self.err("expected `*` or `{` after `import`");
        }
        self.expect_ident("from")?;
        let literal = self.string()?;
        let mut d = self.decl(Kind::Import, None, mark, start);
        d.imports = imports;
        d.literal = Some(literal);
        Ok(d)
    }

    /// `using 'main.bicep'` or `using none` (no literal).
    fn using(&mut self, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let literal = if self.eat_ident("none") {
            None
        } else {
            Some(self.string()?)
        };
        let mut d = self.decl(Kind::Using, None, mark, start);
        d.literal = literal;
        Ok(d)
    }

    fn extends(&mut self, mark: usize, start: usize) -> PResult<Decl> {
        self.bump();
        let literal = self.string()?;
        let mut d = self.decl(Kind::Extends, None, mark, start);
        d.literal = Some(literal);
        Ok(d)
    }

    // ---- bodies and values

    /// The value after `=` in a resource or module: an optional `if (cond)`, then a
    /// value (object, `[for …]` array or expression). Child resources go to `out`.
    fn decl_value(&mut self, out: &mut Vec<Decl>) -> PResult<()> {
        if self.eat_ident("if") {
            self.paren_expr()?;
        }
        self.value(out)
    }

    fn value(&mut self, out: &mut Vec<Decl>) -> PResult<()> {
        match self.kind_op() {
            Some("{") => self.object(out),
            Some("[") => self.bracket(out),
            _ => self.expr(),
        }
    }

    /// An object body: `key: value`, `...spread`, decorated child `resource`s.
    fn object(&mut self, out: &mut Vec<Decl>) -> PResult<()> {
        self.expect_op("{")?;
        self.nl_free.push(false);
        loop {
            self.skip_nl();
            if self.eat_op(",") {
                continue;
            }
            if self.is_op("}") {
                break;
            }
            let mark = self.refs.len();
            let start = self.cur_start();
            self.decorators()?;
            if self.is_ident("resource")
                && matches!(
                    self.toks.get(self.pos + 1).map(|t| &t.tok),
                    Some(Tok::Ident(_))
                )
            {
                let child = self.literal_decl(Kind::Resource, mark, start)?;
                out.push(child);
            } else if self.eat_op("...") {
                self.expr()?;
            } else {
                self.key()?;
                self.expect_op(":")?;
                self.value(&mut Vec::new())?;
            }
            self.separator("}")?;
        }
        self.expect_op("}")?;
        self.nl_free.pop();
        Ok(())
    }

    fn key(&mut self) -> PResult<()> {
        if matches!(self.cur(), Tok::Ident(_)) {
            self.bump();
            Ok(())
        } else if matches!(self.cur(), Tok::Str(_)) {
            self.string()?;
            Ok(())
        } else {
            self.err("expected a property name")
        }
    }

    /// An array literal, or a `[for …: body]` expression.
    fn bracket(&mut self, out: &mut Vec<Decl>) -> PResult<()> {
        self.expect_op("[")?;
        self.nl_free.push(false);
        if self.is_ident("for") {
            self.for_expr(out)?;
            self.skip_nl();
        } else {
            loop {
                self.skip_nl();
                if self.eat_op(",") {
                    continue;
                }
                if self.is_op("]") {
                    break;
                }
                self.eat_op("...");
                self.value(out)?;
                self.separator("]")?;
            }
        }
        self.expect_op("]")?;
        self.nl_free.pop();
        Ok(())
    }

    /// `for x in xs: body` or `for (x, i) in xs: body`; the variables bind in the body only.
    fn for_expr(&mut self, out: &mut Vec<Decl>) -> PResult<()> {
        self.bump();
        let mut names = Vec::new();
        if self.eat_op("(") {
            loop {
                names.push(self.name()?.text);
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op(")")?;
        } else {
            names.push(self.name()?.text);
        }
        self.expect_ident("in")?;
        self.expr()?;
        self.expect_op(":")?;
        let base = self.binders.len();
        self.binders.extend(names);
        self.value(out)?;
        self.binders.truncate(base);
        Ok(())
    }

    // ---- expressions

    fn paren_expr(&mut self) -> PResult<()> {
        self.expect_op("(")?;
        self.nl_free.push(true);
        self.expr()?;
        self.expect_op(")")?;
        self.nl_free.pop();
        Ok(())
    }

    fn args(&mut self) -> PResult<()> {
        self.expect_op("(")?;
        self.nl_free.push(true);
        if !self.is_op(")") {
            loop {
                self.expr()?;
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        self.nl_free.pop();
        Ok(())
    }

    /// A type: an expression, then an optional `?` (nullable).
    fn type_expr(&mut self) -> PResult<()> {
        if self.is_ident("resource")
            && matches!(
                self.toks.get(self.pos + 1).map(|t| &t.tok),
                Some(Tok::Str(_))
            )
        {
            self.bump();
        }
        self.expr()?;
        self.eat_op("?");
        Ok(())
    }

    fn expr(&mut self) -> PResult<()> {
        self.binary(0)?;
        if self.is_op("?") && self.question_is_ternary() {
            self.bump();
            self.expr()?;
            self.expect_op(":")?;
            self.expr()?;
        }
        Ok(())
    }

    /// A `?` begins a ternary only when an operand follows it; otherwise it is a nullable type mark.
    fn question_is_ternary(&self) -> bool {
        matches!(
            self.toks.get(self.pos + 1).map(|t| &t.tok),
            Some(Tok::Ident(_) | Tok::Num | Tok::Str(_) | Tok::Op("(" | "[" | "{" | "-" | "!"))
        )
    }

    fn binary(&mut self, level: usize) -> PResult<()> {
        let Some(ops) = LEVELS.get(level) else {
            return self.unary();
        };
        self.binary(level + 1)?;
        while self.kind_op().is_some_and(|op| ops.contains(&op)) {
            self.bump();
            self.binary(level + 1)?;
        }
        Ok(())
    }

    fn unary(&mut self) -> PResult<()> {
        if matches!(self.kind_op(), Some("!" | "-")) {
            self.bump();
            return self.unary();
        }
        self.postfix()
    }

    /// A primary followed by member access, indexing, `::` accessors and `!`.
    fn postfix(&mut self) -> PResult<()> {
        let mut base = self.primary()?;
        loop {
            match self.kind_op() {
                Some("." | "?.") => {
                    self.bump();
                    self.name()?;
                    base = None;
                    if self.is_op("(") {
                        self.args()?;
                    }
                }
                Some("[") => {
                    self.bump();
                    if !self.is_op("]") {
                        self.expr()?;
                    }
                    self.expect_op("]")?;
                    base = None;
                }
                Some("::") => {
                    self.bump();
                    let accessor = self.name()?;
                    if let Some(i) = base {
                        self.refs[i].accessors.push(accessor);
                    }
                }
                Some("!") => {
                    self.bump();
                }
                _ => return Ok(()),
            }
        }
    }

    /// A literal, string, identifier reference or call, parenthesised expression,
    /// lambda, array or object. Returns the index of a reference it recorded, if any.
    fn primary(&mut self) -> PResult<Option<usize>> {
        if let Some(op) = self.kind_op() {
            match op {
                "(" if self.is_lambda() => self.lambda()?,
                "(" => self.paren_expr()?,
                "[" => self.bracket(&mut Vec::new())?,
                "{" => self.object(&mut Vec::new())?,
                _ => return self.err("expected an expression"),
            }
            return Ok(None);
        }
        match self.cur().clone() {
            Tok::Num => {
                self.bump();
                Ok(None)
            }
            Tok::Str(_) => {
                self.string()?;
                Ok(None)
            }
            Tok::Ident(word) => self.reference(&word),
            _ => self.err("expected an expression"),
        }
    }

    fn reference(&mut self, word: &str) -> PResult<Option<usize>> {
        if matches!(word, "true" | "false" | "null") {
            self.bump();
            return Ok(None);
        }
        let start = self.cur_start();
        self.bump();
        let end = self.last_end;
        let call = self.is_op("(");
        let at = self.push_ref(word, start, end, call);
        if call {
            self.args()?;
        }
        Ok(Some(at))
    }

    /// At `(`: is the matching `)` followed by `=>`?
    fn is_lambda(&self) -> bool {
        let mut depth = 0usize;
        for (i, t) in self.toks.iter().enumerate().skip(self.pos) {
            match &t.tok {
                Tok::Op("(") => depth += 1,
                Tok::Op(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.toks.get(i + 1).map(|n| &n.tok), Some(Tok::Op("=>")));
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
        }
        false
    }

    /// `(a, b) => body`; the parameters bind in the body only.
    fn lambda(&mut self) -> PResult<()> {
        self.expect_op("(")?;
        self.nl_free.push(true);
        let mut names = Vec::new();
        if !self.is_op(")") {
            loop {
                names.push(self.name()?.text);
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        self.nl_free.pop();
        self.expect_op("=>")?;
        let base = self.binders.len();
        self.binders.extend(names);
        self.expr()?;
        self.binders.truncate(base);
        Ok(())
    }
}

/// The text of a string with no interpolation, else `None`.
fn literal(parts: &[Part]) -> Option<String> {
    parts.iter().try_fold(String::new(), |mut s, p| match p {
        Part::Text(t) => {
            s.push_str(t);
            Some(s)
        }
        Part::Expr(_) => None,
    })
}
