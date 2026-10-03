use super::*;
use crate::testutil::av;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
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
        let base = BTreeMap::from([("attaches-to".to_string(), "domain.group.x".to_string())]);
        let err = edit_properties(
            &base,
            &parse_args(&av(&["--unset-property", "attaches_to"])),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no such property"), "{err}");
        assert!(err.contains("did you mean `attaches-to`"), "{err}");

        let err = edit_properties(&base, &parse_args(&av(&["--unset-property", "zzz"])))
            .unwrap_err()
            .to_string();
        assert!(err.contains("present: [attaches-to]"), "{err}");
        assert!(!err.contains("did you mean"), "{err}");
    }
}
