use super::*;

fn parse(line: &str) -> Record {
    serde_json::from_str(line).expect("fixture line must parse")
}

/// unit tier -- pure in-memory: no filesystem, database, git or process.
/// (This module has no e2e test: every test is serde parse/serialize over
/// string literals, so no `e2e` submodule exists.)
mod unit {
    use super::*;

    #[test]
    fn scan_meta_control_record_parses_and_serializes() {
        // The scan_meta control record as `apg scan` emits it (line 1 of
        // graph.jsonl): git fields present in a git repo...
        let r: Record = serde_json::from_str(
        r#"{"type":"scan_meta","git_sha":"abc123","git_clean":true,"content_key":"deadbeef","scanned_at":"2026-09-07T00:00:00Z"}"#,
    )
    .unwrap();
        assert!(
            matches!(r, Record::ScanMeta { ref git_sha, ref git_clean, ref content_key, ref scanned_at }
            if git_sha.as_deref() == Some("abc123")
                && *git_clean == Some(true)
                && content_key.as_deref() == Some("deadbeef")
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
                content_key: None,
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
