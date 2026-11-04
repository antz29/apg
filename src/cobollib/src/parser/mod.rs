//! Parser entry: the program structure of a COBOL file. Splits the token stream into
//! `IDENTIFICATION DIVISION` programs, keeps the ENVIRONMENT span, and hands the DATA and
//! PROCEDURE slices to [`data`] and [`procedure`]. A division ends where the next division
//! header or program begins, so a broken division loses only its own tokens. Entry points:
//! [`parse_compilation_unit`] over mapped tokens, and [`parse_source`], the in-memory reader.

pub mod data;
pub mod procedure;

use std::path::Path;

use crate::ast::{CompilationUnit, Name, Program, Span};
use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::preprocess::{self, Search};
use crate::source_map::{Mapped, SourceMap};

const IDENTIFICATION: &[&str] = &["IDENTIFICATION", "ID"];

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
    };
    let mut used = tokens.len();
    let mut i = id_end;
    while i < tokens.len() {
        if let Some(next) = end_program(tokens, i) {
            used = next;
            break;
        }
        if header(tokens, i, IDENTIFICATION) {
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
    let mut diags = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if header(tokens, i, IDENTIFICATION) {
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
    (CompilationUnit { programs }, diags)
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
