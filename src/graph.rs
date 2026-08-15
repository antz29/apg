use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Graph {
    pub nodes: HashMap<String, Node>,
    pub contains: HashSet<(String, String)>,
    pub calls: HashSet<(String, String)>,
    pub uses: HashSet<(String, String)>,
    /// Unresolved calls: (source, target, target_type). target_type is the
    /// function type of a func-value call (empty when not applicable).
    pub unresolved_calls: HashSet<(String, String, String)>,
    pub unresolved_uses: HashSet<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub path: PathBuf,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Module,
    Struct,
    Function,
    UnresolvedTarget,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub kind: NodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    /// Classification of an UnresolvedTarget: builtin/stdlib/external/
    /// func-value/interface-method/unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Code type of Struct/Function nodes (src/test/generated/external/lib/
    /// user-defined). Lets queries filter test (or other) code out.
    pub code_type: String,
}
