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

    /// A nameless callee (a closure IIFE) is identified by its site: stable for
    /// one site, distinct across two sites, and a marker — never the callee's
    /// source text nor the checkout path.
    #[test]
    fn nameless_callee_gets_a_site_sentinel() {
        let at_a = callee_of("fn f() { (|| 1)(); }");
        let at_a_again = callee_of("fn f() { (|| 1)(); }");
        let shifted = callee_of("fn longer() { (|| 1)(); }");

        // Stable per site.
        assert_eq!(at_a, at_a_again);
        // Distinct across sites (the closure sits at a different byte offset).
        assert_ne!(at_a, shifted);
        // Never the callee's source text.
        assert!(!at_a.contains("|| 1"));
        assert!(!at_a.contains("=>"));
        // Never the checkout path: no filesystem separator, stays repo-relative.
        assert!(!at_a.contains('/'));
        assert!(!at_a.contains('\\'));
        // Distinguishes the marker from a symbol — it is not a plain identifier.
        assert!(at_a.starts_with('<'));
        assert_ne!(at_a, "f");
    }

    /// A callee name longer than 64 bytes comes back whole — no cut, no ellipsis.
    #[test]
    fn long_callee_name_is_not_truncated() {
        let name = "a".repeat(70);
        let identity = callee_of(&format!("fn f() {{ {name}(); }}"));

        assert_eq!(identity, name);
        assert!(identity.len() > 64);
        // The tail past the old 64-byte cut survives: not a truncated prefix.
        assert!(identity.get(64..).is_some_and(|tail| !tail.is_empty()));
        // No ellipsis marker (U+2026).
        assert!(!identity.contains('…'));
    }
}
