use super::*;
use crate::testutil::av;

mod unit {
    use super::*;

    /// The shared property-edit helper MERGEs and unsets: `{a:0,b:2}` with
    /// `--property a=1` yields `{a:1,b:2}`; adding `--unset-property b` yields
    /// `{a:1}`; omitting `--unset-property` never drops a key. `edge_update`
    /// reuses this helper.
    #[test]
    fn edit_properties_merges_and_unsets() {
        let base = BTreeMap::from([
            ("a".to_string(), "0".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        let set_only = edit_properties(&base, &parse_args(&av(&["--property", "a=1"]))).unwrap();
        assert_eq!(
            set_only,
            BTreeMap::from([
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
            ]),
            "MERGE overwrites only the passed key"
        );
        let with_unset = edit_properties(
            &base,
            &parse_args(&av(&["--property", "a=1", "--unset-property", "b"])),
        )
        .unwrap();
        assert_eq!(
            with_unset,
            BTreeMap::from([("a".to_string(), "1".to_string())]),
            "an explicit unset removes exactly the named key"
        );
        // Omitting --unset-property leaves every existing key alone.
        assert_eq!(
            edit_properties(&base, &parse_args(&av(&[]))).unwrap(),
            base,
            "no edit must not drop a key"
        );
    }

    /// An `--unset-property` naming an absent key is refused, never a silent
    /// success: the error lists the present keys and, for a `_`/`-` near-miss,
    /// names the stored spelling.
    #[test]
    fn edit_properties_refuses_unset_of_absent_key() {
        let base = BTreeMap::from([("owner-team".to_string(), "domain.group.x".to_string())]);
        let err = edit_properties(&base, &parse_args(&av(&["--unset-property", "owner_team"])))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no such property"), "{err}");
        assert!(err.contains("did you mean `owner-team`"), "{err}");

        let err = edit_properties(&base, &parse_args(&av(&["--unset-property", "zzz"])))
            .unwrap_err()
            .to_string();
        assert!(err.contains("present: [owner-team]"), "{err}");
        assert!(!err.contains("did you mean"), "{err}");
    }

    /// `node update` admission resolves existence against the supplied committed
    /// model and validates the merged candidate with the write funnel's per-node
    /// rules: a merge that breaks an `Entity`'s `kind` is refused in the builder,
    /// before anything is written, and the node's own presence in the model never
    /// trips the uniqueness rule an in-place overwrite must not.
    #[test]
    fn node_update_change_validates_the_merged_candidate_against_the_model() {
        let base = vec![NodeFile {
            layer: "domain".to_string(),
            node_type: "entity".to_string(),
            name: "order".to_string(),
            body: "The order aggregate.".to_string(),
            properties: BTreeMap::from([("kind".to_string(), "entity".to_string())]),
            out: Vec::new(),
            in_edges: Vec::new(),
        }];
        let err = node_update_change(
            &base,
            &av(&["domain", "entity", "order", "--property", "kind=bogus"]),
        )
        .err()
        .expect("an invalid merged candidate is refused")
        .to_string();
        assert!(
            err.contains("Entity `kind` `bogus` is invalid"),
            "the kind rule refuses the merged candidate: {err}"
        );

        let change = node_update_change(
            &base,
            &av(&["domain", "entity", "order", "--body", "The placed order."]),
        )
        .unwrap();
        assert_eq!(change.writes.len(), 1, "one node file is written");
        assert_eq!(change.writes[0].body, "The placed order.");
        assert_eq!(
            change.writes[0].properties.get("kind").map(String::as_str),
            Some("entity"),
            "an omitted property is preserved"
        );
        assert_eq!(change.message, "Updated node domain.entity.order");
    }
}
