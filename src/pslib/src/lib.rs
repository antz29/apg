//! psfrontend: the PowerShell frontend for apg. It parses PowerShell sources
//! with the pinned tree-sitter-powershell grammar and emits unified JSONL facts.

pub mod parse;

#[cfg(test)]
mod tests;
