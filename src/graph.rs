use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
}

impl Graph {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn add_node(&mut self, name: String, kind: NodeKind, file: Option<String>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node { id, name, kind, file, edges: Vec::new() });
        id
    }

    pub fn add_edge(&mut self, source: usize, target: usize, kind: EdgeKind) {
        if let Some(node) = self.nodes.get_mut(source) {
            node.edges.push(OutEdge { target, kind });
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: usize,
    pub name: String,
    pub kind: NodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub edges: Vec<OutEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutEdge {
    pub target: usize,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Package,
    Class,
    Interface,
    Enum,
    Record,
    Method,
    Constructor,
    Field,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    Contains,
    Calls,
}
