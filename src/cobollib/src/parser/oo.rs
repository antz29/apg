//! OO unit parser (COBOL 2002 with the Micro Focus form): `CLASS-ID`, `INTERFACE-ID` and
//! `METHOD-ID` units, with their `FACTORY` and `OBJECT` sections and REPOSITORY entries.
//! Gated on OO recognition: under `cobol85` every entry point returns `None`, so the caller
//! reads the header as before. Entry points, each called with the tokens from its header:
//! - [`parse_class`] at a `CLASS-ID` token, through `END CLASS name.` (or the next unit);
//! - [`parse_interface`] at an `INTERFACE-ID` token, through `END INTERFACE name.`;
//! - [`parse_method`] at a `METHOD-ID` token; `factory` is set by the enclosing section;
//! - [`repository`] over the tokens of an ENVIRONMENT DIVISION, for a class or a program.
//!
//! [`Parsed::used`] counts the tokens the unit spans, so the caller resumes after them.
//! `None` means not recognised; `Parsed { unit: None, .. }` means a malformed header that was
//! skipped with its diagnostic.

use crate::ast::{
    Class, Interface, Method, MethodPrototype, Name, ObjectSection, RepositoryEntry, Span,
};
use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::source_map::{Mapped, SourceMap};

use super::data::parse_data_division;
use super::procedure::parse_procedure_division;

/// One OO unit, or the malformed header it was skipped at.
pub struct Parsed<T> {
    pub unit: Option<T>,
    /// Tokens from the unit's header through its END clause.
    pub used: usize,
    pub diags: Vec<Diagnostic>,
}

/// Which clause a header word or literal belongs to.
#[derive(Clone, Copy)]
enum Mode {
    Name,
    Inherits,
    Implements,
}

/// The header paragraph of a `CLASS-ID`, `INTERFACE-ID` or `METHOD-ID` unit.
struct Header {
    name: Option<Name>,
    external: Option<Name>,
    inherits: Vec<Name>,
    implements: Vec<Name>,
    /// Index of the period that ends the header paragraph (or the slice end).
    period: usize,
}

fn is_word(t: &Mapped, word: &str) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
        && t.token.text.eq_ignore_ascii_case(word)
}

fn word_at(tokens: &[Mapped], k: usize, word: &str) -> bool {
    tokens.get(k).is_some_and(|t| is_word(t, word))
}

fn period_at(tokens: &[Mapped], k: usize) -> bool {
    tokens
        .get(k)
        .is_some_and(|t| t.token.kind == TokenKind::Period)
}

/// `<word> DIVISION` at `k`.
fn division_at(tokens: &[Mapped], k: usize, word: &str) -> bool {
    word_at(tokens, k, word) && word_at(tokens, k + 1, "DIVISION")
}

/// `END <kind>` at `k`.
fn is_end(tokens: &[Mapped], k: usize, kind: &str) -> bool {
    word_at(tokens, k, "END") && word_at(tokens, k + 1, kind)
}

/// `OBJECT.` or `FACTORY.` at `k`: the start of a section.
fn section_header(tokens: &[Mapped], k: usize) -> bool {
    (word_at(tokens, k, "OBJECT") || word_at(tokens, k, "FACTORY")) && period_at(tokens, k + 1)
}

fn is_name(t: &Mapped) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
}

fn name_at(tokens: &[Mapped], k: usize) -> Name {
    let origin = tokens[k].origin;
    Name {
        text: tokens[k].token.text.clone(),
        span: Span {
            start: origin,
            end: origin,
        },
    }
}

/// The span from token `from` through token `to` (clamped to the slice).
fn span(tokens: &[Mapped], from: usize, to: usize) -> Span {
    Span {
        start: tokens[from].origin,
        end: tokens[to.min(tokens.len() - 1)].origin,
    }
}

/// A class-level unit ends at the next unit header or at END PROGRAM.
fn class_boundary(tokens: &[Mapped], k: usize) -> bool {
    division_at(tokens, k, "IDENTIFICATION")
        || word_at(tokens, k, "CLASS-ID")
        || word_at(tokens, k, "INTERFACE-ID")
        || is_end(tokens, k, "PROGRAM")
}

/// A section ends at a class-level boundary, at the next section, or at END CLASS/INTERFACE.
fn section_boundary(tokens: &[Mapped], k: usize) -> bool {
    class_boundary(tokens, k)
        || is_end(tokens, k, "CLASS")
        || is_end(tokens, k, "INTERFACE")
        || section_header(tokens, k)
}

/// A method ends at a section boundary, at the next METHOD-ID, or at its section's END.
fn method_boundary(tokens: &[Mapped], k: usize) -> bool {
    section_boundary(tokens, k)
        || word_at(tokens, k, "METHOD-ID")
        || is_end(tokens, k, "OBJECT")
        || is_end(tokens, k, "FACTORY")
}

/// Ends the unit `kind` whose body starts at `from`. Returns the index where its body ends,
/// the index after its END clause, and whether that END was found. Without an END the body
/// ends at `boundary` (or the slice end).
fn unit_end(
    tokens: &[Mapped],
    from: usize,
    kind: &str,
    boundary: fn(&[Mapped], usize) -> bool,
) -> (usize, usize, bool) {
    let from = from.min(tokens.len());
    for k in from..tokens.len() {
        if is_end(tokens, k, kind) {
            // `END OBJECT.` and `END FACTORY.` carry no name; the others do.
            let named = !matches!(kind, "OBJECT" | "FACTORY");
            let mut used = k + 2;
            if named
                && tokens
                    .get(used)
                    .is_some_and(|t| t.token.kind != TokenKind::Period)
            {
                used += 1;
            }
            if period_at(tokens, used) {
                used += 1;
            }
            return (k, used.min(tokens.len()), true);
        }
        if boundary(tokens, k) {
            return (k, k, false);
        }
    }
    (tokens.len(), tokens.len(), false)
}

/// The header paragraph of the unit at `tokens[0]`: the keyword has its own period, so the
/// paragraph runs from index 1 to the period after the name. Clauses (`AS "external"`,
/// `INHERITS [FROM]`, `IMPLEMENTS`) may follow the name in any order.
fn header(tokens: &[Mapped]) -> Header {
    let period = (2..tokens.len())
        .find(|&k| period_at(tokens, k))
        .unwrap_or(tokens.len());
    let mut h = Header {
        name: None,
        external: None,
        inherits: Vec::new(),
        implements: Vec::new(),
        period,
    };
    let mut mode = Mode::Name;
    let mut k = 1;
    while k < period {
        let t = &tokens[k];
        let upper = t.token.text.to_ascii_uppercase();
        match (is_name(t), upper.as_str()) {
            (true, "AS") => {
                if tokens
                    .get(k + 1)
                    .is_some_and(|l| l.token.kind == TokenKind::Alnum)
                {
                    h.external = Some(name_at(tokens, k + 1));
                    k += 1;
                }
            }
            (true, "INHERITS") => {
                mode = Mode::Inherits;
                if word_at(tokens, k + 1, "FROM") {
                    k += 1;
                }
            }
            (true, "IMPLEMENTS") => mode = Mode::Implements,
            _ if matches!(
                t.token.kind,
                TokenKind::Word | TokenKind::Reserved | TokenKind::Alnum
            ) =>
            {
                match mode {
                    Mode::Name => {
                        if h.name.is_none() {
                            h.name = Some(name_at(tokens, k));
                        }
                    }
                    Mode::Inherits => h.inherits.push(name_at(tokens, k)),
                    Mode::Implements => h.implements.push(name_at(tokens, k)),
                }
            }
            _ => {}
        }
        k += 1;
    }
    h
}

/// The REPOSITORY entries (`CLASS name [AS "external"]`, `INTERFACE name [AS ...]`) among the
/// tokens of an ENVIRONMENT DIVISION. Empty under a dialect without OO recognition.
pub fn repository(dialect: Dialect, tokens: &[Mapped]) -> Vec<RepositoryEntry> {
    let mut out = Vec::new();
    if !dialect.oo_enabled() {
        return out;
    }
    for k in 0..tokens.len() {
        let entry = word_at(tokens, k, "CLASS") || word_at(tokens, k, "INTERFACE");
        if entry && tokens.get(k + 1).is_some_and(is_name) {
            let external = (word_at(tokens, k + 2, "AS")
                && tokens
                    .get(k + 3)
                    .is_some_and(|t| t.token.kind == TokenKind::Alnum))
            .then(|| name_at(tokens, k + 3));
            out.push(RepositoryEntry {
                name: name_at(tokens, k + 1),
                external,
            });
        }
    }
    out
}

/// Parses the `CLASS-ID` unit at `tokens[0]`: its header, REPOSITORY entries, and its
/// `OBJECT.` and `FACTORY.` sections. `None` under a dialect without OO recognition or when
/// `tokens` does not start with `CLASS-ID`.
pub fn parse_class(map: &SourceMap, dialect: Dialect, tokens: &[Mapped]) -> Option<Parsed<Class>> {
    if !dialect.oo_enabled() || !tokens.first().is_some_and(|t| is_word(t, "CLASS-ID")) {
        return None;
    }
    let h = header(tokens);
    let mut diags = Vec::new();
    let (body_end, used, closed) = unit_end(tokens, h.period + 1, "CLASS", class_boundary);
    if !closed {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "CLASS-ID has no END CLASS; the class ends at the next unit",
        ));
    }
    let Some(name) = h.name else {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "CLASS-ID has no class name; the class is skipped",
        ));
        return Some(Parsed {
            unit: None,
            used,
            diags,
        });
    };
    let mut class = Class {
        name,
        span: span(tokens, 0, used - 1),
        identification: span(tokens, 0, h.period),
        external: h.external,
        inherits: h.inherits,
        implements: h.implements,
        repository: Vec::new(),
        factory: None,
        object: None,
    };
    let mut k = (h.period + 1).min(body_end);
    while k < body_end {
        if division_at(tokens, k, "ENVIRONMENT") {
            let e = (k + 1..body_end)
                .find(|&x| {
                    division_at(tokens, x, "DATA")
                        || division_at(tokens, x, "PROCEDURE")
                        || section_header(tokens, x)
                })
                .unwrap_or(body_end);
            class.repository.extend(repository(dialect, &tokens[k..e]));
            k = e;
        } else if section_header(tokens, k) {
            let factory = word_at(tokens, k, "FACTORY");
            let (section, next) = object_section(map, dialect, tokens, k, &mut diags);
            if factory {
                class.factory = Some(section);
            } else {
                class.object = Some(section);
            }
            k = next;
        } else {
            diags.push(map.diagnostic(tokens[k].origin, "text in a CLASS-ID is skipped"));
            k = (k + 1..body_end)
                .find(|&x| division_at(tokens, x, "ENVIRONMENT") || section_header(tokens, x))
                .unwrap_or(body_end);
        }
    }
    Some(Parsed {
        unit: Some(class),
        used,
        diags,
    })
}

/// Parses the `OBJECT.` or `FACTORY.` section at `tokens[k]`: its DATA DIVISION and its
/// METHOD-IDs. Returns the section and the index after its END clause.
fn object_section(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
    k: usize,
    diags: &mut Vec<Diagnostic>,
) -> (ObjectSection, usize) {
    let factory = word_at(tokens, k, "FACTORY");
    let word = if factory { "FACTORY" } else { "OBJECT" };
    let (body_end, used, closed) = unit_end(tokens, k + 2, word, section_boundary);
    if !closed {
        diags.push(map.diagnostic(
            tokens[k].origin,
            &format!("{word} has no END {word}; it ends at the next unit"),
        ));
    }
    let mut section = ObjectSection {
        span: span(tokens, k, used - 1),
        data: None,
        methods: Vec::new(),
    };
    let start = (k + 2).min(body_end);
    let mut j = start;
    if division_at(tokens, start, "DATA") {
        let p = (start..body_end)
            .find(|&x| division_at(tokens, x, "PROCEDURE") || word_at(tokens, x, "METHOD-ID"))
            .unwrap_or(body_end);
        if let Some((data, ds)) = parse_data_division(map, dialect, &tokens[start..p]) {
            section.data = Some(data);
            diags.extend(ds);
        }
        j = p;
    }
    while j < body_end {
        if word_at(tokens, j, "METHOD-ID") {
            let Some(m) = parse_method(map, dialect, &tokens[j..body_end], factory) else {
                j += 1;
                continue;
            };
            diags.extend(m.diags);
            section.methods.extend(m.unit);
            j += m.used.max(1);
        } else if division_at(tokens, j, "PROCEDURE") {
            // The empty PROCEDURE DIVISION that heads the METHOD-IDs of a section.
            j = (j..body_end)
                .find(|&x| word_at(tokens, x, "METHOD-ID"))
                .unwrap_or(body_end);
        } else {
            diags.push(map.diagnostic(
                tokens[j].origin,
                "text in an OBJECT or FACTORY section is skipped",
            ));
            j = (j + 1..body_end)
                .find(|&x| word_at(tokens, x, "METHOD-ID"))
                .unwrap_or(body_end);
        }
    }
    (section, used)
}

/// Parses the `INTERFACE-ID` unit at `tokens[0]`: its header and its method prototypes (each a
/// METHOD-ID, whose DATA and PROCEDURE divisions are read but not kept). `None` under a dialect
/// without OO recognition or when `tokens` does not start with `INTERFACE-ID`.
pub fn parse_interface(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
) -> Option<Parsed<Interface>> {
    if !dialect.oo_enabled() || !tokens.first().is_some_and(|t| is_word(t, "INTERFACE-ID")) {
        return None;
    }
    let h = header(tokens);
    let mut diags = Vec::new();
    let (body_end, used, closed) = unit_end(tokens, h.period + 1, "INTERFACE", class_boundary);
    if !closed {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "INTERFACE-ID has no END INTERFACE; the interface ends at the next unit",
        ));
    }
    let Some(name) = h.name else {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "INTERFACE-ID has no interface name; the interface is skipped",
        ));
        return Some(Parsed {
            unit: None,
            used,
            diags,
        });
    };
    let mut prototypes = Vec::new();
    let mut k = (h.period + 1).min(body_end);
    while k < body_end {
        if word_at(tokens, k, "METHOD-ID") {
            let Some(m) = parse_method(map, dialect, &tokens[k..body_end], false) else {
                k += 1;
                continue;
            };
            diags.extend(m.diags);
            if let Some(method) = m.unit {
                prototypes.push(MethodPrototype {
                    name: method.name,
                    span: method.span,
                });
            }
            k += m.used.max(1);
        } else {
            diags.push(map.diagnostic(tokens[k].origin, "text in an INTERFACE-ID is skipped"));
            k = (k + 1..body_end)
                .find(|&x| word_at(tokens, x, "METHOD-ID"))
                .unwrap_or(body_end);
        }
    }
    Some(Parsed {
        unit: Some(Interface {
            name,
            span: span(tokens, 0, used - 1),
            identification: span(tokens, 0, h.period),
            inherits: h.inherits,
            using: Vec::new(),
            prototypes,
        }),
        used,
        diags,
    })
}

/// Parses the `METHOD-ID` unit at `tokens[0]`: its header, its DATA DIVISION and its PROCEDURE
/// DIVISION. `factory` is set by the FACTORY section the method sits in. `None` under a dialect
/// without OO recognition or when `tokens` does not start with `METHOD-ID`.
pub fn parse_method(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
    factory: bool,
) -> Option<Parsed<Method>> {
    if !dialect.oo_enabled() || !tokens.first().is_some_and(|t| is_word(t, "METHOD-ID")) {
        return None;
    }
    let h = header(tokens);
    let mut diags = Vec::new();
    let (body_end, used, closed) = unit_end(tokens, h.period + 1, "METHOD", method_boundary);
    if !closed {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "METHOD-ID has no END METHOD; the method ends at the next unit",
        ));
    }
    let Some(name) = h.name else {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "METHOD-ID has no method name; the method is skipped",
        ));
        return Some(Parsed {
            unit: None,
            used,
            diags,
        });
    };
    let start = (h.period + 1).min(body_end);
    let p = (start..body_end)
        .find(|&x| division_at(tokens, x, "PROCEDURE"))
        .unwrap_or(body_end);
    let data = if division_at(tokens, start, "DATA") {
        parse_data_division(map, dialect, &tokens[start..p])
    } else {
        None
    };
    let data = data.map(|(div, ds)| {
        diags.extend(ds);
        div
    });
    let procedure = if p < body_end {
        parse_procedure_division(map, dialect, &tokens[p..body_end])
    } else {
        None
    };
    let procedure = procedure.map(|(div, ds)| {
        diags.extend(ds);
        div
    });
    Some(Parsed {
        unit: Some(Method {
            name,
            span: span(tokens, 0, used - 1),
            factory,
            data,
            procedure,
        }),
        used,
        diags,
    })
}
