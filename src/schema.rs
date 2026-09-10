//! Deserialization of the unified scanner JSONL schema (SPEC §2).
//!
//! One JSON object per line; the `type` field discriminates the record. Node
//! records carry scanner-local opaque `id`s (for `struct`/`function`) or a
//! verbatim `fqn` (`module`/`unresolved`). Edge records reference endpoints by
//! `id` (project node) or `fqn` (unresolved target).

use serde::{Deserialize, Serialize};

/// The FQN of the DB's `Scan` node (the git state of the scan that built the
/// live DB). One per database — every scan replaces it.
pub const SCAN_HEAD: &str = "scan/HEAD";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    /// `{"type":"module","fqn":"github.com/foundry/flow"}`
    Module {
        fqn: String,
    },

    /// `{"type":"struct","id":"n12","parent":"...","name":"Error","path":"/abs/error.go","start":12,"end":300,"start_line":12,"end_line":45}`
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },

    /// `{"type":"function","id":"n13","parent":"...","name":"ComputeContentHash","params":["[]byte","int"],"file":"/abs/store.go","path":"/abs/store.go","start":1,"end":99,"start_line":34,"end_line":99}`
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
        start_line: u32,
        end_line: u32,
    },

    /// `{"type":"file","path":"/abs/store.go","parent":"github.com/foundry/flow","start_line":1,"end_line":142}`
    File {
        path: String,
        #[serde(default)]
        parent: String,
        start_line: u32,
        end_line: u32,
    },

    /// `{"type":"unresolved","fqn":"fmt.Errorf","category":"stdlib"}`
    Unresolved {
        fqn: String,
        #[serde(default)]
        category: Option<String>,
    },

    /// Pipeline-internal control record, emitted by `apg scan` (never by a
    /// scanner) between frontend streams when a scan covers multiple
    /// languages: `{"type":"lang_switch","language":"go"}`. The ingestor uses
    /// the current language for code_type classification and FQN rendering
    /// (e.g. Go `init`), so a merged multi-language stream classifies and
    /// renders each record under its own frontend's language.
    LangSwitch {
        language: String,
    },

    /// Pipeline-internal control record, emitted by `apg scan` at the front of
    /// the merged stream and as **line 1 of `graph.jsonl`**:
    /// `{"type":"scan_meta","git_sha":"...","git_clean":true,"scanned_at":"..."}`.
    /// Carries the git state the scan ran under — HEAD sha plus whether
    /// `git status --porcelain` was empty — so a later mutation gate can tell
    /// whether the live DB matches the tree. The git fields are absent when the
    /// scanned dir is not a git repo. The ingestor records it as the DB's
    /// `Scan` node (fqn [`SCAN_HEAD`]).
    ScanMeta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_clean: Option<bool>,
        scanned_at: String,
    },

    Contains {
        from: String,
        to: String,
    },
    Calls {
        from: String,
        to: String,
    },
    Uses {
        from: String,
        to: String,
    },

    UnresolvedCall {
        from: String,
        to: String,
        #[serde(default)]
        target_type: String,
    },
    UnresolvedUse {
        from: String,
        to: String,
    },

    // --- Spec/plan graph records (SPEC R1/R20; canonical FQNs, no ids) ---
    /// `{"type":"requirement","fqn":"<project>/spec.<id>","id":"R1","title":"...","body":"...","feature":"..."}`
    Requirement {
        fqn: String,
        id: String,
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        feature: String,
    },

    /// `{"type":"planned_node","fqn":"github.com/x/y.Store","kind":"struct","name":"Store","parent":"github.com/x/y"}`
    /// A plan-writer-authored tier-4 addition (GraphModel-SPEC.md): an
    /// Implementation node (module/file/struct/function) marked
    /// `status: planned` at the FQN where the code will land. The scanner never
    /// emits planned nodes; a scan that finds real code at a planned FQN
    /// **replaces** the planned node and re-points its incident edges (the
    /// scanner-replace, PlanExecution-SPEC.md). `name` is the simple name,
    /// `parent` the containing node FQN (both optional — the FQN is the
    /// identity).
    PlannedNode {
        fqn: String,
        kind: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        parent: String,
    },

    // --- Tier-1/2/3 graph-native spec nodes (GraphModel-SPEC.md; PHASE_01) ---
    // The 4-tier taxonomy: tier 1 Requirements (Stakeholder), tier 2 Domain
    // (DDD), tier 3 Solution (C4), tier 4 Implementation (scanner code nodes).
    // These are authored via the spec tools, never scanned. FQNs are
    // project-scoped (`<project>/<slug>.<name>` — PHASE_04 dropped the
    // prefix). `name` is the concept's short name; `body` its description.
    /// `{"type":"stakeholder","fqn":"<project>/stakeholder.<name>","name":"...","body":"..."}`
    Stakeholder {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"entity","fqn":"<project>/entity.<name>","name":"...","body":"..."}`
    Entity {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"system","fqn":"<project>/system.<name>","name":"...","body":"..."}`
    /// The C4 system-context root of the solution.
    System {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"container","fqn":"<project>/container.<name>","name":"...","kind":"app","body":"..."}`
    /// `kind` ∈ app|service|db|queue (the C4 deployable-unit kinds).
    Container {
        fqn: String,
        name: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"component","fqn":"<project>/component.<name>","name":"...","body":"..."}`
    Component {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    // --- New-model tier catalog (apg-projects SPEC §3.1). FQNs are now
    // `<layer>.<type>.<name>` (no project prefix) — derived from the node-file
    // path by `ingest_tree`, never carried on the wire. ---
    /// `{"type":"user","fqn":"requirements.user.<name>","name":"...","body":"..."}`
    /// `User` ⊂ Stakeholder — "a thing that uses the system" (requirements).
    User {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"group","fqn":"domain.group.<name>","name":"...","attribute":"core","root":"...","body":"..."}`
    /// `Group` — the hierarchical domain container. `attribute` ∈
    /// core|supporting|generic; `root` is the optional aggregate-group root.
    Group {
        fqn: String,
        name: String,
        #[serde(default)]
        attribute: String,
        #[serde(default)]
        root: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"value","fqn":"domain.value.<name>","name":"...","body":"..."}`
    /// `Value` — immutable (was ValueObject).
    Value {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"service","fqn":"domain.service.<name>","name":"...","body":"..."}`
    /// `Service` — stateless behaviour (was DomainProcess).
    Service {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"person","fqn":"solution.person.<name>","name":"...","body":"..."}`
    /// `Person` — the C4 view of User/Stakeholder (solution).
    Person {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"constraint","fqn":"<layer>.constraint.<name>","name":"...","body":"...","attaches-to":"..."}`
    /// `Constraint` — declarative prose ("X must hold"); `attaches-to` is the
    /// optional FQN of the one tier-1–3 node a local constraint constrains
    /// (a global constraint carries none).
    Constraint {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
        #[serde(rename = "attaches-to", default)]
        attaches_to: String,
    },

    /// `{"type":"note","fqn":"<project>/note-<n>","body":"...","kind":"background"}`
    Note {
        fqn: String,
        body: String,
        #[serde(default)]
        kind: String,
    },

    /// `{"type":"feedback","fqn":"<project>/feedback-<n>","body":"...","status":"open","disposition":""}`
    Feedback {
        fqn: String,
        body: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        disposition: String,
    },

    /// `{"type":"plan","fqn":"<project>/plan","title":"...","strategy":"..."}`
    Plan {
        fqn: String,
        title: String,
        #[serde(default)]
        strategy: String,
    },

    /// `{"type":"plan_phase","fqn":"<project>/plan.phase-<n>","number":1,"title":"...","deliverable":"...","status":"pending|done"}`
    /// `status` is the durable phase milestone: `pending` until `apg plan
    /// complete` marks the phase `done` (PlanExecution-SPEC.md) — a completed
    /// phase is distinguishable from an uncompleted one whose tasks are all
    /// done and feedback resolved.
    PlanPhase {
        fqn: String,
        number: u32,
        title: String,
        #[serde(default)]
        deliverable: String,
        #[serde(default)]
        status: String,
    },

    /// `{"type":"task","fqn":"<project>/plan.phase-<n>.task-<k>","title":"...","kind":"source","tier":"","status":"pending"}`
    /// `kind` is the owning role (source/test/gate/docs); `tier`
    /// (unit/int/e2e) is the verification depth, meaningful only for
    /// `kind = test`.
    Task {
        fqn: String,
        title: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        tier: String,
        #[serde(default)]
        status: String,
    },

    Details {
        from: String,
        to: String,
    },
    Reviews {
        from: String,
        to: String,
    },
    DependsOn {
        from: String,
        to: String,
    },
    Gates {
        from: String,
        to: String,
    },
    Satisfies {
        from: String,
        to: String,
    },

    // --- Spine edges (GraphModel-SPEC.md; PHASE_01) ---
    // End-to-end traceability from why to code. The §3.3 matrix (apg-projects)
    // keeps `Drives` (Requirement → Group/Entity/Value/Service) and
    // `Represents` (User → Entity, Entity → Person).
    Drives {
        from: String,
        to: String,
    },
    Represents {
        from: String,
        to: String,
    },

    // --- New-model §3.3 spec edges (apg-projects). The kebab spellings are the
    // §3.3 wire names — `Record`'s `rename_all = "snake_case"` would give
    // `realised_by`/`implemented_by`, which is NOT the §3.3 spelling, so these
    // carry explicit renames. `contains`/`drives`/`represents`/`details`/
    // `calls`/`uses`/`depends-on` reuse the existing record variants and are
    // routed by endpoint node-kind at load time (`depends-on` is the existing
    // `DependsOn` Requirement→Requirement). ---
    /// `{"type":"realised-by","from":"domain.service.<name>","to":"solution.system.<name>"}`
    /// Domain Group/Entity/Service → Solution System/Container/Component.
    #[serde(rename = "realised-by")]
    RealisedBy {
        from: String,
        to: String,
    },
    /// `{"type":"implemented-by","from":"solution.component.<name>","to":"<code fqn>"}`
    /// Solution System/Container/Component → code FQN (validated vs the
    /// scanned graph by `layers::validate_code_refs`).
    #[serde(rename = "implemented-by")]
    SpecImplementedBy {
        from: String,
        to: String,
    },
    /// `{"type":"publishes","from":"domain.service.<name>","to":"domain.entity.<name>"}`
    /// Service → Entity (kind: event).
    Publishes {
        from: String,
        to: String,
    },
    /// `{"type":"subscribes","from":"domain.service.<name>","to":"domain.entity.<name>"}`
    /// Service → Entity (kind: event).
    Subscribes {
        from: String,
        to: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Record {
        serde_json::from_str(line).expect("fixture line must parse")
    }

    #[test]
    fn scan_meta_control_record_parses_and_serializes() {
        // The scan_meta control record as `apg scan` emits it (line 1 of
        // graph.jsonl): git fields present in a git repo...
        let r: Record = serde_json::from_str(
            r#"{"type":"scan_meta","git_sha":"abc123","git_clean":true,"scanned_at":"2026-09-07T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(
            matches!(r, Record::ScanMeta { ref git_sha, ref git_clean, ref scanned_at }
                if git_sha.as_deref() == Some("abc123")
                    && *git_clean == Some(true)
                    && scanned_at == "2026-09-07T00:00:00Z")
        );
        // ...and omitted when the scanned dir is not a git repo.
        let r: Record =
            serde_json::from_str(r#"{"type":"scan_meta","scanned_at":"2026-09-07T00:00:00Z"}"#)
                .unwrap();
        assert!(matches!(
            r,
            Record::ScanMeta {
                git_sha: None,
                git_clean: None,
                ..
            }
        ));
        // Serialization omits the absent git fields (round-trips the input).
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"type":"scan_meta","scanned_at":"2026-09-07T00:00:00Z"}"#
        );
    }

    #[test]
    fn spec_node_records_parse() {
        // Fixture lines in the unified-JSONL style of the SPEC serialization
        // section (canonical fqns, type-tagged).
        let lines = [
            r#"{"type":"requirement","fqn":"workitem-timer/spec.R1","id":"R1","title":"Timer","body":"A workitem can be started","feature":"feature-a"}"#,
            r#"{"type":"planned_node","fqn":"github.com/foundry/flow.Store","kind":"struct","name":"Store","parent":"github.com/foundry/flow"}"#,
            r#"{"type":"note","fqn":"workitem-timer/note-1","body":"Prose","kind":"background"}"#,
            r#"{"type":"feedback","fqn":"workitem-timer/feedback-1","body":"Split R1","status":"open"}"#,
            r#"{"type":"plan","fqn":"workitem-timer/plan","title":"Plan","strategy":"Layer-first"}"#,
            r#"{"type":"plan_phase","fqn":"workitem-timer/plan.phase-01","number":1,"title":"P1","deliverable":"Schema"}"#,
            r#"{"type":"task","fqn":"workitem-timer/plan.phase-01.task-1","title":"Add RootStore","kind":"source","tier":"","status":"pending"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        // Field extraction sanity checks.
        let r = parse(lines[0]);
        match r {
            Record::Requirement {
                fqn, id, feature, ..
            } => {
                assert_eq!(fqn, "workitem-timer/spec.R1");
                assert_eq!(id, "R1");
                assert_eq!(feature, "feature-a");
            }
            other => panic!("expected requirement, got {other:?}"),
        }
        let r = parse(lines[1]);
        match r {
            Record::PlannedNode {
                fqn,
                kind,
                name,
                parent,
            } => {
                assert_eq!(fqn, "github.com/foundry/flow.Store");
                assert_eq!(kind, "struct");
                assert_eq!(name, "Store");
                assert_eq!(parent, "github.com/foundry/flow");
            }
            other => panic!("expected planned_node, got {other:?}"),
        }
    }

    #[test]
    fn spec_edge_records_parse() {
        let lines = [
            r#"{"type":"contains","from":"foo/spec","to":"foo/spec.R1"}"#,
            r#"{"type":"details","from":"foo/note-1","to":"foo/spec"}"#,
            r#"{"type":"reviews","from":"foo/feedback-1","to":"foo/spec.R1"}"#,
            r#"{"type":"depends_on","from":"foo/spec.R2","to":"foo/spec.R1"}"#,
            r#"{"type":"gates","from":"foo/plan.phase-02","to":"foo/plan.phase-01"}"#,
            r#"{"type":"satisfies","from":"foo/plan.phase-01","to":"foo/spec.R1"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        assert!(
            matches!(parse(lines[4]), Record::Gates { from, to } if from == "foo/plan.phase-02" && to == "foo/plan.phase-01")
        );
        assert!(matches!(parse(lines[5]), Record::Satisfies { to, .. } if to == "foo/spec.R1"));
    }

    #[test]
    fn missing_optional_fields_default() {
        // Edge and optional-field records tolerate absent optional props.
        let r: Record =
            serde_json::from_str(r#"{"type":"unresolved_call","from":"x","to":"fmt.Errorf"}"#)
                .unwrap();
        assert!(
            matches!(r, Record::UnresolvedCall { ref target_type, .. } if target_type.is_empty())
        );
        let r: Record = serde_json::from_str(
            r#"{"type":"planned_node","fqn":"github.com/x/y.Store","kind":"struct"}"#,
        )
        .unwrap();
        assert!(matches!(
            r,
            Record::PlannedNode {
                ref name,
                ref parent,
                ..
            } if name.is_empty() && parent.is_empty()
        ));
        let r: Record =
            serde_json::from_str(r#"{"type":"requirement","fqn":"f","id":"R1","title":"t"}"#)
                .unwrap();
        assert!(matches!(r, Record::Requirement { ref feature, .. } if feature.is_empty()));
    }

    #[test]
    fn tier_1_2_3_node_records_parse() {
        // The surviving tier-1/2/3 records (the removed DDD/Aggregate/Domain*
        // vocabulary collapsed into the §3.1 catalog).
        let lines = [
            r#"{"type":"stakeholder","fqn":"foo/stakeholder.Ops","name":"Ops","body":"runs it"}"#,
            r#"{"type":"entity","fqn":"foo/entity.User","name":"User","body":"an identity"}"#,
            r#"{"type":"system","fqn":"foo/system.Platform","name":"Platform","body":"..."}"#,
            r#"{"type":"container","fqn":"foo/container.Api","name":"Api","kind":"app","body":"..."}"#,
            r#"{"type":"component","fqn":"foo/component.Gateway","name":"Gateway","body":"..."}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        match parse(lines[1]) {
            Record::Entity { fqn, name, body } => {
                assert_eq!(fqn, "foo/entity.User");
                assert_eq!(name, "User");
                assert_eq!(body, "an identity");
            }
            other => panic!("expected entity, got {other:?}"),
        }
        match parse(lines[3]) {
            Record::Container { kind, .. } => assert_eq!(kind, "app"),
            other => panic!("expected container, got {other:?}"),
        }
    }

    #[test]
    fn spine_edge_records_parse() {
        let lines = [
            r#"{"type":"drives","from":"foo/spec.R1","to":"domain.group.sales"}"#,
            r#"{"type":"represents","from":"requirements.user.u1","to":"domain.entity.e1"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        assert!(matches!(parse(lines[0]), Record::Drives { from, to }
                if from == "foo/spec.R1" && to == "domain.group.sales"));
        assert!(matches!(parse(lines[1]), Record::Represents { from, to }
                if from == "requirements.user.u1" && to == "domain.entity.e1"));
    }

    #[test]
    fn new_model_node_records_parse() {
        // The §3.1 catalog's new node records (apg-projects) parse with
        // `<layer>.<type>.<name>` FQNs and their §3.1 attributes.
        let lines = [
            r#"{"type":"user","fqn":"requirements.user.customer","name":"customer","body":"uses it"}"#,
            r#"{"type":"group","fqn":"domain.group.sales","name":"sales","attribute":"core","root":"sales-root","body":"..."}"#,
            r#"{"type":"value","fqn":"domain.value.money","name":"money","body":"..."}"#,
            r#"{"type":"service","fqn":"domain.service.checkout","name":"checkout","body":"..."}"#,
            r#"{"type":"person","fqn":"solution.person.alice","name":"alice","body":"..."}"#,
            r#"{"type":"constraint","fqn":"domain.constraint.law","name":"law","body":"X must hold","attaches-to":"domain.entity.customer"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        match parse(lines[1]) {
            Record::Group {
                fqn,
                name,
                attribute,
                root,
                ..
            } => {
                assert_eq!(fqn, "domain.group.sales");
                assert_eq!(name, "sales");
                assert_eq!(attribute, "core");
                assert_eq!(root, "sales-root");
            }
            other => panic!("expected group, got {other:?}"),
        }
        match parse(lines[5]) {
            Record::Constraint {
                fqn, attaches_to, ..
            } => {
                assert_eq!(fqn, "domain.constraint.law");
                assert_eq!(attaches_to, "domain.entity.customer");
            }
            other => panic!("expected constraint, got {other:?}"),
        }
        // Optional attributes default when absent.
        let g = parse(r#"{"type":"group","fqn":"domain.group.sales","name":"sales"}"#);
        assert!(
            matches!(g, Record::Group { ref attribute, ref root, ref body, .. }
                if attribute.is_empty() && root.is_empty() && body.is_empty())
        );
        let c = parse(r#"{"type":"constraint","fqn":"global.constraint.law","name":"law"}"#);
        assert!(matches!(c, Record::Constraint { ref attaches_to, .. } if attaches_to.is_empty()));
    }

    #[test]
    fn new_model_edge_records_use_kebab_wire_names() {
        // The §3.3 matrix edges that are NEW verbs serialize with kebab wire
        // names, NOT the snake_case `rename_all` spelling. `depends-on` is not
        // here — it reuses the existing `DependsOn` (Requirement→Requirement).
        let lines = [
            r#"{"type":"realised-by","from":"domain.group.sales","to":"solution.system.payments"}"#,
            r#"{"type":"implemented-by","from":"solution.component.checkout","to":"apg.layers.ingest_tree"}"#,
            r#"{"type":"publishes","from":"domain.service.orders","to":"domain.entity.order-placed"}"#,
            r#"{"type":"subscribes","from":"domain.service.shipping","to":"domain.entity.order-placed"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        assert!(
            matches!(parse(lines[0]), Record::RealisedBy { ref from, ref to }
                if from == "domain.group.sales" && to == "solution.system.payments")
        );
        assert!(
            matches!(parse(lines[1]), Record::SpecImplementedBy { ref to, .. }
                if to == "apg.layers.ingest_tree")
        );
        assert!(
            matches!(parse(lines[2]), Record::Publishes { ref from, ref to }
                if from == "domain.service.orders" && to == "domain.entity.order-placed")
        );
        assert!(matches!(parse(lines[3]), Record::Subscribes { ref to, .. }
                if to == "domain.entity.order-placed"));
        // Serialization round-trips the kebab wire name (not snake_case).
        let s = serde_json::to_string(&parse(lines[0])).unwrap();
        assert!(s.contains(r#""type":"realised-by""#), "{s}");
        assert!(!s.contains("realised_by"), "{s}");
        let s = serde_json::to_string(&parse(lines[1])).unwrap();
        assert!(s.contains(r#""type":"implemented-by""#), "{s}");
        assert!(!s.contains("implemented_by"), "{s}");
    }
}
