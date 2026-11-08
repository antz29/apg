//! Shared test helpers for the Rust frontend, reached only under `cfg(test)`.

use syntax::ast::{self, AstNode};

/// Parses `source` in memory and returns the identity of its first call's callee.
fn callee_of(source: &str) -> String {
    let parse = ast::SourceFile::parse(source, syntax::Edition::CURRENT);
    let file = parse.tree();
    let call = file
        .syntax()
        .descendants()
        .find_map(ast::CallExpr::cast)
        .expect("snippet has a call expression");
    crate::walk::callee_identity(&call.expr().expect("call has a callee"), "site.rs")
}

mod unit {
    use super::*;

    /// A bare-identifier callee keeps its name verbatim — no sentinel, no cut.
    #[test]
    fn bare_identifier_identity_is_preserved() {
        assert_eq!(
            callee_of("fn f() { computeContentHash(); }"),
            "computeContentHash"
        );
    }
}
