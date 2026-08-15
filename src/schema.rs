//! Deserialization of the unified scanner JSONL schema (SPEC §2).
//!
//! One JSON object per line; the `type` field discriminates the record. Node
//! records carry scanner-local opaque `id`s (for `struct`/`function`) or a
//! verbatim `fqn` (`module`/`unresolved`). Edge records reference endpoints by
//! `id` (project node) or `fqn` (unresolved target).

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    /// `{"type":"module","fqn":"github.com/foundry/flow"}`
    Module { fqn: String },

    /// `{"type":"struct","id":"n12","parent":"...","name":"Error","path":"/abs/error.go","start":12,"end":300}`
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: u32,
        end: u32,
    },

    /// `{"type":"function","id":"n13","parent":"...","name":"ComputeContentHash","params":["[]byte","int"],"file":"/abs/store.go","path":"/abs/store.go","start":1,"end":99}`
    Function {
        id: String,
        parent: String,
        name: String,
        #[serde(default)]
        params: Vec<String>,
        #[serde(default)]
        file: String,
        path: String,
        start: u32,
        end: u32,
    },

    /// `{"type":"unresolved","fqn":"fmt.Errorf","category":"stdlib"}`
    Unresolved {
        fqn: String,
        #[serde(default)]
        category: Option<String>,
    },

    Contains { from: String, to: String },
    Calls { from: String, to: String },
    Uses { from: String, to: String },

    UnresolvedCall {
        from: String,
        to: String,
        #[serde(default)]
        target_type: String,
    },
    UnresolvedUse { from: String, to: String },
}
