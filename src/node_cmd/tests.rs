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
        let set_only = edit_properties(&base, &parse_args(&av(&["--property", "a=1"])));
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
        );
        assert_eq!(
            with_unset,
            BTreeMap::from([("a".to_string(), "1".to_string())]),
            "an explicit unset removes exactly the named key"
        );
        // Omitting --unset-property leaves every existing key alone.
        assert_eq!(
            edit_properties(&base, &parse_args(&av(&[]))),
            base,
            "no edit must not drop a key"
        );
    }
}
