//! PROCEDURE DIVISION parser. Reads the division's tokens into `ast` declaratives, sections,
//! paragraphs, sentences and statements. A sentence ends at its period; a statement is parsed
//! by its verb to its scope terminator, with nested statements as children. A malformed
//! sentence keeps the statements parsed before its error. Entry point:
//! [`parse_procedure_division`], called by the parser with the tokens from `PROCEDURE DIVISION`
//! to the end of the program's procedure text.

use super::exec::{exec_block_end, is_exec_block};
use crate::ast::{
    CallTarget, Name, Paragraph, ProcRange, ProcedureDivision, QualifiedName, RaiseTarget,
    Receiver, Section, Sentence, Span, Statement, Transfer,
};
use crate::diag::Diagnostic;
use crate::dialect::{Dialect, Feature};
use crate::format::Layout;
use crate::lexer::TokenKind;
use crate::source_map::{Mapped, Origin, SourceMap};

/// Original columns of area A (SPEC §3): where a fixed-layout header starts.
const AREA_A: std::ops::RangeInclusive<usize> = 8..=11;

/// Statement verbs: a token that is one of these starts a statement.
const VERBS: &[&str] = &[
    "ACCEPT",
    "ADD",
    "ALTER",
    "CALL",
    "CANCEL",
    "CLOSE",
    "COMPUTE",
    "CONTINUE",
    "DELETE",
    "DISPLAY",
    "DIVIDE",
    "ENTRY",
    "EVALUATE",
    "EXIT",
    "GENERATE",
    "GO",
    "GOBACK",
    "IF",
    "INITIALIZE",
    "INITIATE",
    "INSPECT",
    "MERGE",
    "MOVE",
    "MULTIPLY",
    "NEXT",
    "OPEN",
    "PERFORM",
    "PURGE",
    "READ",
    "RELEASE",
    "RESUME",
    "RETURN",
    "REWRITE",
    "SEARCH",
    "SET",
    "SORT",
    "START",
    "STOP",
    "STRING",
    "SUBTRACT",
    "SUPPRESS",
    "TERMINATE",
    "UNSTRING",
    "USE",
    "WRITE",
];

/// The COBOL-85 scope terminators: `END-<verb>` for each verb that takes one.
const SCOPE_TERMINATORS: &[&str] = &[
    "END-ADD",
    "END-CALL",
    "END-COMPUTE",
    "END-DELETE",
    "END-DIVIDE",
    "END-EVALUATE",
    "END-IF",
    "END-MULTIPLY",
    "END-PERFORM",
    "END-READ",
    "END-RECEIVE",
    "END-RETURN",
    "END-REWRITE",
    "END-SEARCH",
    "END-START",
    "END-STRING",
    "END-SUBTRACT",
    "END-UNSTRING",
    "END-WRITE",
];

/// Phrases that open a branch of a statement (`ON SIZE ERROR`, `AT END`, ...), each with the
/// statements it holds. A leading `NOT` negates the branch. The optional `ON`/`AT`/`KEY` words
/// are spelled out both ways; the longer form comes first so it wins.
const BRANCHES: &[&[&str]] = &[
    &["ON", "SIZE", "ERROR"],
    &["SIZE", "ERROR"],
    &["ON", "OVERFLOW"],
    &["OVERFLOW"],
    &["ON", "EXCEPTION"],
    &["EXCEPTION"],
    &["AT", "END"],
    &["END"],
    &["AT", "END-OF-PAGE"],
    &["END-OF-PAGE"],
    &["AT", "EOP"],
    &["EOP"],
    &["INVALID", "KEY"],
    &["INVALID"],
];

/// Words that are phrase keywords, class or category words, or special registers, never data
/// names in a reference. One set serves every verb: none is a data name in its operands.
const KEYWORDS: &[&str] = &[
    "ADVANCING",
    "AFTER",
    "ALL",
    "ALPHABETIC",
    "ALPHABETIC-LOWER",
    "ALPHABETIC-UPPER",
    "ALPHANUMERIC",
    "ALPHANUMERIC-EDITED",
    "ALSO",
    "AND",
    "ANY",
    "AT",
    "ASCENDING",
    "BEFORE",
    "BY",
    "CHARACTERS",
    "COLLATING",
    "CONTENT",
    "CONVERTING",
    "CORR",
    "CORRESPONDING",
    "COUNT",
    "DATA",
    "DATE",
    "DAY",
    "DAY-OF-WEEK",
    "DELIMITED",
    "DELIMITER",
    "DEPENDING",
    "DESCENDING",
    "DOWN",
    "DUPLICATES",
    "ELSE",
    "EOP",
    "EQUAL",
    "ERROR",
    "EXCEPTION",
    "EXTEND",
    "FALSE",
    "FIRST",
    "FOR",
    "FROM",
    "FUNCTION",
    "GIVING",
    "GREATER",
    "HIGH-VALUE",
    "HIGH-VALUES",
    "I-O",
    "IN",
    "INITIAL",
    "INPUT",
    "INTO",
    "INVALID",
    "IS",
    "KEY",
    "LEADING",
    "LESS",
    "LINE",
    "LINES",
    "LOCK",
    "LOW-VALUE",
    "LOW-VALUES",
    "NEGATIVE",
    "NO",
    "NOT",
    "NUMERIC",
    "NUMERIC-EDITED",
    "OF",
    "OFF",
    "ON",
    "OR",
    "ORDER",
    "OTHER",
    "OUTPUT",
    "OVERFLOW",
    "PAGE",
    "POINTER",
    "POSITIVE",
    "PROCEED",
    "PROCEDURE",
    "PROGRAM",
    "QUOTE",
    "QUOTES",
    "RECORD",
    "REEL",
    "REFERENCE",
    "REMAINDER",
    "REMOVAL",
    "REPLACING",
    "RETURNING",
    "REVERSED",
    "REWIND",
    "ROUNDED",
    "RUN",
    "SECTION",
    "SENTENCE",
    "SEQUENCE",
    "SIZE",
    "SPACE",
    "SPACES",
    "STANDARD",
    "TALLYING",
    "TEST",
    "THAN",
    "THEN",
    "THROUGH",
    "THRU",
    "TIME",
    "TIMES",
    "TO",
    "TRUE",
    "UNIT",
    "UNTIL",
    "UP",
    "UPON",
    "USING",
    "VALUE",
    "VARYING",
    "WHEN-COMPILED",
    "WITH",
    "ZERO",
    "ZEROS",
    "ZEROES",
];

/// The name of the nameless leading paragraph a METHOD-ID procedure division opens for the
/// sentences before its first paragraph. The angle brackets are not in any COBOL word, so no
/// paragraph name can clash with it; resolve and scan recognise the paragraph by this name.
pub const METHOD_BODY_PARAGRAPH: &str = "<method-body>";

/// Parses the PROCEDURE DIVISION at the start of `tokens`. `map` locates the tokens and the
/// diagnostics. Returns `None` when `tokens` does not start with the division header. A
/// diagnostic never stops the parse: a malformed sentence keeps the statements before its
/// error, and the next sentence is read from its period.
pub fn parse_procedure_division(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
) -> Option<(ProcedureDivision, Vec<Diagnostic>)> {
    parse_division(map, dialect, tokens, false)
}

/// Parses a METHOD-ID's procedure division like [`parse_procedure_division`], except that the
/// sentences before its first paragraph are kept in a leading [`METHOD_BODY_PARAGRAPH`] rather
/// than diagnosed.
pub fn parse_method_procedure_division(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
) -> Option<(ProcedureDivision, Vec<Diagnostic>)> {
    parse_division(map, dialect, tokens, true)
}

fn parse_division(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
    method_body: bool,
) -> Option<(ProcedureDivision, Vec<Diagnostic>)> {
    if !(is(tokens.first()?, "PROCEDURE") && is(tokens.get(1)?, "DIVISION")) {
        return None;
    }
    let mut p = Parser {
        map,
        dialect,
        tokens,
        method_body,
        diags: Vec::new(),
        declaratives: Vec::new(),
        paragraphs: Vec::new(),
        sections: Vec::new(),
        section: None,
        para: None,
        in_decl: false,
        decl_done: false,
        started: false,
        last: tokens[0].origin,
    };
    let body = tokens
        .iter()
        .position(|t| t.token.kind == TokenKind::Period)
        .map_or(tokens.len(), |at| at + 1);
    p.body(body);
    p.flush_para();
    p.flush_section();
    if p.in_decl {
        let at = p.origin(0);
        p.diag(at, "DECLARATIVES is not ended by END DECLARATIVES");
    }
    let division = ProcedureDivision {
        span: Span {
            start: tokens[0].origin,
            end: p.last,
        },
        declaratives: p.declaratives,
        paragraphs: p.paragraphs,
        sections: p.sections,
    };
    Some((division, p.diags))
}

/// Which statement shape a verb takes beyond its operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Alter,
    Call,
    SortMerge,
    Resume,
}

pub(super) struct Parser<'a> {
    map: &'a SourceMap,
    pub(super) dialect: Dialect,
    pub(super) tokens: &'a [Mapped],
    /// A METHOD-ID body: statements before its first paragraph open the leading paragraph.
    method_body: bool,
    diags: Vec<Diagnostic>,
    declaratives: Vec<Section>,
    paragraphs: Vec<Paragraph>,
    sections: Vec<Section>,
    section: Option<Section>,
    para: Option<Paragraph>,
    in_decl: bool,
    decl_done: bool,
    started: bool,
    last: Origin,
}

impl Parser<'_> {
    pub(super) fn origin(&self, i: usize) -> Origin {
        self.tokens[i].origin
    }

    pub(super) fn diag(&mut self, at: Origin, message: &str) {
        let diag = self.map.diagnostic(at, message);
        self.diags.push(diag);
    }

    pub(super) fn name(&self, i: usize) -> Name {
        let origin = self.origin(i);
        Name {
            text: self.tokens[i].token.text.clone(),
            span: Span {
                start: origin,
                end: origin,
            },
        }
    }

    /// The span of tokens `a..b` (`b` exclusive, `b > a`).
    pub(super) fn span(&self, a: usize, b: usize) -> Span {
        Span {
            start: self.origin(a),
            end: self.origin(b - 1),
        }
    }

    /// Every sentence from `start` to the end of the tokens. A program's end or the next
    /// program's identification division stops the body.
    fn body(&mut self, start: usize) {
        let t = self.tokens;
        let mut i = start;
        while i < t.len() {
            let stop = self.period_from(i);
            if self.ends_program(i, stop) {
                break;
            }
            // A header inside the sentence means its period is missing: the sentence ends there.
            // An EXEC block is skipped whole, so a word inside it is never a header.
            let mut header = None;
            let mut j = i + 1;
            while j < stop {
                if is_exec_block(t, j) {
                    j = self.past_block(j);
                    continue;
                }
                if self.header_at(j, stop) {
                    header = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(header) = header {
                self.sentence(i, header, None);
                i = header;
                continue;
            }
            if stop > i {
                let period = (stop < t.len()).then(|| self.origin(stop));
                self.sentence(i, stop, period);
            }
            i = stop + 1;
        }
    }

    /// The index of the first period at or after `from` outside an EXEC block (the period of the
    /// sentence), or the token count when there is none.
    fn period_from(&self, from: usize) -> usize {
        let t = self.tokens;
        let mut j = from;
        while j < t.len() && t[j].token.kind != TokenKind::Period {
            j = self.past_block(j);
        }
        j.min(t.len())
    }

    /// The index after the token at `j`: past its `END-EXEC` when it opens an EXEC block that has
    /// one, else `j + 1`.
    fn past_block(&self, j: usize) -> usize {
        match is_exec_block(self.tokens, j)
            .then(|| exec_block_end(self.tokens, j, self.tokens.len()))
            .flatten()
        {
            Some(after) => after,
            None => j + 1,
        }
    }

    /// Whether token `j` starts a header that ends the sentence before `stop` (which holds a
    /// period): a paragraph (name then the period) or `<name> SECTION [n].`. Fixed layout needs
    /// the name in area A; free layout takes only the section form. A token at the position of
    /// the previous token (a REPLACE substitution) never starts one.
    fn header_at(&self, j: usize, stop: usize) -> bool {
        let t = self.tokens;
        let o = t[j].origin;
        if stop >= t.len() || !self.is_proc_name(j) || t[j - 1].origin == o {
            return false;
        }
        let Some(layout) = self.map.layout(o.file) else {
            return false;
        };
        let area_a = matches!(layout, Layout::Source(_)) && AREA_A.contains(&o.col);
        if j + 1 == stop {
            return area_a;
        }
        let section = j + 1 < stop
            && is(&t[j + 1], "SECTION")
            && (j + 2 == stop || (j + 3 == stop && t[j + 2].token.kind == TokenKind::Numeric));
        section && (area_a || layout == Layout::Free)
    }

    fn ends_program(&self, i: usize, stop: usize) -> bool {
        let t = self.tokens;
        let second = |word: &str| i + 1 < stop && is(&t[i + 1], word);
        (is(&t[i], "END") && second("PROGRAM"))
            || ((is(&t[i], "IDENTIFICATION") || is(&t[i], "ID")) && second("DIVISION"))
    }

    /// One sentence `a..b` with its period: a declarative or section or paragraph header, or
    /// a run of statements.
    fn sentence(&mut self, a: usize, b: usize, period: Option<Origin>) {
        let t = self.tokens;
        let words = &t[a..b];
        self.last = period.unwrap_or(self.origin(b - 1));
        if period.is_none() {
            let at = self.origin(a);
            self.diag(at, "sentence is not ended by a period");
        }
        if spells(words, &["DECLARATIVES"]) {
            if self.started || self.in_decl || self.decl_done {
                let at = self.origin(a);
                self.diag(at, "DECLARATIVES must open the PROCEDURE DIVISION");
            } else {
                self.in_decl = true;
            }
        } else if spells(words, &["END", "DECLARATIVES"]) {
            if self.in_decl {
                self.flush_para();
                self.flush_section();
                self.in_decl = false;
                self.decl_done = true;
            } else {
                let at = self.origin(a);
                self.diag(at, "END DECLARATIVES without DECLARATIVES");
            }
        } else if section_header(words) {
            if words.len() == 3
                && !self.feature(a + 2, Feature::SectionSegments, "SECTION segments")
            {
                return;
            }
            self.flush_para();
            self.flush_section();
            self.started = true;
            self.section = Some(Section {
                name: self.name(a),
                span: self.span(a, a + 1),
                paragraphs: Vec::new(),
            });
        } else if words.len() == 1 && is_header_word(&words[0]) && !self.starts_statement(a) {
            if self.in_decl && self.section.is_none() {
                let at = self.origin(a);
                self.diag(at, "expected a section header in DECLARATIVES");
                return;
            }
            self.flush_para();
            self.started = true;
            self.para = Some(Paragraph {
                name: self.name(a),
                span: self.span(a, a + 1),
                sentences: Vec::new(),
            });
        } else {
            self.statement_sentence(a, b, period);
        }
    }

    /// A sentence of statements, added to the open paragraph.
    fn statement_sentence(&mut self, a: usize, b: usize, period: Option<Origin>) {
        // A declarative section's USE sentence precedes its first paragraph; the AST has no slot
        // for it, so it is checked for diagnostics and not kept.
        let use_sentence = self.in_decl && self.section.is_some() && is(&self.tokens[a], "USE");
        if self.para.is_none() && !use_sentence {
            if self.method_body && self.section.is_none() && !self.in_decl {
                let at = self.origin(a);
                let name = Name {
                    text: METHOD_BODY_PARAGRAPH.to_string(),
                    span: Span { start: at, end: at },
                };
                self.para = Some(Paragraph {
                    name,
                    span: self.span(a, a + 1),
                    sentences: Vec::new(),
                });
            } else {
                let at = self.origin(a);
                self.diag(at, "statement outside a paragraph");
                return;
            }
        }
        let (statements, next) = self.statements(a, b);
        if next < b {
            let at = self.origin(next);
            let found = self.tokens[next].token.text.clone();
            self.diag(at, &format!("expected a statement, found `{found}`"));
        }
        if self.para.is_none() {
            return;
        }
        let sentence = Sentence {
            span: self.span(a, b),
            statements,
            period,
        };
        if let Some(para) = self.para.as_mut() {
            para.sentences.push(sentence);
        }
    }

    fn flush_para(&mut self) {
        if let Some(mut para) = self.para.take() {
            para.span.end = self.last;
            match self.section.as_mut() {
                Some(section) => section.paragraphs.push(para),
                None => self.paragraphs.push(para),
            }
        }
    }

    fn flush_section(&mut self) {
        if let Some(mut section) = self.section.take() {
            section.span.end = self.last;
            if self.in_decl {
                self.declaratives.push(section);
            } else {
                self.sections.push(section);
            }
        }
    }

    /// Reports a feature whose level gives a diagnostic. `false` when the level skips the
    /// construct.
    fn feature(&mut self, i: usize, feature: Feature, construct: &str) -> bool {
        let level = self.dialect.feature_level(feature);
        if level.emits_diagnostic() {
            let at = self.origin(i);
            self.diag(at, &format!("{construct} is {level:?} under this dialect"));
        }
        !level.skips_construct()
    }

    fn is_verb(&self, k: usize) -> bool {
        word_in(&self.tokens[k], VERBS)
    }

    /// Whether token `k` starts a statement. `INVOKE` always does (as it did when it was a verb);
    /// `RAISE` only under OO recognition; an EXEC block always does. Elsewhere they are data names.
    fn starts_statement(&self, k: usize) -> bool {
        let t = &self.tokens[k];
        self.is_verb(k)
            || is(t, "INVOKE")
            || (self.dialect.oo_enabled() && is(t, "RAISE"))
            || is_exec_block(self.tokens, k)
    }

    /// Whether an operand or condition scan stops at `k`: a verb, an OO statement or an EXEC block.
    fn stmt_boundary(&self, k: usize, end: usize) -> bool {
        is_exec_block(self.tokens, k) || self.oo_statement_at(k, end)
    }

    /// Whether an OO `INVOKE` or `RAISE` at `k` has a statement's shape, so an operand or
    /// condition scan stops before it. Only under OO recognition; a data name fails the shape
    /// check and stays an operand (`MOVE INVOKE TO X`).
    fn oo_statement_at(&self, k: usize, end: usize) -> bool {
        if !self.dialect.oo_enabled() || k + 1 >= end {
            return false;
        }
        let t = self.tokens;
        if is(&t[k], "INVOKE") {
            let method = if word_in(&t[k + 1], &["SELF", "SUPER", "NULL"]) {
                k + 2
            } else if self.is_name(k + 1) {
                self.qualified(k + 1, end).1
            } else {
                return false;
            };
            return method < end
                && (matches!(t[method].token.kind, TokenKind::Alnum | TokenKind::National)
                    || self.is_name(method));
        }
        is(&t[k], "RAISE") && (is(&t[k + 1], "EXCEPTION") || self.is_name(k + 1))
    }

    /// A data or procedure name: a word that is neither a keyword nor a verb.
    pub(super) fn is_name(&self, k: usize) -> bool {
        let t = &self.tokens[k];
        t.token.kind == TokenKind::Word && !word_in(t, KEYWORDS) && !word_in(t, VERBS)
    }

    /// A procedure name: a data-name-like word or an all-digit paragraph name (`GO TO 50`).
    fn is_proc_name(&self, k: usize) -> bool {
        self.is_name(k) || self.tokens[k].token.kind == TokenKind::Numeric
    }

    /// The name at `k` with its `OF`/`IN` qualifiers, and the index after it.
    pub(super) fn qualified(&self, k: usize, end: usize) -> (QualifiedName, usize) {
        let t = self.tokens;
        let mut next = k + 1;
        let mut qualifiers = Vec::new();
        while next + 1 < end
            && (is(&t[next], "OF") || is(&t[next], "IN"))
            && t[next + 1].token.kind == TokenKind::Word
        {
            qualifiers.push(self.name(next + 1));
            next += 2;
        }
        (
            QualifiedName {
                name: self.name(k),
                qualifiers,
            },
            next,
        )
    }

    /// Adds the name at `k` to `refs` when it is one; the index after the token consumed. The
    /// name after `FUNCTION` is an intrinsic function, so it is skipped; its arguments still count.
    fn ref_at(&self, k: usize, end: usize, refs: &mut Vec<QualifiedName>) -> usize {
        let t = self.tokens;
        if is(&t[k], "FUNCTION") && k + 1 < end && t[k + 1].token.kind == TokenKind::Word {
            return k + 2;
        }
        if self.is_name(k) {
            let (name, next) = self.qualified(k, end);
            refs.push(name);
            next
        } else {
            k + 1
        }
    }

    /// Collects the references in operands up to a verb or a statement-closing token.
    fn refs_until(&self, mut k: usize, end: usize, refs: &mut Vec<QualifiedName>) -> usize {
        while k < end && !self.is_verb(k) && !self.stmt_boundary(k, end) && !self.closes(k, end) {
            k = self.ref_at(k, end, refs);
        }
        k
    }

    /// The number of tokens of the branch phrase at `k` (a leading `NOT` included), or 0.
    /// `WHEN` opens a branch only in `SEARCH`.
    fn branch_at(&self, k: usize, end: usize, search: bool) -> usize {
        let t = self.tokens;
        let not = usize::from(k < end && is(&t[k], "NOT"));
        let at = k + not;
        if search && at < end && is(&t[at], "WHEN") {
            return not + 1;
        }
        for phrase in BRANCHES {
            let n = phrase.len();
            if at + n <= end && phrase.iter().enumerate().all(|(j, w)| is(&t[at + j], w)) {
                return not + n;
            }
        }
        0
    }

    /// Whether token `k` closes the list it sits in: `ELSE`, `WHEN`, a scope terminator or a
    /// branch phrase.
    fn closes(&self, k: usize, end: usize) -> bool {
        let t = &self.tokens[k];
        word_in(t, &["ELSE", "WHEN"]) || is_end_word(t) || self.branch_at(k, end, false) > 0
    }

    /// The statements in `i..end`. Stops at a token that closes the list or starts no statement;
    /// the caller decides what that token means.
    fn statements(&mut self, mut i: usize, end: usize) -> (Vec<Statement>, usize) {
        let mut out = Vec::new();
        while i < end && self.starts_statement(i) {
            let (statement, next) = self.statement(i, end);
            out.extend(statement);
            i = next;
        }
        (out, i)
    }

    /// The statement starting at verb `i`, and the index after it. `None` when the statement is
    /// skipped: its diagnostic is already reported and the rest of the sentence is dropped.
    fn statement(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        if is_exec_block(self.tokens, i) {
            return self.exec_block(i, end);
        }
        let verb = self.tokens[i].token.text.to_ascii_uppercase();
        match verb.as_str() {
            "IF" => self.if_statement(i, end),
            "PERFORM" => self.perform(i, end),
            "EVALUATE" => self.evaluate(i, end),
            "GO" => self.go(i, end),
            "ALTER" => {
                if !self.feature(i, Feature::Alter, "ALTER") {
                    return (None, end);
                }
                self.generic(i, end, Kind::Alter)
            }
            "ENTRY" => {
                if !self.feature(i, Feature::Entry, "ENTRY") {
                    return (None, end);
                }
                self.generic(i, end, Kind::Plain)
            }
            "CALL" => self.generic(i, end, Kind::Call),
            "SORT" | "MERGE" => self.generic(i, end, Kind::SortMerge),
            "RESUME" => self.generic(i, end, Kind::Resume),
            "INVOKE" if self.dialect.oo_enabled() => self.invoke(i, end),
            "RAISE" if self.dialect.oo_enabled() => self.raise(i, end),
            _ => self.generic(i, end, Kind::Plain),
        }
    }

    /// `IF` with its condition, its `ELSE` branch and an optional `END-IF`.
    fn if_statement(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let mut refs = Vec::new();
        let mut k = self.refs_until(i + 1, end, &mut refs);
        let (mut children, mut next) = self.statements(k, end);
        if next < end && is(&t[next], "ELSE") {
            let (more, after) = self.statements(next + 1, end);
            children.extend(more);
            next = after;
        }
        k = next;
        let mut scope_end = None;
        if k < end && is(&t[k], "END-IF") {
            scope_end = Some(self.span(k, k + 1));
            k += 1;
        }
        let statement = Statement {
            span: self.span(i, k),
            scope_end,
            transfer: None,
            refs,
            children,
        };
        (Some(statement), k)
    }

    /// `PERFORM`: out-of-line with an optional `THRU` range, or inline up to `END-PERFORM`.
    fn perform(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let mut k = i + 1;
        if k + 1 < end && is(&t[k], "WITH") && is(&t[k + 1], "TEST") {
            k += 2;
            if k < end && (is(&t[k], "BEFORE") || is(&t[k], "AFTER")) {
                k += 1;
            }
        }
        if k >= end {
            let at = self.origin(i);
            self.diag(at, "PERFORM without a target or statements");
            return (None, end);
        }
        let outline = self.is_proc_name(k)
            && !self.closes(k, end)
            && !(k + 1 < end && is(&t[k + 1], "TIMES"));
        if outline {
            if let Some((range, next)) = self.proc_range(k, end) {
                let mut refs = Vec::new();
                let next = self.refs_until(next, end, &mut refs);
                let statement = Statement {
                    span: self.span(i, next),
                    scope_end: None,
                    transfer: Some(Transfer::Perform(Some(range))),
                    refs,
                    children: Vec::new(),
                };
                return (Some(statement), next);
            }
        }
        let mut refs = Vec::new();
        let mut h = k;
        while h < end
            && !self.is_verb(h)
            && !self.stmt_boundary(h, end)
            && !self.closes(h, end)
            && !is(&t[h], "TIMES")
        {
            h = self.ref_at(h, end, &mut refs);
        }
        if h < end && is(&t[h], "TIMES") {
            h += 1;
        }
        let (children, mut next) = self.statements(h, end);
        let mut scope_end = None;
        if next < end && is(&t[next], "END-PERFORM") {
            scope_end = Some(self.span(next, next + 1));
            next += 1;
        }
        let statement = Statement {
            span: self.span(i, next),
            scope_end,
            transfer: Some(Transfer::Perform(None)),
            refs,
            children,
        };
        (Some(statement), next)
    }

    /// `EVALUATE` subjects, then each `WHEN` condition and its statements to `END-EVALUATE`.
    fn evaluate(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let mut refs = Vec::new();
        let mut children = Vec::new();
        let mut k = self.refs_until(i + 1, end, &mut refs);
        while k < end && is(&t[k], "WHEN") {
            k = self.refs_until(k + 1, end, &mut refs);
            let (kids, next) = self.statements(k, end);
            children.extend(kids);
            k = next;
        }
        let mut scope_end = None;
        if k < end && is(&t[k], "END-EVALUATE") {
            scope_end = Some(self.span(k, k + 1));
            k += 1;
        }
        let statement = Statement {
            span: self.span(i, k),
            scope_end,
            transfer: None,
            refs,
            children,
        };
        (Some(statement), k)
    }

    /// `GO [TO] name... [DEPENDING ON item]`. A `GO` without a name is obsolete but still scanned.
    fn go(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let mut k = i + 1;
        if k < end && is(&t[k], "TO") {
            k += 1;
        }
        let mut targets = Vec::new();
        while k < end && self.is_proc_name(k) && !self.closes(k, end) && !is(&t[k], "DEPENDING") {
            let (name, next) = self.qualified(k, end);
            targets.push(name);
            k = next;
        }
        if targets.is_empty()
            && !self.feature(
                i,
                Feature::GoToWithoutName,
                "GO TO without a procedure name",
            )
        {
            return (None, end);
        }
        let mut refs = Vec::new();
        if k < end && is(&t[k], "DEPENDING") {
            k = self.refs_until(k + 1, end, &mut refs);
        }
        let statement = Statement {
            span: self.span(i, k),
            scope_end: None,
            transfer: Some(Transfer::GoTo(targets)),
            refs,
            children: Vec::new(),
        };
        (Some(statement), k)
    }

    /// `INVOKE receiver method [USING ...] [RETURNING name]` (OO COBOL). `SELF`, `SUPER` and
    /// `NULL` are receivers of their own; a name is a class or an object, which resolve decides.
    /// The method is a literal or an identifier, as a `CALL` target is; the receiver is not a
    /// data reference. USING and RETURNING names are references.
    fn invoke(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let mut k = i + 1;
        let receiver = if k < end && is(&t[k], "SELF") {
            k += 1;
            Receiver::Self_
        } else if k < end && is(&t[k], "SUPER") {
            k += 1;
            Receiver::Super
        } else if k < end && is(&t[k], "NULL") {
            k += 1;
            Receiver::Null
        } else if k < end && self.is_name(k) {
            let (name, next) = self.qualified(k, end);
            k = next;
            Receiver::Name(name)
        } else {
            let at = self.origin(i);
            self.diag(at, "INVOKE without a receiver");
            return (None, end);
        };
        let method = if k < end && matches!(t[k].token.kind, TokenKind::Alnum | TokenKind::National)
        {
            k += 1;
            CallTarget::Literal(self.name(k - 1))
        } else if k < end && self.is_name(k) {
            let (name, next) = self.qualified(k, end);
            k = next;
            CallTarget::Identifier(name)
        } else {
            let at = self.origin(i);
            self.diag(at, "INVOKE without a method name");
            return (None, end);
        };
        let mut using = Vec::new();
        let mut returning = None;
        let mut refs = Vec::new();
        let mut after_returning = false;
        while k < end && !self.is_verb(k) && !self.stmt_boundary(k, end) && !self.closes(k, end) {
            if is(&t[k], "RETURNING") {
                after_returning = true;
                k += 1;
            } else if after_returning && self.is_name(k) {
                let (name, next) = self.qualified(k, end);
                refs.push(name.clone());
                returning = Some(name);
                k = next;
            } else {
                let before = refs.len();
                k = self.ref_at(k, end, &mut refs);
                using.extend(refs[before..].iter().cloned());
            }
        }
        let statement = Statement {
            span: self.span(i, k),
            scope_end: None,
            transfer: Some(Transfer::Invoke {
                receiver,
                method,
                using,
                returning,
            }),
            refs,
            children: Vec::new(),
        };
        (Some(statement), k)
    }

    /// `RAISE EXCEPTION condition-name` or `RAISE object` (Micro Focus). Neither operand is a
    /// data reference: the statement carries only its target.
    fn raise(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let mut k = i + 1;
        let target = if k < end && is(&t[k], "EXCEPTION") {
            k += 1;
            if k < end && self.is_name(k) {
                k += 1;
                RaiseTarget::ExceptionName(self.name(k - 1))
            } else {
                let at = self.origin(i);
                self.diag(at, "RAISE EXCEPTION without a condition name");
                return (None, end);
            }
        } else if k < end && self.is_name(k) {
            let (name, next) = self.qualified(k, end);
            k = next;
            RaiseTarget::Object(name)
        } else {
            let at = self.origin(i);
            self.diag(at, "RAISE without an exception name or object");
            return (None, end);
        };
        let statement = Statement {
            span: self.span(i, k),
            scope_end: None,
            transfer: Some(Transfer::Raise(target)),
            refs: Vec::new(),
            children: Vec::new(),
        };
        (Some(statement), k)
    }

    /// `name [IS]? from [THRU|THROUGH thru]` at `k`, and the index after it.
    fn proc_range(&self, mut k: usize, end: usize) -> Option<(ProcRange, usize)> {
        let t = self.tokens;
        if k < end && is(&t[k], "IS") {
            k += 1;
        }
        if k >= end || !self.is_proc_name(k) {
            return None;
        }
        let (from, mut next) = self.qualified(k, end);
        let mut thru = None;
        if next + 1 < end
            && (is(&t[next], "THRU") || is(&t[next], "THROUGH"))
            && self.is_proc_name(next + 1)
        {
            let (name, after) = self.qualified(next + 1, end);
            thru = Some(name);
            next = after;
        }
        Some((ProcRange { from, thru }, next))
    }

    /// Any other statement: its operands are references, its branch phrases hold nested
    /// statements, and an own scope terminator (`END-<verb>`) is taken when written. `kind`
    /// adds the transfer that the verb's operands carry.
    fn generic(&mut self, i: usize, end: usize, kind: Kind) -> (Option<Statement>, usize) {
        let t = self.tokens;
        let verb = t[i].token.text.to_ascii_uppercase();
        let search = verb == "SEARCH";
        let mut k = i + 1;
        let mut refs = Vec::new();
        let mut children = Vec::new();
        let mut call = None;
        let mut resume = None;
        let mut sort: (Option<ProcRange>, Option<ProcRange>) = (None, None);
        if kind == Kind::Call {
            if k < end && matches!(t[k].token.kind, TokenKind::Alnum | TokenKind::National) {
                call = Some(CallTarget::Literal(self.name(k)));
                k += 1;
            } else if k < end && self.is_name(k) {
                let (name, next) = self.qualified(k, end);
                call = Some(CallTarget::Identifier(name));
                k = next;
            } else {
                let at = self.origin(i);
                self.diag(at, "CALL without a program name");
                return (None, end);
            }
        }
        while k < end {
            if kind == Kind::SortMerge
                && (is(&t[k], "INPUT") || is(&t[k], "OUTPUT"))
                && k + 1 < end
                && is(&t[k + 1], "PROCEDURE")
            {
                let Some((range, next)) = self.proc_range(k + 2, end) else {
                    let at = self.origin(k);
                    self.diag(at, "expected a procedure name after PROCEDURE");
                    return (None, end);
                };
                if is(&t[k], "INPUT") {
                    sort.0 = Some(range);
                } else {
                    sort.1 = Some(range);
                }
                k = next;
                continue;
            }
            if kind == Kind::Resume
                && k == i + 1
                && is(&t[k], "AT")
                && k + 1 < end
                && self.is_name(k + 1)
            {
                let (name, next) = self.qualified(k + 1, end);
                resume = Some(name);
                k = next;
                continue;
            }
            let branch = self.branch_at(k, end, search);
            if branch > 0 {
                // A SEARCH `WHEN` holds a condition before its statements.
                let mut body = k + branch;
                if search && is(&t[body - 1], "WHEN") {
                    body = self.refs_until(body, end, &mut refs);
                }
                let (kids, next) = self.statements(body, end);
                children.extend(kids);
                k = next;
                continue;
            }
            if self.closes(k, end) || self.is_verb(k) || self.stmt_boundary(k, end) {
                break;
            }
            // ALTER operands are procedure names, which are not data references: dropped here.
            k = if kind == Kind::Alter {
                k + 1
            } else {
                self.ref_at(k, end, &mut refs)
            };
        }
        let mut scope_end = None;
        let end_word = format!("END-{verb}");
        if k < end && is(&t[k], &end_word) {
            scope_end = Some(self.span(k, k + 1));
            k += 1;
        }
        let transfer = match kind {
            Kind::Plain | Kind::Alter => None,
            Kind::Call => call.map(Transfer::Call),
            Kind::SortMerge => Some(Transfer::SortMerge {
                input: sort.0,
                output: sort.1,
            }),
            Kind::Resume => resume.map(Transfer::Resume),
        };
        let statement = Statement {
            span: self.span(i, k),
            scope_end,
            transfer,
            refs,
            children,
        };
        (Some(statement), k)
    }
}

/// Whether `t` is the word `word` (case-insensitive).
pub(super) fn is(t: &Mapped, word: &str) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
        && t.token.text.eq_ignore_ascii_case(word)
}

pub(super) fn word_in(t: &Mapped, list: &[&str]) -> bool {
    list.iter().any(|word| is(t, word))
}

/// Whether `t` is a COBOL-85 scope terminator. A user word such as `END-ROUTINE` is not one.
fn is_end_word(t: &Mapped) -> bool {
    word_in(t, SCOPE_TERMINATORS)
}

/// Whether `words` are exactly the words in `list`.
fn spells(words: &[Mapped], list: &[&str]) -> bool {
    words.len() == list.len() && words.iter().zip(list).all(|(t, &w)| is(t, w))
}

/// Whether `t` can name a section or paragraph header: a word or an all-digit name.
fn is_header_word(t: &Mapped) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Numeric)
}

/// Whether `words` is a `<name> SECTION.` header, with an optional segment number.
fn section_header(words: &[Mapped]) -> bool {
    words.len() >= 2
        && is_header_word(&words[0])
        && is(&words[1], "SECTION")
        && (words.len() == 2 || (words.len() == 3 && words[2].token.kind == TokenKind::Numeric))
}
