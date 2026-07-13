use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Graph {
    pub nodes: HashMap<String, Node>,
    pub contains: HashSet<(String, String)>,
    pub calls: HashSet<(String, String)>,
    pub uses: HashSet<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub path: PathBuf,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Module,
    Struct,
    Function,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub kind: NodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}
