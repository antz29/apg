//! apg COBOL frontend crate (`cobolfrontend`).

pub mod diag;
pub mod dialect;
pub mod format;
pub mod lexer;
pub mod replace;

#[cfg(test)]
mod tests;
