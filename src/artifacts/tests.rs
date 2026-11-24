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

    /// [`pending_state_present`] walks exactly the durable node-table roster in
    /// its declared order, with the pending-removal ledger as its other input:
    /// a fake row source records every label the rule asks about, so a table
    /// added to (or dropped from) that roster fails this test loudly instead of
    /// silently escaping the predicate. The roster is written out here because
    /// `DURABLE_NODE_TABLES` is private to `db`.
    #[test]
    fn pending_state_rule_reads_the_durable_roster_and_the_ledger() {
        const ROSTER: [&str; 15] = [
            "Requirement",
            "Stakeholder",
            "User",
            "Note",
            "Constraint",
            "DomainGroup",
            "Entity",
            "Value",
            "Service",
            "System",
            "Container",
            "Component",
            "Person",
            "Actor",
            "Role",
        ];

        // All-clean rows + empty ledger: not pending, and the walk visits the
        // whole roster in declared order.
        let mut visited = Vec::new();
        assert!(!pending_state_present(
            |table| {
                visited.push(table.to_string());
                false
            },
            &[],
        ));
        assert_eq!(visited, ROSTER.map(String::from));

        // One dirty table + empty ledger: pending, and the walk stops at the
        // first dirty label.
        let mut visited = Vec::new();
        assert!(pending_state_present(
            |table| {
                visited.push(table.to_string());
                table == "Entity"
            },
            &[],
        ));
        assert_eq!(visited.len(), 7);
        assert_eq!(visited.last().map(String::as_str), Some("Entity"));

        // All-clean rows + one ledger identity: pending, because the ledger is
        // consulted before any table.
        let mut visited = Vec::new();
        assert!(pending_state_present(
            |table| {
                visited.push(table.to_string());
                false
            },
            &[PendingRemoval {
                fqn: "requirements.requirement.r".into(),
            }],
        ));
        assert!(
            visited.is_empty(),
            "the ledger alone is pending state, without a table probe"
        );
    }

    /// With every override absent, [`apply_db_overrides`] returns the base
    /// config untouched — including a non-default `read_only` and every other
    /// field the caller set. `SystemConfig` has no getters and no `PartialEq`,
    /// so the comparison is its derived `Debug` rendering (which prints each
    /// field, `read_only` and `max_db_size` among them).
    #[test]
    fn absent_overrides_return_the_base_config_unchanged() {
        let base = lbug::SystemConfig::default()
            .read_only(true)
            .max_db_size(1 << 30)
            .buffer_pool_size(64 << 20)
            .max_num_threads(3);
        let returned = apply_db_overrides(base.clone(), None, None, None).unwrap();
        assert_eq!(format!("{returned:?}"), format!("{base:?}"));
        assert!(format!("{returned:?}").contains("read_only: true"));
    }

    /// A `1 GiB` / `128 MiB` / `2`-thread override folds into the base config:
    /// the inputs arrive as values (`"1G"`, the pool byte count, the thread
    /// count), never through the environment.
    #[test]
    fn override_applies_max_size_pool_and_threads() {
        let base = lbug::SystemConfig::default().read_only(false);
        let returned =
            apply_db_overrides(base, Some("1G"), Some(128 * 1024 * 1024), Some(2)).unwrap();
        let expected = lbug::SystemConfig::default()
            .read_only(false)
            .max_db_size(1 << 30)
            .buffer_pool_size(128 << 20)
            .max_num_threads(2);
        assert_eq!(format!("{returned:?}"), format!("{expected:?}"));
        let debug = format!("{returned:?}");
        assert!(debug.contains("max_db_size: 1073741824"));
        assert!(debug.contains("buffer_pool_size: 134217728"));
        assert!(debug.contains("max_num_threads: 2"));
    }

    /// The caller's `read_only` flag survives the override fold in both
    /// directions, so a read-only fixture open stays read-only under the
    /// fixture-sized overrides.
    #[test]
    fn read_only_passes_through_both_values() {
        for read_only in [true, false] {
            let base = lbug::SystemConfig::default().read_only(read_only);
            let returned = apply_db_overrides(base, Some("8M"), Some(16 << 20), Some(1)).unwrap();
            let debug = format!("{returned:?}");
            assert!(
                debug.contains(&format!("read_only: {read_only}")),
                "read_only must survive the overrides: {debug}"
            );
        }
    }

    /// A `max_db_size` that is not a power of two (`12M`) and one below the
    /// 8 MiB floor (`4M`) are each refused with an error naming the input —
    /// never clamped or silently dropped.
    #[test]
    fn invalid_max_db_size_is_refused() {
        let base = lbug::SystemConfig::default();
        let not_power_of_two = apply_db_overrides(base.clone(), Some("12M"), None, None)
            .expect_err("12 MiB is not a power of two");
        assert!(
            not_power_of_two.to_string().contains("not a power of two"),
            "{not_power_of_two}"
        );
        let below_floor = apply_db_overrides(base, Some("4M"), None, None)
            .expect_err("4 MiB is below the 8 MiB floor");
        assert!(
            below_floor.to_string().contains("below the 8 MiB"),
            "{below_floor}"
        );
    }

    /// `--flag=value` is one token: the first `=` splits the name from the
    /// value, and nothing lands in `positional`.
    #[test]
    fn inline_flag_value_splits_into_flag_and_value() {
        let args = ["--flag=value".to_string()];
        let parsed = parse_args(&args);
        assert_eq!(parsed.get("flag"), Some("value".to_string()));
        assert_eq!(parsed.all("flag"), vec!["value".to_string()]);
        assert!(parsed.positional.is_empty(), "{:?}", parsed.positional);
    }

    /// The two-token `--flag value` form behaves exactly as before the inline
    /// split was added.
    #[test]
    fn two_token_flag_value_form_is_unchanged() {
        let args = ["--flag".to_string(), "value".to_string()];
        let parsed = parse_args(&args);
        assert_eq!(parsed.get("flag"), Some("value".to_string()));
        assert_eq!(parsed.all("flag"), vec!["value".to_string()]);
        assert!(parsed.positional.is_empty(), "{:?}", parsed.positional);
    }

    /// A bare `--flag` with no following token is a boolean: present, with an
    /// empty value list and no `get` value.
    #[test]
    fn bare_flag_stays_boolean_with_no_values() {
        let args = ["--flag".to_string()];
        let parsed = parse_args(&args);
        assert!(parsed.has("flag"));
        assert!(parsed.all("flag").is_empty());
        assert_eq!(parsed.get("flag"), None);
        assert!(parsed.positional.is_empty(), "{:?}", parsed.positional);
    }

    /// A following token that starts with `--` is another flag, never the
    /// first flag's value.
    #[test]
    fn value_starting_with_dashes_is_not_swallowed() {
        let args = ["--flag".to_string(), "--other".to_string()];
        let parsed = parse_args(&args);
        assert!(parsed.has("flag"));
        assert!(parsed.all("flag").is_empty());
        assert!(parsed.has("other"));
        assert!(parsed.all("other").is_empty());
        assert!(parsed.positional.is_empty(), "{:?}", parsed.positional);
    }

    /// The inline split takes the first `=` only (so `a=b` survives whole), an
    /// empty value is a value, and a positional containing `=` stays
    /// positional.
    #[test]
    fn inline_value_keeps_further_equals_and_empty_value() {
        let with_equals = parse_args(&["--flag=a=b".to_string()]);
        assert_eq!(with_equals.get("flag"), Some("a=b".to_string()));

        let empty = parse_args(&["--flag=".to_string()]);
        assert_eq!(empty.get("flag"), Some(String::new()));
        assert_eq!(empty.all("flag"), vec![String::new()]);

        let positional = parse_args(&["a=b".to_string()]);
        assert_eq!(positional.positional, vec!["a=b".to_string()]);
        assert!(!positional.has("a"));
    }
}
