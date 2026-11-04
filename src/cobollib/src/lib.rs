//! apg COBOL frontend crate (`cobolfrontend`).

pub mod ast;
pub mod diag;
pub mod dialect;
pub mod format;
pub mod lexer;
pub mod parser;
pub mod preprocess;
pub mod replace;
pub mod resolve;
pub mod source_map;

#[cfg(test)]
mod tests;
