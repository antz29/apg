//! PROCEDURE DIVISION parser. Reads the division's tokens into `ast` declaratives, sections,
//! paragraphs, sentences and statements. A sentence ends at its period; a statement is parsed
//! by its verb to its scope terminator, with nested statements as children. A malformed
//! sentence keeps the statements parsed before its error. Entry point:
//! [`parse_procedure_division`], called by the parser with the tokens from `PROCEDURE DIVISION`
//! to the end of the program's procedure text.

use crate::ast::{
    CallTarget, Name, Paragraph, ProcRange, ProcedureDivision, QualifiedName, Section, Sentence,
    Span, Statement, Transfer,
};
use crate::diag::Diagnostic;
use crate::dialect::{Dialect, Feature};
use crate::lexer::TokenKind;
use crate::source_map::{Mapped, Origin, SourceMap};

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
    "INVOKE",
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

/// Phrases that open a branch of a statement (`ON SIZE ERROR`, `AT END`, ...), each with the
/// statements it holds. A leading `NOT` negates the branch.
const BRANCHES: &[&[&str]] = &[
    &["ON", "SIZE", "ERROR"],
    &["ON", "OVERFLOW"],
    &["ON", "EXCEPTION"],
    &["AT", "END"],
    &["AT", "EOP"],
    &["INVALID", "KEY"],
];

/// Words that are phrase keywords or operand noise, never data names in a reference.
const KEYWORDS: &[&str] = &[
    "AFTER",
    "ALL",
    "ALSO",
    "AND",
    "AT",
    "ASCENDING",
    "BEFORE",
    "BY",
    "CONTENT",
    "CORRESPONDING",
    "DEPENDING",
    "DESCENDING",
    "ELSE",
    "EOP",
    "EQUAL",
    "ERROR",
    "EXCEPTION",
    "FALSE",
    "FROM",
    "GIVING",
    "GREATER",
    "HIGH-VALUE",
    "HIGH-VALUES",
    "IN",
    "INPUT",
    "INTO",
    "INVALID",
    "IS",
    "KEY",
    "LESS",
    "LINE",
    "LINES",
    "LOW-VALUE",
    "LOW-VALUES",
    "NOT",
    "OF",
    "ON",
    "OR",
    "OTHER",
    "OUTPUT",
    "OVERFLOW",
    "PAGE",
    "PROCEED",
    "PROCEDURE",
    "QUOTE",
    "QUOTES",
    "RECORD",
    "REFERENCE",
    "RETURNING",
    "ROUNDED",
    "SECTION",
    "SENTENCE",
    "SIZE",
    "SPACE",
    "SPACES",
    "STANDARD",
    "TEST",
    "THAN",
    "THEN",
    "THROUGH",
    "THRU",
    "TIMES",
    "TO",
    "TRUE",
    "UNTIL",
    "UPON",
    "USING",
    "VALUE",
    "VARYING",
    "WITH",
    "ZERO",
    "ZEROS",
    "ZEROES",
];

/// Parses the PROCEDURE DIVISION at the start of `tokens`. `map` locates the tokens and the
/// diagnostics. Returns `None` when `tokens` does not start with the division header. A
/// diagnostic never stops the parse: a malformed sentence keeps the statements before its
/// error, and the next sentence is read from its period.
pub fn parse_procedure_division(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
) -> Option<(ProcedureDivision, Vec<Diagnostic>)> {
    if !(is(tokens.first()?, "PROCEDURE") && is(tokens.get(1)?, "DIVISION")) {
        return None;
    }
    let mut p = Parser {
        map,
        dialect,
        tokens,
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
    Call,
    SortMerge,
    Resume,
}

struct Parser<'a> {
    map: &'a SourceMap,
    dialect: Dialect,
    tokens: &'a [Mapped],
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

    /// The span of tokens `a..b` (`b` exclusive, `b > a`).
    fn span(&self, a: usize, b: usize) -> Span {
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
            let stop = (i..t.len())
                .find(|&j| t[j].token.kind == TokenKind::Period)
                .unwrap_or(t.len());
            if self.ends_program(i, stop) {
                break;
            }
            if stop > i {
                let period = (stop < t.len()).then(|| self.origin(stop));
                self.sentence(i, stop, period);
            }
            i = stop + 1;
        }
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
        } else if words.len() == 1 && words[0].token.kind == TokenKind::Word && !self.is_verb(a) {
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
        if self.para.is_none() {
            let at = self.origin(a);
            self.diag(at, "statement outside a paragraph");
            return;
        }
        let (statements, next) = self.statements(a, b);
        if next < b {
            let at = self.origin(next);
            let found = self.tokens[next].token.text.clone();
            self.diag(at, &format!("expected a statement, found `{found}`"));
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

    /// A data or procedure name: a word that is neither a keyword nor a verb.
    fn is_name(&self, k: usize) -> bool {
        let t = &self.tokens[k];
        t.token.kind == TokenKind::Word && !word_in(t, KEYWORDS) && !word_in(t, VERBS)
    }

    /// The name at `k` with its `OF`/`IN` qualifiers, and the index after it.
    fn qualified(&self, k: usize, end: usize) -> (QualifiedName, usize) {
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

    /// Adds the name at `k` to `refs` when it is one; the index after the token consumed.
    fn ref_at(&self, k: usize, end: usize, refs: &mut Vec<QualifiedName>) -> usize {
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
        while k < end && !self.is_verb(k) && !self.closes(k, end) {
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
        while i < end && self.is_verb(i) {
            let (statement, next) = self.statement(i, end);
            out.extend(statement);
            i = next;
        }
        (out, i)
    }

    /// The statement starting at verb `i`, and the index after it. `None` when the statement is
    /// skipped: its diagnostic is already reported and the rest of the sentence is dropped.
    fn statement(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
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
                self.generic(i, end, Kind::Plain)
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
        let outline =
            self.is_name(k) && !self.closes(k, end) && !(k + 1 < end && is(&t[k + 1], "TIMES"));
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
        while h < end && !self.is_verb(h) && !self.closes(h, end) && !is(&t[h], "TIMES") {
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
        while k < end && self.is_name(k) && !self.closes(k, end) && !is(&t[k], "DEPENDING") {
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

    /// `name [IS]? from [THRU|THROUGH thru]` at `k`, and the index after it.
    fn proc_range(&self, mut k: usize, end: usize) -> Option<(ProcRange, usize)> {
        let t = self.tokens;
        if k < end && is(&t[k], "IS") {
            k += 1;
        }
        if k >= end || !self.is_name(k) {
            return None;
        }
        let (from, mut next) = self.qualified(k, end);
        let mut thru = None;
        if next + 1 < end
            && (is(&t[next], "THRU") || is(&t[next], "THROUGH"))
            && self.is_name(next + 1)
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
                let (kids, next) = self.statements(k + branch, end);
                children.extend(kids);
                k = next;
                continue;
            }
            if self.closes(k, end) || self.is_verb(k) {
                break;
            }
            k = self.ref_at(k, end, &mut refs);
        }
        let mut scope_end = None;
        let end_word = format!("END-{verb}");
        if k < end && is(&t[k], &end_word) {
            scope_end = Some(self.span(k, k + 1));
            k += 1;
        }
        let transfer = match kind {
            Kind::Plain => None,
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
fn is(t: &Mapped, word: &str) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
        && t.token.text.eq_ignore_ascii_case(word)
}

fn word_in(t: &Mapped, list: &[&str]) -> bool {
    list.iter().any(|word| is(t, word))
}

/// Whether `t` is a scope terminator (`END-IF`, `END-PERFORM`, ...).
fn is_end_word(t: &Mapped) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
        && t.token
            .text
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("END-"))
}

/// Whether `words` are exactly the words in `list`.
fn spells(words: &[Mapped], list: &[&str]) -> bool {
    words.len() == list.len() && words.iter().zip(list).all(|(t, &w)| is(t, w))
}

/// Whether `words` is a `<name> SECTION.` header, with an optional segment number.
fn section_header(words: &[Mapped]) -> bool {
    words.len() >= 2
        && words[0].token.kind == TokenKind::Word
        && is(&words[1], "SECTION")
        && (words.len() == 2 || (words.len() == 3 && words[2].token.kind == TokenKind::Numeric))
}
