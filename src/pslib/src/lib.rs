//! psfrontend: the PowerShell frontend for apg. It parses PowerShell sources
//! with the pinned woodruffw-trough-tree-sitter-powershell grammar and emits
//! unified JSONL facts.

pub mod diag;
pub mod identity;
pub mod manifest;
pub mod parse;
pub mod paths;
pub mod scan;
pub mod source;
pub mod units;

#[cfg(test)]
mod tests;
