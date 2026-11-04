//! REPLACE and REPLACE OFF over lexer tokens. Matching is token by token: separators (`,` and
//! `;`) are insignificant and words compare case-insensitively. A replaced token takes the
//! position of the first source token it replaces, so the source map can locate it.

use crate::diag::Diagnostic;
use crate::lexer::{Token, TokenKind};

/// One `==pattern== BY ==replacement==` pair of the latest REPLACE statement.
struct Pair {
    pattern: Vec<Token>,
    replacement: Vec<Token>,
}

/// Applies the REPLACE statements in `tokens`, dropping each statement. Returns the replaced
/// tokens and the diagnostics for malformed statements.
pub fn replace(file: &str, tokens: Vec<Token>) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut out = Vec::new();
    let mut diags = Vec::new();
    let mut pairs: Vec<Pair> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if is_word(&tokens[i], "REPLACE") {
            i = statement(file, &tokens, i, &mut pairs, &mut diags);
        } else if let Some((pair, end)) = pairs
            .iter()
            .find_map(|p| matched(&tokens, i, &p.pattern).map(|end| (p, end)))
        {
            let at = &tokens[i];
            out.extend(pair.replacement.iter().map(|t| Token {
                offset: at.offset,
                line: at.line,
                col: at.col,
                ..t.clone()
            }));
            i = end;
        } else {
            out.push(tokens[i].clone());
            i += 1;
        }
    }
    (out, diags)
}

/// Reads the REPLACE statement at `start`, updates `pairs` and returns the index after it.
/// A malformed statement leaves `pairs` as they were and skips to its period.
fn statement(
    file: &str,
    tokens: &[Token],
    start: usize,
    pairs: &mut Vec<Pair>,
    diags: &mut Vec<Diagnostic>,
) -> usize {
    if tokens.get(start + 1).is_some_and(|t| is_word(t, "OFF")) {
        pairs.clear();
        return after_period(tokens, start + 2);
    }
    let mut next = Vec::new();
    let mut i = start + 1;
    loop {
        let Some((pair, end)) = read_pair(tokens, i) else {
            let at = &tokens[start];
            diags.push(Diagnostic {
                file: file.to_string(),
                line: at.line,
                col: at.col,
                message: "REPLACE needs ==pseudo-text== BY ==pseudo-text==".to_string(),
            });
            return skip_past_period(tokens, i);
        };
        next.push(pair);
        i = end;
        if tokens.get(i).is_some_and(|t| t.kind == TokenKind::Comma) {
            i += 1;
        }
        if !tokens
            .get(i)
            .is_some_and(|t| t.kind == TokenKind::PseudoDelim)
        {
            break;
        }
    }
    *pairs = next;
    after_period(tokens, i)
}

/// Reads `==pattern== BY ==replacement==` at `i`; returns the pair and the index after it.
fn read_pair(tokens: &[Token], i: usize) -> Option<(Pair, usize)> {
    let (pattern, i) = pseudo_text(tokens, i)?;
    let i = match tokens.get(i) {
        Some(t) if is_word(t, "BY") => i + 1,
        _ => return None,
    };
    let (replacement, i) = pseudo_text(tokens, i)?;
    Some((
        Pair {
            pattern,
            replacement,
        },
        i,
    ))
}

/// The tokens between the `==` delimiters opening at `i`, and the index after the closing one.
fn pseudo_text(tokens: &[Token], i: usize) -> Option<(Vec<Token>, usize)> {
    if tokens.get(i)?.kind != TokenKind::PseudoDelim {
        return None;
    }
    let end = i
        + 1
        + tokens[i + 1..]
            .iter()
            .position(|t| t.kind == TokenKind::PseudoDelim)?;
    Some((tokens[i + 1..end].to_vec(), end + 1))
}

/// The index after the period at `i`, or `i` when there is none.
fn after_period(tokens: &[Token], i: usize) -> usize {
    match tokens.get(i) {
        Some(t) if t.kind == TokenKind::Period => i + 1,
        _ => i,
    }
}

/// The index after the first period at or after `i` (or the end of the tokens).
fn skip_past_period(tokens: &[Token], i: usize) -> usize {
    tokens[i.min(tokens.len())..]
        .iter()
        .position(|t| t.kind == TokenKind::Period)
        .map_or(tokens.len(), |n| i + n + 1)
}

/// The index after the input tokens that match `pattern` starting at `start`, if any. A
/// pattern with no significant token matches nothing.
fn matched(tokens: &[Token], start: usize, pattern: &[Token]) -> Option<usize> {
    let mut want = pattern.iter().filter(|t| !is_separator(t)).peekable();
    if want.peek().is_none() || tokens.get(start).is_none_or(is_separator) {
        return None;
    }
    let mut i = start;
    for expected in want {
        while tokens.get(i).is_some_and(is_separator) {
            i += 1;
        }
        if !same(tokens.get(i)?, expected) {
            return None;
        }
        i += 1;
    }
    Some(i)
}

fn is_separator(t: &Token) -> bool {
    matches!(t.kind, TokenKind::Comma | TokenKind::Semicolon)
}

fn is_word(t: &Token, word: &str) -> bool {
    matches!(t.kind, TokenKind::Word | TokenKind::Reserved) && t.text.eq_ignore_ascii_case(word)
}

fn same(a: &Token, b: &Token) -> bool {
    a.kind == b.kind
        && if matches!(a.kind, TokenKind::Word | TokenKind::Reserved) {
            a.text.eq_ignore_ascii_case(&b.text)
        } else {
            a.text == b.text
        }
}
