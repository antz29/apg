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
    /// `{"type":"spec","fqn":"<project>/spec","title":"...","goal":"..."}`
    Spec {
        fqn: String,
        title: String,
        #[serde(default)]
        goal: String,
    },

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

    /// `{"type":"phase","fqn":"<project>/spec.phase-<n>","number":1,"title":"..."}`
    Phase {
        fqn: String,
        number: u32,
        title: String,
    },

    /// `{"type":"decision","fqn":"<project>/spec.decision-<id>","id":"...","summary":"..."}`
    Decision {
        fqn: String,
        id: String,
        summary: String,
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

    NonGoal {
        fqn: String,
        body: String,
    },
    AcceptanceCriterion {
        fqn: String,
        body: String,
    },
    VerificationItem {
        fqn: String,
        body: String,
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

    /// `{"type":"domain","fqn":"<project>/domain.<name>","name":"...","body":"..."}`
    /// The domain area (a bounded context); `bounded-context` is an authoring
    /// alias that produces a Domain node.
    Domain {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"subdomain","fqn":"<project>/subdomain.<name>","name":"...","kind":"core","body":"..."}`
    /// `kind` ∈ core|supporting|generic (the DDD subdomain partitioning).
    Subdomain {
        fqn: String,
        name: String,
        #[serde(default)]
        kind: String,
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

    /// `{"type":"value_object","fqn":"<project>/value-object.<name>","name":"...","body":"..."}`
    ValueObject {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"aggregate","fqn":"<project>/aggregate.<name>","name":"...","root":"Order","body":"..."}`
    /// `root` names the aggregate root entity.
    Aggregate {
        fqn: String,
        name: String,
        #[serde(default)]
        root: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"domain_event","fqn":"<project>/domain-event.<name>","name":"...","body":"..."}`
    DomainEvent {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"domain_process","fqn":"<project>/domain-process.<name>","name":"...","body":"..."}`
    DomainProcess {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"domain_rule","fqn":"<project>/domain-rule.<name>","name":"...","body":"..."}`
    /// The invariant mechanism at the domain tier (Invariants-SPEC.md).
    DomainRule {
        fqn: String,
        name: String,
        #[serde(default)]
        body: String,
    },

    /// `{"type":"actor","fqn":"<project>/actor.<name>","name":"...","body":"..."}`
    Actor {
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

    // --- New-model tier catalog (apg-projects SPEC §3.1). ADD-ONLY: the old
    // variants stay until task-18 removes them. FQNs are now
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

    /// `{"type":"invariant","fqn":"invariant/<name>","title":"...","body":"...","category":"process","scope":"spec","status":"active"}`
    /// Graph-wide rules artifacts must respect (Invariants-SPEC.md). Roots:
    /// `invariant/<name>` for universal rules, `<project>/invariant/<name>`
    /// for repo/project-specific ones. `category` ∈ process|product|
    /// graph-integrity; `status` ∈ active|retired.
    Invariant {
        fqn: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        category: String,
        #[serde(default)]
        scope: String,
        #[serde(default)]
        status: String,
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
    SpecDepends {
        from: String,
        to: String,
    },
    Anchors {
        from: String,
        to: String,
    },
    Implements {
        from: String,
        to: String,
    },
    Satisfies {
        from: String,
        to: String,
    },
    Builds {
        from: String,
        to: String,
    },

    // --- Spine edges (GraphModel-SPEC.md; PHASE_01) ---
    // End-to-end traceability from why to code:
    //   Requirement --Drives/Requires--> Domain --Realises/Represents--> Solution
    //   --ImplementedBy--> Implementation (code).
    // `Drives`/`Requires` are Requirement → Domain; `Realises`/`Represents`
    // Domain → Solution; `ImplementedBy` Solution → Implementation.
    Drives {
        from: String,
        to: String,
    },
    Requires {
        from: String,
        to: String,
    },
    Realises {
        from: String,
        to: String,
    },
    Represents {
        from: String,
        to: String,
    },
    ImplementedBy {
        from: String,
        to: String,
    },
    /// `GuardedBy` (artifact → Invariant) — an artifact (Spec, Plan,
    /// Requirement, Task, or a code/domain node) is guarded by the invariants
    /// it must respect (Invariants-SPEC.md).
    GuardedBy {
        from: String,
        to: String,
    },
    /// `Checks` (Feedback → Invariant) — a review comment cites the rule it
    /// enforces (optional; most feedback is not an invariant violation).
    Checks {
        from: String,
        to: String,
    },

    // --- New-model §3.3 spec edges (apg-projects). ADD-ONLY: the old spine
    // edges (`Realises`, `ImplementedBy`) stay until task-18. The kebab
    // spellings are the §3.3 wire names — `Record`'s `rename_all =
    // "snake_case"` would give `realised_by`/`implemented_by`, which is NOT
    // the §3.3 spelling, so these carry explicit renames. `contains`/`drives`/
    // `represents`/`details`/`calls`/`uses`/`depends-on` reuse the existing
    // record variants and are routed by endpoint node-kind at load time
    // (`depends-on` is the existing `DependsOn` Requirement→Requirement). ---
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
            r#"{"type":"spec","fqn":"workitem-timer/spec","title":"Workitem Timer","goal":"Let workitems time out"}"#,
            r#"{"type":"requirement","fqn":"workitem-timer/spec.R1","id":"R1","title":"Timer","body":"A workitem can be started","feature":"feature-a"}"#,
            r#"{"type":"phase","fqn":"workitem-timer/spec.phase-1","number":1,"title":"Core"}"#,
            r#"{"type":"decision","fqn":"workitem-timer/spec.decision-d1","id":"d1","summary":"Wall-clock"}"#,
            r#"{"type":"planned_node","fqn":"github.com/foundry/flow.Store","kind":"struct","name":"Store","parent":"github.com/foundry/flow"}"#,
            r#"{"type":"non_goal","fqn":"workitem-timer/spec.ng1","body":"No daemon"}"#,
            r#"{"type":"acceptance_criterion","fqn":"workitem-timer/spec.ac1","body":"Fires once"}"#,
            r#"{"type":"verification_item","fqn":"workitem-timer/spec.vi1","body":"cargo test green"}"#,
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
        let r = parse(lines[1]);
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
        let r = parse(lines[4]);
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
            r#"{"type":"gates","from":"foo/spec.phase-2","to":"foo/spec.phase-1"}"#,
            r#"{"type":"spec_depends","from":"foo/spec","to":"bar/spec"}"#,
            r#"{"type":"anchors","from":"foo/spec.R1","to":"github.com/x/impl"}"#,
            r#"{"type":"implements","from":"github.com/x/impl","to":"foo/spec.R1"}"#,
            r#"{"type":"satisfies","from":"foo/plan.phase-01","to":"foo/spec.R1"}"#,
            r#"{"type":"builds","from":"foo/plan.phase-01.task-1","to":"github.com/x/y.Store"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        assert!(
            matches!(parse(lines[4]), Record::Gates { from, to } if from == "foo/spec.phase-2" && to == "foo/spec.phase-1")
        );
        assert!(
            matches!(parse(lines[9]), Record::Builds { to, .. } if to == "github.com/x/y.Store")
        );
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
        let lines = [
            r#"{"type":"stakeholder","fqn":"foo/stakeholder.Ops","name":"Ops","body":"runs it"}"#,
            r#"{"type":"domain","fqn":"foo/domain.Auth","name":"Auth","body":"the auth area"}"#,
            r#"{"type":"subdomain","fqn":"foo/subdomain.Access","name":"Access","kind":"core","body":"..."}"#,
            r#"{"type":"entity","fqn":"foo/entity.User","name":"User","body":"an identity"}"#,
            r#"{"type":"value_object","fqn":"foo/value-object.Email","name":"Email","body":"..."}"#,
            r#"{"type":"aggregate","fqn":"foo/aggregate.Order","name":"Order","root":"Order","body":"..."}"#,
            r#"{"type":"domain_event","fqn":"foo/domain-event.UserLoggedIn","name":"UserLoggedIn","body":"..."}"#,
            r#"{"type":"domain_process","fqn":"foo/domain-process.Checkout","name":"Checkout","body":"..."}"#,
            r#"{"type":"domain_rule","fqn":"foo/domain-rule.NoNegativeBalance","name":"NoNegativeBalance","body":"..."}"#,
            r#"{"type":"actor","fqn":"foo/actor.Customer","name":"Customer","body":"..."}"#,
            r#"{"type":"system","fqn":"foo/system.Platform","name":"Platform","body":"..."}"#,
            r#"{"type":"container","fqn":"foo/container.Api","name":"Api","kind":"app","body":"..."}"#,
            r#"{"type":"component","fqn":"foo/component.Gateway","name":"Gateway","body":"..."}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        match parse(lines[1]) {
            Record::Domain { fqn, name, body } => {
                assert_eq!(fqn, "foo/domain.Auth");
                assert_eq!(name, "Auth");
                assert_eq!(body, "the auth area");
            }
            other => panic!("expected domain, got {other:?}"),
        }
        match parse(lines[2]) {
            Record::Subdomain { kind, .. } => assert_eq!(kind, "core"),
            other => panic!("expected subdomain, got {other:?}"),
        }
        match parse(lines[5]) {
            Record::Aggregate { root, .. } => assert_eq!(root, "Order"),
            other => panic!("expected aggregate, got {other:?}"),
        }
    }

    #[test]
    fn spine_edge_records_parse() {
        let lines = [
            r#"{"type":"drives","from":"foo/spec.R1","to":"foo/domain.Auth"}"#,
            r#"{"type":"requires","from":"foo/spec.R2","to":"foo/domain.Auth"}"#,
            r#"{"type":"realises","from":"foo/domain.Auth","to":"foo/system.Platform"}"#,
            r#"{"type":"represents","from":"foo/domain.Auth","to":"foo/container.Api"}"#,
            r#"{"type":"implemented_by","from":"foo/component.Gateway","to":"github.com/x/impl.Gateway"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        assert!(matches!(parse(lines[0]), Record::Drives { from, to }
                if from == "foo/spec.R1" && to == "foo/domain.Auth"));
        assert!(matches!(parse(lines[4]), Record::ImplementedBy { from, to }
                if from == "foo/component.Gateway" && to == "github.com/x/impl.Gateway"));
    }

    #[test]
    fn invariant_and_guard_checks_records_parse() {
        let i = parse(
            r#"{"type":"invariant","fqn":"invariant/plan.task-kind-in-set","title":"Task kind","body":"Every task carries one kind","category":"process","scope":"plan","status":"active"}"#,
        );
        match i {
            Record::Invariant {
                fqn,
                title,
                category,
                status,
                ..
            } => {
                assert_eq!(fqn, "invariant/plan.task-kind-in-set");
                assert_eq!(title, "Task kind");
                assert_eq!(category, "process");
                assert_eq!(status, "active");
            }
            other => panic!("expected invariant, got {other:?}"),
        }
        // A project-scoped invariant (a domain rule materialized as an
        // Invariant with category=product).
        let i = parse(
            r#"{"type":"invariant","fqn":"foo/invariant/NoNegativeBalance","title":"No negative balance","category":"product","scope":"code","status":"active"}"#,
        );
        assert!(matches!(i, Record::Invariant { ref fqn, ref category, .. }
                if fqn == "foo/invariant/NoNegativeBalance" && category == "product"));
        // GuardedBy and Checks edges.
        let g = parse(
            r#"{"type":"guarded_by","from":"foo/spec","to":"invariant/plan.task-kind-in-set"}"#,
        );
        assert!(matches!(g, Record::GuardedBy { from, to }
                if from == "foo/spec" && to == "invariant/plan.task-kind-in-set"));
        let c = parse(
            r#"{"type":"checks","from":"foo/feedback-1","to":"invariant/plan.task-kind-in-set"}"#,
        );
        assert!(matches!(c, Record::Checks { from, to }
                if from == "foo/feedback-1" && to == "invariant/plan.task-kind-in-set"));
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
