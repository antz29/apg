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
    /// Spec/plan edge sets (SPEC R2/R21). Endpoints are canonical FQNs.
    pub details: HashSet<(String, String)>,
    pub reviews: HashSet<(String, String)>,
    pub depends_on: HashSet<(String, String)>,
    pub gates: HashSet<(String, String)>,
    pub spec_depends: HashSet<(String, String)>,
    pub anchors: HashSet<(String, String)>,
    pub implements: HashSet<(String, String)>,
    pub satisfies: HashSet<(String, String)>,
    pub builds: HashSet<(String, String)>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Location {
    pub path: PathBuf,
    /// 0-based byte offsets of the node's span in `path`. File nodes carry
    /// no byte span (start/end are 0); their span is the line range only.
    pub start: u32,
    pub end: u32,
    /// 1-based inclusive line range of the node's span.
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Module,
    Struct,
    Function,
    File,
    UnresolvedTarget,
    Spec,
    Requirement,
    Phase,
    Decision,
    Future,
    NonGoal,
    AcceptanceCriterion,
    VerificationItem,
    Note,
    Feedback,
    Plan,
    PlanPhase,
    Task,
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
    /// user-defined). Lets queries filter test (or other) code out. Empty for
    /// spec/plan nodes and Modules/UnresolvedTargets.
    pub code_type: String,
    /// Spec/plan node properties. Only the kinds that own a field set it;
    /// the rest stay `None` (SPEC R1/R20).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    /// `Future.kind` / `Note.kind` string (function/struct/service/... and
    /// background/design/...); distinct from the `NodeKind` enum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliverable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            kind: NodeKind::Module,
            location: None,
            category: None,
            code_type: String::new(),
            title: None,
            goal: None,
            id: None,
            feature: None,
            body: None,
            summary: None,
            number: None,
            sub_kind: None,
            target: None,
            deliverable: None,
            strategy: None,
            tier: None,
            status: None,
            disposition: None,
        }
    }
}