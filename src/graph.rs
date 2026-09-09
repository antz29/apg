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
    /// Spine edges (GraphModel-SPEC.md; PHASE_01): end-to-end why-to-code
    /// traceability through the Domain and Solution tiers.
    pub drives: HashSet<(String, String)>,
    pub requires: HashSet<(String, String)>,
    pub realises: HashSet<(String, String)>,
    pub represents: HashSet<(String, String)>,
    pub implemented_by: HashSet<(String, String)>,
    /// Invariant edges (Invariants-SPEC.md; PHASE_02): artifact → Invariant
    /// (`GuardedBy`) and Feedback → Invariant (`Checks`).
    pub guarded_by: HashSet<(String, String)>,
    pub checks: HashSet<(String, String)>,
    /// New-model §3.3 spec edges (apg-projects). The kebab spellings
    /// (`realised-by`, `implemented-by`) are the §3.3 wire names — distinct
    /// from the old spine edges (`Realises`, `ImplementedBy`) that task-18
    /// removes. `calls`/`uses`/`contains`/`drives`/`represents`/`details`/
    /// `depends-on` reuse the scanner/spine sets above and are routed by
    /// endpoint node-kind at load time (`depends-on` is the same
    /// Requirement→Requirement relation as `DependsOn`).
    pub realised_by: HashSet<(String, String)>,
    pub spec_implemented_by: HashSet<(String, String)>,
    pub publishes: HashSet<(String, String)>,
    pub subscribes: HashSet<(String, String)>,
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
    NonGoal,
    AcceptanceCriterion,
    VerificationItem,
    Note,
    Feedback,
    Plan,
    PlanPhase,
    Task,
    // Tier-1/2/3 graph-native spec nodes (GraphModel-SPEC.md; PHASE_01).
    // Tier 1: Stakeholder. Tier 2 (DDD): Domain, Subdomain, Entity,
    // ValueObject, Aggregate, DomainEvent, DomainProcess, DomainRule, Actor.
    // Tier 3 (C4): System, Container, Component.
    Stakeholder,
    Domain,
    Subdomain,
    Entity,
    ValueObject,
    Aggregate,
    DomainEvent,
    DomainProcess,
    DomainRule,
    Actor,
    System,
    Container,
    Component,
    /// The graph-wide invariant mechanism (Invariants-SPEC.md; PHASE_02): a
    /// rule artifacts must respect, guardable onto them (`GuardedBy`) and
    /// citable from review feedback (`Checks`).
    Invariant,
    // --- New-model tier catalog (apg-projects SPEC §3.1). ADD-ONLY: the old
    // ValueObject/DomainProcess/Actor/Invariant/etc. variants stay until
    // task-18 removes them; these new kinds coexist. ---
    /// `User` ⊂ Stakeholder — "a thing that uses the system" (requirements).
    User,
    /// `Group` — the hierarchical domain container (groups in groups;
    /// attribute core/supporting/generic, optional root). BoundedContext/
    /// Subdomain/Aggregate/DomainRule collapse into it.
    Group,
    /// `Value` — immutable (was ValueObject).
    Value,
    /// `Service` — stateless behaviour (was DomainProcess).
    Service,
    /// `Person` — the C4 view of User/Stakeholder (solution).
    Person,
    /// `Constraint` — declarative prose ("X must hold"); structure/reference
    /// validation only, satisfaction by review (was Invariant).
    Constraint,
    /// The scan-time git-state node (fqn `scan/HEAD`, one per DB; rewritten at
    /// every scan). Standalone — it carries no rel tables.
    Scan,
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
    /// The short name of a tier-1/2/3 spec node (Stakeholder/Domain/Entity/…).
    /// Distinct from the FQN's final segment only by spelling (e.g. a
    /// `value-object.Email` node's `name` is `Email`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The aggregate-root entity name of an `Aggregate` node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// The `attribute` of a `Group` node (`core`/`supporting`/`generic`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    /// The `attaches-to` FQN of a `Constraint` node — the one tier-1–3 node a
    /// local constraint constrains (a global constraint carries none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attaches_to: Option<String>,
    /// The `scope` of an `Invariant` node (the artifact kind it applies to:
    /// spec/plan/review/code/…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    /// `Note.kind` string (background/design/...); distinct from the
    /// `NodeKind` enum. (The placeholder `kind`-alignment is gone — planned
    /// Implementation nodes carry their kind as a DB label, not a string.)
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
    /// The node's lifecycle status. For the four Implementation node kinds a
    /// `Some("planned")` marks a plan-writer-authored placeholder (the code
    /// does not exist yet; the scanner replaces it on realization); the
    /// scanner never emits a planned node. Feedback/Task/Invariant carry their
    /// own statuses (open/resolved, pending/done, active/retired).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    /// Scan-node properties (SPEC agent-loop hardening): the git state the
    /// scan ran under. `git_sha`/`git_clean` are `None` when the scanned dir
    /// is not a git repo; only the `Scan` kind sets them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_clean: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<String>,
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
            name: None,
            root: None,
            attribute: None,
            attaches_to: None,
            scope: None,
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
            git_sha: None,
            git_clean: None,
            scanned_at: None,
        }
    }
}
