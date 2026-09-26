//! Write-through authoring (SPEC R5) and the DB helpers the `apg spec` /
//! `apg plan` / `apg review` CLIs share: opening the live `apg/.trans/db.lbug`
//! read-write, resolving FQNs against the code graph, and re-ingesting a
//! project's spec/plan/note records via Cypher MERGE. Mutations never rebuild
//! the DB — the code graph is untouched; only the project's `…` nodes
//! are detached and re-merged from its JSONL files.
//!
//! The cohesive groups live in submodules — the DB handle, lock plumbing and
//! record lookups ([`db`]), the record/edge MERGE helpers ([`merge`]), and the
//! transient-projection and code-universe helpers ([`project`]) — all
//! re-exported here so `crate::artifacts::<name>` keeps resolving at the
//! original paths. The command-line option parser stays here: many callers use
//! `artifacts::parse_args` / `artifacts::ParsedArgs`.

use std::collections::HashMap;

pub mod db;
pub mod merge;
pub mod project;

pub use db::*;
pub use merge::*;
pub use project::*;

// `rel_pair_allowed` is crate-internal (the merge guard and the unit tests
// exercise it); an item narrower than `pub` cannot travel through a
// `pub use …::*` glob (E0365), so re-export it explicitly here — test builds
// only, since production reaches it inside `merge`.
#[cfg(test)]
pub(crate) use merge::rel_pair_allowed;

// The relocated unit tests reach `Record` through `use super::*` (it was
// imported in this module before the split); keep it reachable in test builds
// only, so a non-test build carries no unused import.
#[cfg(test)]
use crate::schema::Record;

/// A tiny option parser for the spec/plan/review subcommands. Positional args
/// (not starting with `--`) and repeatable flags (`--flag value`, boolean when
/// no value follows).
pub struct ParsedArgs {
    pub positional: Vec<String>,
    pub flags: HashMap<String, Vec<String>>,
}

pub fn parse_args(args: &[String]) -> ParsedArgs {
    let mut positional = Vec::new();
    let mut flags: HashMap<String, Vec<String>> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(name) = args[i].strip_prefix("--") {
            let mut vals = Vec::new();
            while i + 1 < args.len() && !args[i + 1].starts_with("--") {
                vals.push(args[i + 1].clone());
                i += 1;
            }
            flags.entry(name.to_string()).or_default().extend(vals);
        } else {
            positional.push(args[i].clone());
        }
        i += 1;
    }
    ParsedArgs { positional, flags }
}

impl ParsedArgs {
    /// All values of a repeatable flag (empty when absent).
    pub fn all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }
    /// The single value of a flag, or `None`.
    pub fn get(&self, name: &str) -> Option<String> {
        self.flags.get(name).and_then(|v| v.first().cloned())
    }
    /// True when a boolean flag is present.
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
}

#[cfg(test)]
mod tests;
