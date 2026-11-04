//! EXEC block recognition on the PROCEDURE DIVISION parser. An `EXEC CICS` or `EXEC SQL` block
//! runs to its `END-EXEC`. Under `ibm` it is one statement; under any other dialect it is an
//! error and is skipped. Entry points: [`is_exec_block`] and [`exec_block_end`] (free, for the
//! DATA DIVISION skipper too), and [`Parser::exec_block`] (statement dispatch).

use super::procedure::{is, Parser};
use crate::ast::{CallTarget, QualifiedName, Statement, Transfer};
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::source_map::Mapped;

/// Whether token `k` opens an EXEC block: `EXEC` followed by `CICS` or `SQL`.
pub(super) fn is_exec_block(tokens: &[Mapped], k: usize) -> bool {
    k + 1 < tokens.len()
        && is(&tokens[k], "EXEC")
        && (is(&tokens[k + 1], "CICS") || is(&tokens[k + 1], "SQL"))
}

/// The index after the `END-EXEC` that closes the block whose `EXEC` is at `start`, searched in
/// `start..end`; `None` when no `END-EXEC` comes before `end`. A skipper resumes at the returned
/// index, or at `end` (the sentence period) on `None`.
pub(super) fn exec_block_end(tokens: &[Mapped], start: usize, end: usize) -> Option<usize> {
    (start..end)
        .find(|&k| is(&tokens[k], "END-EXEC"))
        .map(|k| k + 1)
}

impl Parser<'_> {
    /// The EXEC block at `i` (see [`is_exec_block`]), read to the `END-EXEC` before `end`. Under
    /// `ibm` it is one statement. Under any other dialect it is one error and is skipped through
    /// its `END-EXEC`. Without an `END-EXEC` it is an error and skips to `end`. Returns the
    /// statement (`None` when skipped) and the index after the block.
    pub(super) fn exec_block(&mut self, i: usize, end: usize) -> (Option<Statement>, usize) {
        let Some(after) = exec_block_end(self.tokens, i, end) else {
            let at = self.origin(i);
            self.diag(at, "EXEC block is not ended by END-EXEC");
            return (None, end);
        };
        if self.dialect != Dialect::Ibm {
            let at = self.origin(i);
            self.diag(at, "an EXEC block is recognised only under ibm");
            return (None, after);
        }
        let close = after - 1;
        let statement = if is(&self.tokens[i + 1], "CICS") {
            self.cics(i, close)
        } else {
            self.sql(i, close)
        };
        (Some(statement), after)
    }

    /// `EXEC CICS verb operands END-EXEC`, the block's words `i+2..close`. `PROGRAM` on `LINK` or
    /// `XCTL` is a call; `SERVICE` or `WEBSERVICE` on `INVOKE` is an external service. Other
    /// operands, and the transaction verbs, are no transfer and no reference.
    fn cics(&self, i: usize, close: usize) -> Statement {
        let t = self.tokens;
        let verb = t[i + 2].token.text.to_ascii_uppercase();
        let mut target = None;
        let mut k = i + 3;
        while k < close {
            let keyword = t[k].token.text.to_ascii_uppercase();
            let operand = (matches!(keyword.as_str(), "PROGRAM" | "SERVICE" | "WEBSERVICE")
                && k + 2 < close
                && t[k + 1].token.kind == TokenKind::LParen)
                .then(|| k + 2)
                .and_then(|o| {
                    if matches!(t[o].token.kind, TokenKind::Alnum | TokenKind::National) {
                        Some((CallTarget::Literal(self.name(o)), o + 1))
                    } else if self.is_name(o) {
                        let (name, next) = self.qualified(o, close);
                        Some((CallTarget::Identifier(name), next))
                    } else {
                        None
                    }
                });
            match operand {
                Some((op, next)) => {
                    if target.is_none() {
                        target = Some((keyword, op));
                    }
                    k = next;
                }
                None => k += 1,
            }
        }
        let transfer = match target {
            Some((keyword, op))
                if keyword == "PROGRAM" && matches!(verb.as_str(), "LINK" | "XCTL") =>
            {
                Some(Transfer::Call(op))
            }
            Some((keyword, op))
                if verb == "INVOKE" && matches!(keyword.as_str(), "SERVICE" | "WEBSERVICE") =>
            {
                Some(Transfer::ExecService(op))
            }
            _ => None,
        };
        Statement {
            span: self.span(i, close + 1),
            scope_end: None,
            transfer,
            refs: Vec::new(),
            children: Vec::new(),
        }
    }

    /// `EXEC SQL ... END-EXEC`, the block's words `i+2..close`. Host variables are references;
    /// `CALL name` with a bare name is a call. Table names and SQL functions are not references.
    fn sql(&self, i: usize, close: usize) -> Statement {
        let t = self.tokens;
        let transfer = (i + 3 < close && is(&t[i + 2], "CALL") && self.is_name(i + 3))
            .then(|| Transfer::Call(CallTarget::Literal(self.name(i + 3))));
        Statement {
            span: self.span(i, close + 1),
            scope_end: None,
            transfer,
            refs: self.host_vars(i + 2, close),
            children: Vec::new(),
        }
    }

    /// The host variables `:name` in `a..close`. The indicator of `:HV:IND`, a colon touching the
    /// host variable before it, is not a reference: only the host variable is.
    fn host_vars(&self, a: usize, close: usize) -> Vec<QualifiedName> {
        let t = self.tokens;
        let colon = |k: usize| matches!(t[k].token.kind, TokenKind::Symbol(':'));
        let touching =
            |k: usize| t[k].token.offset + t[k].token.text.len() == t[k + 1].token.offset;
        let mut refs = Vec::new();
        let mut k = a;
        while k < close {
            if colon(k) && k + 1 < close && self.is_name(k + 1) {
                let (name, mut next) = self.qualified(k + 1, close);
                refs.push(name);
                if next + 1 < close && colon(next) && touching(next - 1) && self.is_name(next + 1) {
                    next = self.qualified(next + 1, close).1;
                }
                k = next;
            } else {
                k += 1;
            }
        }
        refs
    }
}
