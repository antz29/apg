//! The pinned-grammar entry: the tree-sitter-powershell language and a parse
//! of decoded text that always yields a tree.

use tree_sitter::{Language, Parser, Tree};

/// The tree-sitter-powershell language handle.
pub fn language() -> Language {
    tree_sitter_powershell::LANGUAGE.into()
}

/// Parses decoded PowerShell text. Error recovery leaves ERROR/MISSING nodes
/// in the tree; it never fails.
pub fn parse(text: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .expect("pinned tree-sitter-powershell grammar matches the pinned tree-sitter ABI");
    // No timeout or cancellation is set, so tree-sitter always returns a tree.
    parser
        .parse(text, None)
        .expect("parse without timeout yields a tree")
}
