use super::*;

// `read_graph_jsonl` now lives on the shared harness (`apg::testutil`) — the
// single definition the relocated e2e crate reaches directly. Re-export it
// under the old `crate::load::tests::read_graph_jsonl` path so the still-inline
// e2e tests in `src/splice.rs` / `src/ingest.rs` keep resolving until their own
// relocation tasks move them onto `crate::testutil` too.
pub(crate) use crate::testutil::read_graph_jsonl;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// The write-through merge guard sees the two authored-only pairs, while
    /// the load path's pair enumeration keeps omitting them: `build_load_files`
    /// and `copy_from` already write/COPY `calls_svc.parquet` /
    /// `uses_person.parquet` explicitly, so a `spec_rel_pairs` entry would
    /// double-write the files and emit duplicate COPY statements.
    #[test]
    fn merge_guard_sees_authored_calls_and_uses_but_the_load_path_does_not() {
        let pairs = rel_table_pairs();
        assert!(pairs.contains(&("Calls", "Service", "Service")));
        assert!(pairs.contains(&("Uses", "Person", "System")));
        assert!(
            !spec_rel_pairs().iter().any(|(t, f, to)| {
                (*t == "Calls" && *f == NodeKind::Service && *to == NodeKind::Service)
                    || (*t == "Uses" && *f == NodeKind::Person && *to == NodeKind::System)
            }),
            "spec_rel_pairs feeds build_load_files/copy_from — the authored pairs must stay out"
        );
    }

    /// `Note` is a reviewable artifact, so the `Reviews` target list admits
    /// `(Feedback, Note)` (and therefore the schema + merge guard do too), while
    /// `Note` stays out of the shared `Details` target list — `Note`→`Note`
    /// `Details` must remain refused.
    #[test]
    fn reviews_pairs_admit_note_but_details_still_excludes_it() {
        let pairs = rel_table_pairs();
        assert!(
            pairs.contains(&("Reviews", "Feedback", "Note")),
            "Reviews must declare Feedback→Note: {pairs:?}"
        );
        assert!(
            !pairs.contains(&("Details", "Note", "Note")),
            "Note→Note Details must stay refused: {pairs:?}"
        );

        // The enumeration itself: the Reviews pair is present, the Details pair
        // is not.
        let spec = spec_rel_pairs();
        assert!(
            spec.iter().any(|(t, f, to)| {
                *t == "Reviews" && *f == NodeKind::Feedback && *to == NodeKind::Note
            }),
            "spec_rel_pairs must declare the Reviews Feedback→Note pair: {spec:?}"
        );
        assert!(
            !spec.iter().any(|(t, f, to)| {
                *t == "Details" && *f == NodeKind::Note && *to == NodeKind::Note
            }),
            "spec_rel_pairs must NOT declare a Details Note→Note pair: {spec:?}"
        );
    }
}
