//! Deserialization of the unified scanner JSONL schema (SPEC §2).
//!
//! One JSON object per line; the `type` field discriminates the record. Node
//! records carry scanner-local opaque `id`s (for `struct`/`function`) or a
//! verbatim `fqn` (`module`/`unresolved`). Edge records reference endpoints by
//! `id` (project node) or `fqn` (unresolved target).

use serde::{Deserialize, Serialize};

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

    /// `{"type":"spec","fqn":"future/<project>/spec","title":"...","goal":"..."}`
    Spec {
        fqn: String,
        title: String,
        #[serde(default)]
        goal: String,
    },

    /// `{"type":"requirement","fqn":"future/<project>/spec.<id>","id":"R1","title":"...","body":"...","feature":"..."}`
    Requirement {
        fqn: String,
        id: String,
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        feature: String,
    },

    /// `{"type":"phase","fqn":"future/<project>/spec.phase-<n>","number":1,"title":"..."}`
    Phase {
        fqn: String,
        number: u32,
        title: String,
    },

    /// `{"type":"decision","fqn":"future/<project>/spec.decision-<id>","id":"...","summary":"..."}`
    Decision {
        fqn: String,
        id: String,
        summary: String,
    },

    /// `{"type":"future","fqn":"future/<project>/<name>","kind":"function","target":"..."}`
    Future {
        fqn: String,
        kind: String,
        #[serde(default)]
        target: String,
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

    /// `{"type":"note","fqn":"future/<project>/note-<n>","body":"...","kind":"background"}`
    Note {
        fqn: String,
        body: String,
        #[serde(default)]
        kind: String,
    },

    /// `{"type":"feedback","fqn":"future/<project>/feedback-<n>","body":"...","status":"open","disposition":""}`
    Feedback {
        fqn: String,
        body: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        disposition: String,
    },

    /// `{"type":"plan","fqn":"future/<project>/plan","title":"...","strategy":"..."}`
    Plan {
        fqn: String,
        title: String,
        #[serde(default)]
        strategy: String,
    },

    /// `{"type":"plan_phase","fqn":"future/<project>/plan.phase-<n>","number":1,"title":"...","deliverable":"..."}`
    PlanPhase {
        fqn: String,
        number: u32,
        title: String,
        #[serde(default)]
        deliverable: String,
    },

    /// `{"type":"task","fqn":"future/<project>/plan.phase-<n>.task-<k>","title":"...","kind":"source","tier":"","status":"pending"}`
    /// `kind` is the owning role (source/test/gate/docs/human); `tier`
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Record {
        serde_json::from_str(line).expect("fixture line must parse")
    }

    #[test]
    fn spec_node_records_parse() {
        // Fixture lines in the unified-JSONL style of the SPEC serialization
        // section (canonical fqns, type-tagged).
        let lines = [
            r#"{"type":"spec","fqn":"future/workitem-timer/spec","title":"Workitem Timer","goal":"Let workitems time out"}"#,
            r#"{"type":"requirement","fqn":"future/workitem-timer/spec.R1","id":"R1","title":"Timer","body":"A workitem can be started","feature":"feature-a"}"#,
            r#"{"type":"phase","fqn":"future/workitem-timer/spec.phase-1","number":1,"title":"Core"}"#,
            r#"{"type":"decision","fqn":"future/workitem-timer/spec.decision-d1","id":"d1","summary":"Wall-clock"}"#,
            r#"{"type":"future","fqn":"future/workitem-timer/gateway","kind":"rpc","target":"github.com/foundry/flow.Gateway"}"#,
            r#"{"type":"non_goal","fqn":"future/workitem-timer/spec.ng1","body":"No daemon"}"#,
            r#"{"type":"acceptance_criterion","fqn":"future/workitem-timer/spec.ac1","body":"Fires once"}"#,
            r#"{"type":"verification_item","fqn":"future/workitem-timer/spec.vi1","body":"cargo test green"}"#,
            r#"{"type":"note","fqn":"future/workitem-timer/note-1","body":"Prose","kind":"background"}"#,
            r#"{"type":"feedback","fqn":"future/workitem-timer/feedback-1","body":"Split R1","status":"open"}"#,
            r#"{"type":"plan","fqn":"future/workitem-timer/plan","title":"Plan","strategy":"Layer-first"}"#,
            r#"{"type":"plan_phase","fqn":"future/workitem-timer/plan.phase-01","number":1,"title":"P1","deliverable":"Schema"}"#,
            r#"{"type":"task","fqn":"future/workitem-timer/plan.phase-01.task-1","title":"Add RootStore","kind":"source","tier":"","status":"pending"}"#,
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
                assert_eq!(fqn, "future/workitem-timer/spec.R1");
                assert_eq!(id, "R1");
                assert_eq!(feature, "feature-a");
            }
            other => panic!("expected requirement, got {other:?}"),
        }
        let r = parse(lines[4]);
        match r {
            Record::Future { fqn, kind, target } => {
                assert_eq!(fqn, "future/workitem-timer/gateway");
                assert_eq!(kind, "rpc");
                assert_eq!(target, "github.com/foundry/flow.Gateway");
            }
            other => panic!("expected future, got {other:?}"),
        }
    }

    #[test]
    fn spec_edge_records_parse() {
        let lines = [
            r#"{"type":"contains","from":"future/foo/spec","to":"future/foo/spec.R1"}"#,
            r#"{"type":"details","from":"future/foo/note-1","to":"future/foo/spec"}"#,
            r#"{"type":"reviews","from":"future/foo/feedback-1","to":"future/foo/spec.R1"}"#,
            r#"{"type":"depends_on","from":"future/foo/spec.R2","to":"future/foo/spec.R1"}"#,
            r#"{"type":"gates","from":"future/foo/spec.phase-2","to":"future/foo/spec.phase-1"}"#,
            r#"{"type":"spec_depends","from":"future/foo/spec","to":"future/bar/spec"}"#,
            r#"{"type":"anchors","from":"future/foo/spec.R1","to":"github.com/x/impl"}"#,
            r#"{"type":"implements","from":"github.com/x/impl","to":"future/foo/spec.R1"}"#,
            r#"{"type":"satisfies","from":"future/foo/plan.phase-01","to":"future/foo/spec.R1"}"#,
            r#"{"type":"builds","from":"future/foo/plan.phase-01.task-1","to":"future/foo/gateway"}"#,
        ];
        for l in lines {
            let _ = parse(l);
        }
        assert!(
            matches!(parse(lines[4]), Record::Gates { from, to } if from == "future/foo/spec.phase-2" && to == "future/foo/spec.phase-1")
        );
        assert!(
            matches!(parse(lines[9]), Record::Builds { to, .. } if to == "future/foo/gateway")
        );
    }

    #[test]
    fn missing_optional_fields_default() {
        // Edge and optional-field records tolerate absent optional props.
        let r: Record = serde_json::from_str(
            r#"{"type":"unresolved_call","from":"x","to":"fmt.Errorf"}"#,
        )
        .unwrap();
        assert!(matches!(r, Record::UnresolvedCall { ref target_type, .. } if target_type.is_empty()));
        let r: Record = serde_json::from_str(
            r#"{"type":"future","fqn":"future/foo/g","kind":"function"}"#,
        )
        .unwrap();
        assert!(matches!(r, Record::Future { ref target, .. } if target.is_empty()));
        let r: Record = serde_json::from_str(r#"{"type":"requirement","fqn":"f","id":"R1","title":"t"}"#)
            .unwrap();
        assert!(matches!(r, Record::Requirement { ref feature, .. } if feature.is_empty()));
    }
}
