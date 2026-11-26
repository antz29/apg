//! bicepfrontend: the Azure Bicep frontend for apg. It parses Bicep templates and
//! parameter files with a hand-written parser and emits unified JSONL facts on
//! stdout. It is self-contained and spawns no child processes.

pub mod diag;
pub mod parse;
pub mod source;
pub mod units;
