//! Parser entry: the program structure of a COBOL file. Splits the token stream into
//! `IDENTIFICATION DIVISION` programs, keeps the ENVIRONMENT span, and hands the DATA and
//! PROCEDURE slices to [`data`] and [`procedure`]. A division ends where the next division
//! header or program begins, so a broken division loses only its own tokens. Entry points:
//! [`parse_compilation_unit`] over mapped tokens, and [`parse_source`], the in-memory reader.

pub mod data;
mod oo;
pub mod procedure;

use std::path::Path;

use crate::ast::{Class, CompilationUnit, Interface, Name, Program, Span};
use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::preprocess::{self, Search};
use crate::source_map::{Mapped, SourceMap};

const IDENTIFICATION: &[&str] = &["IDENTIFICATION", "ID"];

/// An OO unit: a `CLASS-ID` or `INTERFACE-ID` with its header-level result, or a `METHOD-ID`.
enum Oo {
    Class(Option<Box<Class>>),
    Interface(Option<Box<Interface>>),
    Method,
}

/// The OO unit starting at `i`: its keyword, or an `IDENTIFICATION DIVISION` line directly above
/// one. Returns the unit and the tokens it spans from `i`; `None` when no OO unit starts there
/// (always under cobol85).
fn oo_unit(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
    i: usize,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Oo, usize)> {
    let k = if header(tokens, i, IDENTIFICATION) {
        i + 2
            + usize::from(
                tokens
                    .get(i + 2)
                    .is_some_and(|t| t.token.kind == TokenKind::Period),
            )
    } else {
        i
    };
    let head = tokens.get(k)?;
    let (unit, used, ds) = if is_word(head, "CLASS-ID") {
        let p = oo::parse_class(map, dialect, &tokens[k..])?;
        (Oo::Class(p.unit.map(Box::new)), p.used, p.diags)
    } else if is_word(head, "INTERFACE-ID") {
        let p = oo::parse_interface(map, dialect, &tokens[k..])?;
        (Oo::Interface(p.unit.map(Box::new)), p.used, p.diags)
    } else if is_word(head, "METHOD-ID") {
        let p = oo::parse_method(map, dialect, &tokens[k..], false)?;
        (Oo::Method, p.used, p.diags)
    } else {
        return None;
    };
    diags.extend(ds);
    Some((unit, k - i + used.max(1)))
}

fn is_word(t: &Mapped, word: &str) -> bool {
    matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved)
        && t.token.text.eq_ignore_ascii_case(word)
}

/// `<one of words> DIVISION` starts at `i`.
fn header(tokens: &[Mapped], i: usize, words: &[&str]) -> bool {
    match (tokens.get(i), tokens.get(i + 1)) {
        (Some(a), Some(b)) => words.iter().any(|w| is_word(a, w)) && is_word(b, "DIVISION"),
        _ => false,
    }
}

/// The index after `END PROGRAM [name] [.]` starting at `i`.
fn end_program(tokens: &[Mapped], i: usize) -> Option<usize> {
    let (Some(a), Some(b)) = (tokens.get(i), tokens.get(i + 1)) else {
        return None;
    };
    if !(is_word(a, "END") && is_word(b, "PROGRAM")) {
        return None;
    }
    let mut next = i + 2;
    let is_name = tokens
        .get(next)
        .is_some_and(|t| matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved))
        && !header(tokens, next, IDENTIFICATION);
    if is_name {
        next += 1;
    }
    if tokens
        .get(next)
        .is_some_and(|t| t.token.kind == TokenKind::Period)
    {
        next += 1;
    }
    Some(next)
}

/// A program boundary: a nested `IDENTIFICATION DIVISION` or an `END PROGRAM`.
fn boundary(tokens: &[Mapped], i: usize) -> bool {
    header(tokens, i, IDENTIFICATION) || end_program(tokens, i).is_some()
}

/// Whether `END PROGRAM <name>` appears at or after `from`: only then can a program nest inside `name`.
fn closed_later(tokens: &[Mapped], from: usize, name: &str) -> bool {
    (from..tokens.len()).any(|k| {
        end_program(tokens, k).is_some() && tokens.get(k + 2).is_some_and(|t| is_word(t, name))
    })
}

/// The first index after `from` where a division, or a program boundary, begins.
fn section_end(tokens: &[Mapped], from: usize) -> usize {
    (from..tokens.len())
        .find(|&k| header(tokens, k, &["ENVIRONMENT", "DATA", "PROCEDURE"]) || boundary(tokens, k))
        .unwrap_or(tokens.len())
}

fn next_identification(tokens: &[Mapped], from: usize) -> usize {
    (from..tokens.len())
        .find(|&k| header(tokens, k, IDENTIFICATION))
        .unwrap_or(tokens.len())
}

fn span(first: &Mapped, last: &Mapped) -> Span {
    Span {
        start: first.origin,
        end: last.origin,
    }
}

/// The non-data names an ENVIRONMENT DIVISION declares: SELECT [OPTIONAL] file names, and the
/// SPECIAL-NAMES mnemonic (after IS), ALPHABET, CLASS and ON/OFF [STATUS] [IS] names.
fn environment_names(tokens: &[Mapped]) -> Vec<Name> {
    fn declare(tokens: &[Mapped], k: usize, out: &mut Vec<Name>) {
        // `ON STATUS IS n` is seen by both the ON and IS arms; a name is declared once.
        if let Some(t) = tokens.get(k).filter(|t| t.token.kind == TokenKind::Word) {
            if out
                .iter()
                .any(|n| n.text.eq_ignore_ascii_case(&t.token.text))
            {
                return;
            }
            out.push(Name {
                text: t.token.text.clone(),
                span: span(t, t),
            });
        }
    }
    let mut out = Vec::new();
    let mut in_special = false;
    for (i, t) in tokens.iter().enumerate() {
        if !matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved) {
            continue;
        }
        match t.token.text.to_ascii_uppercase().as_str() {
            "SELECT" => {
                let optional = tokens.get(i + 1).is_some_and(|n| is_word(n, "OPTIONAL"));
                declare(tokens, i + 1 + usize::from(optional), &mut out);
            }
            "SPECIAL-NAMES" => in_special = true,
            "FILE-CONTROL" | "I-O-CONTROL" => in_special = false,
            "IS" | "ALPHABET" | "CLASS" if in_special => declare(tokens, i + 1, &mut out),
            "ON" | "OFF" if in_special => {
                let mut k = i + 1;
                if tokens.get(k).is_some_and(|n| is_word(n, "STATUS")) {
                    k += 1;
                }
                if tokens.get(k).is_some_and(|n| is_word(n, "IS")) {
                    k += 1;
                }
                declare(tokens, k, &mut out);
            }
            _ => {}
        }
    }
    out
}

/// Parses the program at the start of `tokens` (an `IDENTIFICATION DIVISION`) and returns it
/// with the number of tokens it used. `None` when it has no `PROGRAM-ID`: one diagnostic is
/// pushed and the caller resumes at the next program.
fn parse_program(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
    diags: &mut Vec<Diagnostic>,
) -> Option<(Program, usize)> {
    let id_end = section_end(tokens, 2);
    let name = (0..id_end)
        .find(|&k| is_word(&tokens[k], "PROGRAM-ID"))
        .and_then(|pos| (pos + 1..id_end).find(|&k| tokens[k].token.kind != TokenKind::Period))
        .filter(|&k| matches!(tokens[k].token.kind, TokenKind::Word | TokenKind::Reserved));
    let Some(name) = name else {
        diags.push(map.diagnostic(
            tokens[0].origin,
            "IDENTIFICATION DIVISION has no PROGRAM-ID; the program is skipped",
        ));
        return None;
    };
    let mut prog = Program {
        name: Name {
            text: tokens[name].token.text.clone(),
            span: span(&tokens[name], &tokens[name]),
        },
        span: span(&tokens[0], &tokens[0]),
        identification: span(&tokens[0], &tokens[id_end - 1]),
        environment: None,
        data: None,
        procedure: None,
        nested: Vec::new(),
        declared: Vec::new(),
        repository: Vec::new(),
        classes: Vec::new(),
    };
    let mut used = tokens.len();
    let mut i = id_end;
    while i < tokens.len() {
        if let Some(next) = end_program(tokens, i) {
            used = next;
            break;
        }
        if let Some((unit, n)) = oo_unit(map, dialect, tokens, i, diags) {
            match unit {
                Oo::Class(c) => prog.classes.extend(c.map(|c| *c)),
                Oo::Interface(_) => diags.push(map.diagnostic(
                    tokens[i].origin,
                    "INTERFACE-ID nested in a program is skipped",
                )),
                Oo::Method => diags.push(map.diagnostic(
                    tokens[i].origin,
                    "METHOD-ID outside a CLASS-ID or INTERFACE-ID is skipped",
                )),
            }
            i += n;
        } else if header(tokens, i, IDENTIFICATION) {
            // Without an END PROGRAM for this program, the next header is a sibling, not nested.
            if !closed_later(tokens, i, &prog.name.text) {
                used = i;
                break;
            }
            match parse_program(map, dialect, &tokens[i..], diags) {
                Some((nested, n)) => {
                    prog.nested.push(nested);
                    i += n;
                }
                None => i = next_identification(tokens, i + 1),
            }
        } else if header(tokens, i, &["ENVIRONMENT"]) {
            let e = section_end(tokens, i + 1);
            prog.environment = Some(span(&tokens[i], &tokens[e - 1]));
            prog.declared = environment_names(&tokens[i..e]);
            prog.repository
                .extend(oo::repository(dialect, &tokens[i..e]));
            i = e;
        } else if header(tokens, i, &["DATA"]) {
            let e = section_end(tokens, i + 1);
            if let Some((data, ds)) = data::parse_data_division(map, dialect, &tokens[i..e]) {
                prog.data = Some(data);
                diags.extend(ds);
            }
            i = e;
        } else if header(tokens, i, &["PROCEDURE"]) {
            let e = section_end(tokens, i + 1);
            if let Some((procedure, ds)) =
                procedure::parse_procedure_division(map, dialect, &tokens[i..e])
            {
                prog.procedure = Some(procedure);
                diags.extend(ds);
            }
            i = e;
        } else {
            let e = section_end(tokens, i + 1);
            diags.push(map.diagnostic(tokens[i].origin, "text outside a division is skipped"));
            i = e;
        }
    }
    prog.span = span(&tokens[0], &tokens[used - 1]);
    Some((prog, used))
}

/// Parses mapped tokens into the file's programs. Diagnostics come in scan order; every
/// declaration that parses is kept even when the file has errors.
pub fn parse_compilation_unit(
    map: &SourceMap,
    dialect: Dialect,
    tokens: &[Mapped],
) -> (CompilationUnit, Vec<Diagnostic>) {
    let mut programs = Vec::new();
    let mut classes = Vec::new();
    let mut interfaces = Vec::new();
    let mut diags = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Some((unit, n)) = oo_unit(map, dialect, tokens, i, &mut diags) {
            match unit {
                Oo::Class(c) => classes.extend(c.map(|c| *c)),
                Oo::Interface(x) => interfaces.extend(x.map(|x| *x)),
                Oo::Method => diags.push(map.diagnostic(
                    tokens[i].origin,
                    "METHOD-ID outside a CLASS-ID or INTERFACE-ID is skipped",
                )),
            }
            i += n;
        } else if header(tokens, i, IDENTIFICATION) {
            match parse_program(map, dialect, &tokens[i..], &mut diags) {
                Some((program, n)) => {
                    programs.push(program);
                    i += n;
                }
                None => i = next_identification(tokens, i + 1),
            }
        } else {
            diags.push(map.diagnostic(
                tokens[i].origin,
                "expected IDENTIFICATION DIVISION; text before the program is skipped",
            ));
            i = next_identification(tokens, i + 1);
        }
    }
    (
        CompilationUnit {
            programs,
            classes,
            interfaces,
        },
        diags,
    )
}

/// The in-memory reader: COPY and REPLACE expansion over `text` (which runs the format check
/// and the lexer), then the parser. Returns the file's programs and every diagnostic.
pub fn parse_source(
    file: &str,
    text: &str,
    dialect: Dialect,
    search: &Search,
    lookup: &dyn Fn(&Path) -> Option<String>,
) -> (CompilationUnit, Vec<Diagnostic>) {
    let mut map = SourceMap::default();
    let (tokens, mut diags) = preprocess::preprocess(&mut map, file, text, dialect, search, lookup);
    let (unit, more) = parse_compilation_unit(&map, dialect, &tokens);
    diags.extend(more);
    (unit, diags)
}
