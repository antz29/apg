use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// The two record kinds the write-through re-merge used to drop through
    /// `_ => None`: authored `uses` (Person→System) and `calls`
    /// (Service→Service) map to their rel tables.
    #[test]
    fn edge_merge_maps_authored_uses_and_calls() {
        assert_eq!(
            edge_merge(&Record::Uses {
                from: "solution.person.alice".into(),
                to: "solution.system.portal".into(),
            }),
            Some(("Uses", "solution.person.alice", "solution.system.portal"))
        );
        assert_eq!(
            edge_merge(&Record::Calls {
                from: "domain.service.a".into(),
                to: "domain.service.b".into(),
            }),
            Some(("Calls", "domain.service.a", "domain.service.b"))
        );
    }

    /// The merge guard admits the two authored-only pairs through
    /// [`load::rel_table_pairs`] — the guard's only consumer.
    #[test]
    fn rel_pair_allowed_admits_authored_uses_and_calls() {
        assert!(rel_pair_allowed("Uses", "Person", "System"));
        assert!(rel_pair_allowed("Calls", "Service", "Service"));
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
            },
            Record::System {
                fqn: "solution.system.portal".into(),
                name: "portal".into(),
                body: String::new(),
            },
            Record::Uses {
                from: "solution.person.alice".into(),
                to: "solution.system.portal".into(),
            },
            Record::Service {
                fqn: "domain.service.a".into(),
                name: "a".into(),
                body: String::new(),
            },
            Record::Service {
                fqn: "domain.service.b".into(),
                name: "b".into(),
                body: String::new(),
            },
            Record::Calls {
                from: "domain.service.a".into(),
                to: "domain.service.b".into(),
            },
            // An unrelated node + edge that must survive both removals.
            Record::Note {
                fqn: "foo/note-1".into(),
                body: "background".into(),
                kind: "background".into(),
            },
            Record::Details {
                from: "foo/note-1".into(),
                to: "solution.system.portal".into(),
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
            !has_edge("solution.person.alice", "solution.system.portal"),
            "the incident Uses edge must be removed with the person"
        );
        assert!(
            !has_edge("domain.service.a", "domain.service.b"),
            "the incident Calls edge must be removed with the service"
        );
        assert!(has_node("solution.system.portal"));
        assert!(has_node("domain.service.b"));
        assert!(has_edge("foo/note-1", "solution.system.portal"));
    }
}
