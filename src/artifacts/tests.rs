use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;
    use crate::layers::NodeProperties;

    /// The authored record kinds the write-through re-merge used to drop through
    /// `_ => None`: `uses` (Person→Component), `calls` (Service→Service) and
    /// `applies-to` (implementation constraint → scan Language identity) all map
    /// to their rel tables, and the live DB's `AppliesTo` row reconstructs to
    /// the same record. The merge materialises the Language row a target needs
    /// when the record set/DB holds none (a language id no scanned stream
    /// produced, say `rust` in a Go-only repo), so the pair lands instead of
    /// being skipped as a dangling endpoint.
    #[test]
    fn edge_merge_maps_authored_uses_and_calls() {
        assert_eq!(
            edge_merge(&Record::Uses {
                from: "solution.person.alice".into(),
                to: "solution.component.portal".into(),
                properties: NodeProperties::default(),
            }),
            Some((
                "Uses",
                "solution.person.alice",
                "solution.component.portal",
                Some(&NodeProperties::default())
            ))
        );
        assert_eq!(
            edge_merge(&Record::Calls {
                from: "domain.service.a".into(),
                to: "domain.service.b".into(),
                properties: NodeProperties::default(),
            }),
            Some((
                "Calls",
                "domain.service.a",
                "domain.service.b",
                Some(&NodeProperties::default())
            ))
        );

        // applies-to: implementation constraint → the scan Language identity
        // `rust`, mapped to the `AppliesTo` rel table.
        let props = NodeProperties::from([("note".to_string(), "tier-4".to_string())]);
        let applies = Record::AppliesTo {
            from: "implementation.constraint.law".into(),
            to: "rust".into(),
            properties: props.clone(),
        };
        assert_eq!(
            edge_merge(&applies),
            Some((
                "AppliesTo",
                "implementation.constraint.law",
                "rust",
                Some(&props)
            ))
        );
        assert!(rel_pair_allowed("AppliesTo", "Constraint", "Language"));
        // The live DB's AppliesTo row reconstructs to the same record.
        assert_eq!(
            crate::artifacts::db::durable_edge_record(
                "AppliesTo",
                "implementation.constraint.law".into(),
                "rust".into(),
                props,
            ),
            applies
        );

        // The merge materialises the target row before the edge: with no
        // `Language` row labelled by the record set, `rust` is the row to
        // create (`MERGE (n:Language {fqn})`), so `merge_edge` cannot skip the
        // pair as a dangling endpoint.
        let unlabelled = std::collections::HashMap::new();
        assert_eq!(
            crate::artifacts::merge::applies_to_missing_languages(
                std::slice::from_ref(&applies),
                &unlabelled
            ),
            vec!["rust"]
        );
        // A target the record set already labels needs no row.
        let labelled = std::collections::HashMap::from([("rust".to_string(), "Language")]);
        assert!(
            crate::artifacts::merge::applies_to_missing_languages(&[applies], &labelled).is_empty()
        );
    }

    /// The merge guard admits the two authored-only pairs through
    /// [`load::rel_table_pairs`] — the guard's only consumer.
    #[test]
    fn rel_pair_allowed_admits_authored_uses_and_calls() {
        assert!(rel_pair_allowed("Uses", "Person", "Component"));
        assert!(rel_pair_allowed("Calls", "Service", "Service"));
        assert!(rel_pair_allowed("Calls", "Actor", "Service"));
    }

    /// `remove_node` strips incident authored `uses`/`calls` edges too — they
    /// were previously left behind because `edge_endpoints` did not recognize
    /// the two record kinds.
    #[test]
    fn remove_node_strips_incident_uses_and_calls_edges() {
        let mut records = vec![
            Record::Person {
                fqn: "solution.person.alice".into(),
                name: "alice".into(),
                body: String::new(),
                properties: NodeProperties::default(),
            },
            Record::Component {
                fqn: "solution.component.portal".into(),
                name: "portal".into(),
                body: String::new(),
                properties: NodeProperties::default(),
            },
            Record::Uses {
                from: "solution.person.alice".into(),
                to: "solution.component.portal".into(),
                properties: NodeProperties::default(),
            },
            Record::Service {
                fqn: "domain.service.a".into(),
                name: "a".into(),
                body: String::new(),
                properties: NodeProperties::default(),
            },
            Record::Service {
                fqn: "domain.service.b".into(),
                name: "b".into(),
                body: String::new(),
                properties: NodeProperties::default(),
            },
            Record::Calls {
                from: "domain.service.a".into(),
                to: "domain.service.b".into(),
                properties: NodeProperties::default(),
            },
            // An unrelated node + edge that must survive both removals.
            Record::Note {
                fqn: "foo/note-1".into(),
                body: "background".into(),
                kind: "background".into(),
                properties: NodeProperties::default(),
            },
            Record::Details {
                from: "foo/note-1".into(),
                to: "solution.component.portal".into(),
                properties: NodeProperties::default(),
            },
        ];

        remove_node(&mut records, "solution.person.alice");
        remove_node(&mut records, "domain.service.a");

        let has_node = |fqn: &str| records.iter().any(|r| node_fqn(r) == Some(fqn));
        let has_edge = |from: &str, to: &str| {
            records
                .iter()
                .any(|r| edge_endpoints(r) == Some((from, to)))
        };
        assert!(!has_node("solution.person.alice"), "person must be removed");
        assert!(!has_node("domain.service.a"), "service must be removed");
        assert!(
            !has_edge("solution.person.alice", "solution.component.portal"),
            "the incident Uses edge must be removed with the person"
        );
        assert!(
            !has_edge("domain.service.a", "domain.service.b"),
            "the incident Calls edge must be removed with the service"
        );
        assert!(has_node("solution.component.portal"));
        assert!(has_node("domain.service.b"));
        assert!(has_edge("foo/note-1", "solution.component.portal"));
    }
}
